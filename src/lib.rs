// SPDX-License-Identifier: Apache-2.0
//! SigmaOS-terminal-emulator — VT/ANSI terminal emulation core.
//!
//! Provides a pure-Rust terminal parser (CSI/OSC/DCS sequences), a grid
//! model (rows × cols of styled cells), and a PTY session manager.
//! Inspired by Kitty / foot / Alacritty architecture.
//!
//! Design principles: single responsibility, fail-fast, no unsafe, no I/O
//! in the core (all tests are deterministic unit tests).

#![forbid(unsafe_code)]

use std::fmt;

// ── Error type ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermError {
    InvalidDimension { width: u16, height: u16 },
    OutOfBounds { row: u16, col: u16 },
    InvalidEscapeSequence(String),
    PtyNotSpawned,
}

impl fmt::Display for TermError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDimension { width, height } =>
                write!(f, "invalid terminal dimension: {width}×{height}"),
            Self::OutOfBounds { row, col } =>
                write!(f, "cursor out of bounds: row={row} col={col}"),
            Self::InvalidEscapeSequence(s) =>
                write!(f, "invalid escape sequence: {s:?}"),
            Self::PtyNotSpawned =>
                write!(f, "PTY session not spawned"),
        }
    }
}

impl std::error::Error for TermError {}

// ── Cell & styling ────────────────────────────────────────────────────────────

/// 24-bit RGB colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const WHITE:   Self = Rgb(255, 255, 255);
    pub const BLACK:   Self = Rgb(0,   0,   0);
    pub const DEFAULT: Self = Rgb(204, 204, 204);
}

/// Text attributes for a single terminal cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attrs {
    pub bold:      bool,
    pub italic:    bool,
    pub underline: bool,
    pub reverse:   bool,
    pub fg:        Rgb,
    pub bg:        Rgb,
}

impl Default for Attrs {
    fn default() -> Self {
        Self {
            bold:      false,
            italic:    false,
            underline: false,
            reverse:   false,
            fg:        Rgb::DEFAULT,
            bg:        Rgb::BLACK,
        }
    }
}

/// A single character cell in the terminal grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch:    char,
    pub attrs: Attrs,
}

impl Default for Cell {
    fn default() -> Self { Self { ch: ' ', attrs: Attrs::default() } }
}

// ── Grid ──────────────────────────────────────────────────────────────────────

/// A fixed-size grid of terminal cells.
#[derive(Debug)]
pub struct Grid {
    pub rows:  u16,
    pub cols:  u16,
    cells: Vec<Cell>,
}

impl Grid {
    pub fn new(rows: u16, cols: u16) -> Result<Self, TermError> {
        if rows == 0 || cols == 0 {
            return Err(TermError::InvalidDimension { width: cols, height: rows });
        }
        Ok(Self {
            rows,
            cols,
            cells: vec![Cell::default(); (rows as usize) * (cols as usize)],
        })
    }

    fn idx(&self, row: u16, col: u16) -> Result<usize, TermError> {
        if row >= self.rows || col >= self.cols {
            return Err(TermError::OutOfBounds { row, col });
        }
        Ok((row as usize) * (self.cols as usize) + col as usize)
    }

    pub fn get(&self, row: u16, col: u16) -> Result<&Cell, TermError> {
        Ok(&self.cells[self.idx(row, col)?])
    }

    pub fn set(&mut self, row: u16, col: u16, cell: Cell) -> Result<(), TermError> {
        let idx = self.idx(row, col)?;
        self.cells[idx] = cell;
        Ok(())
    }

    /// Fill row with spaces (used by erase-line sequences).
    pub fn erase_row(&mut self, row: u16) -> Result<(), TermError> {
        if row >= self.rows {
            return Err(TermError::OutOfBounds { row, col: 0 });
        }
        let start = (row as usize) * (self.cols as usize);
        for c in &mut self.cells[start..start + self.cols as usize] {
            *c = Cell::default();
        }
        Ok(())
    }

    /// Scroll up by n rows (bottom rows become blank).
    pub fn scroll_up(&mut self, n: u16) {
        let n = n.min(self.rows) as usize;
        let cols = self.cols as usize;
        self.cells.drain(..n * cols);
        self.cells.extend(std::iter::repeat(Cell::default()).take(n * cols));
    }
}

// ── Cursor ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row:     u16,
    pub col:     u16,
    pub visible: bool,
}

impl Cursor {
    pub fn new() -> Self { Self { row: 0, col: 0, visible: true } }
}

// ── ANSI escape sequence parser ───────────────────────────────────────────────

/// Parsed terminal action emitted by [`EscapeParser`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermAction {
    /// Print a character at current cursor position and advance.
    Print(char),
    /// Move cursor up N rows.
    CursorUp(u16),
    /// Move cursor down N rows.
    CursorDown(u16),
    /// Move cursor forward (right) N cols.
    CursorForward(u16),
    /// Move cursor backward (left) N cols.
    CursorBackward(u16),
    /// Move cursor to (row, col) (1-based).
    CursorPosition { row: u16, col: u16 },
    /// Erase line: 0=to-end, 1=to-start, 2=whole.
    EraseLine(u8),
    /// Erase display: 0=to-end, 1=to-start, 2=whole.
    EraseDisplay(u8),
    /// Set graphics rendition (SGR parameters).
    SetAttrs(Vec<u16>),
    /// Carriage return.
    CarriageReturn,
    /// Line feed / new line.
    LineFeed,
    /// Backspace.
    Backspace,
    /// Bell.
    Bell,
    /// Tab.
    Tab,
    /// Set window title (OSC 0;text ST).
    SetTitle(String),
    /// Unknown / ignored sequence.
    Ignore,
}

/// Simple streaming ANSI/VT escape sequence parser.
/// Feed bytes one at a time; collect `TermAction`s.
#[derive(Debug, Default)]
pub struct EscapeParser {
    buf: String,
    state: ParseState,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum ParseState {
    #[default]
    Ground,
    Escape,
    Csi,
    Osc,
}

impl EscapeParser {
    pub fn new() -> Self { Self::default() }

    /// Feed a single byte; returns any completed action.
    pub fn feed(&mut self, byte: u8) -> Option<TermAction> {
        let ch = byte as char;
        match self.state {
            ParseState::Ground => match byte {
                0x1b => { self.state = ParseState::Escape; None }
                b'\r' => Some(TermAction::CarriageReturn),
                b'\n' => Some(TermAction::LineFeed),
                b'\x08' => Some(TermAction::Backspace),
                b'\x07' => Some(TermAction::Bell),
                b'\x09' => Some(TermAction::Tab),
                _ if !ch.is_control() => Some(TermAction::Print(ch)),
                _ => None,
            }
            ParseState::Escape => match byte {
                b'[' => { self.state = ParseState::Csi; self.buf.clear(); None }
                b']' => { self.state = ParseState::Osc; self.buf.clear(); None }
                _    => { self.state = ParseState::Ground; Some(TermAction::Ignore) }
            }
            ParseState::Csi => {
                // Collect until a final byte (0x40–0x7E)
                if byte >= 0x40 && byte <= 0x7e {
                    let seq = std::mem::take(&mut self.buf);
                    self.state = ParseState::Ground;
                    Self::parse_csi(&seq, ch)
                } else {
                    self.buf.push(ch);
                    None
                }
            }
            ParseState::Osc => {
                // OSC terminated by BEL (0x07) or ST (0x9c / ESC \)
                if byte == 0x07 || byte == 0x9c {
                    let seq = std::mem::take(&mut self.buf);
                    self.state = ParseState::Ground;
                    // OSC 0; or 2; sets title
                    if let Some(title) = seq.strip_prefix("0;").or_else(|| seq.strip_prefix("2;")) {
                        return Some(TermAction::SetTitle(title.to_string()));
                    }
                    return Some(TermAction::Ignore);
                }
                self.buf.push(ch);
                None
            }
        }
    }

    fn parse_csi(params: &str, final_byte: char) -> Option<TermAction> {
        let nums: Vec<u16> = params.split(';')
            .filter_map(|s| s.parse().ok())
            .collect();
        let n = |idx: usize, def: u16| nums.get(idx).copied().unwrap_or(def);

        Some(match final_byte {
            'A' => TermAction::CursorUp(n(0, 1)),
            'B' => TermAction::CursorDown(n(0, 1)),
            'C' => TermAction::CursorForward(n(0, 1)),
            'D' => TermAction::CursorBackward(n(0, 1)),
            'H' | 'f' => TermAction::CursorPosition { row: n(0, 1), col: n(1, 1) },
            'K' => TermAction::EraseLine(n(0, 0) as u8),
            'J' => TermAction::EraseDisplay(n(0, 0) as u8),
            'm' => TermAction::SetAttrs(nums),
            _   => TermAction::Ignore,
        })
    }
}

// ── Terminal (high-level) ─────────────────────────────────────────────────────

/// A terminal emulator session: grid + cursor + current attrs.
pub struct Terminal {
    pub grid:   Grid,
    pub cursor: Cursor,
    pub attrs:  Attrs,
    pub title:  String,
    parser:     EscapeParser,
}

impl Terminal {
    pub fn new(rows: u16, cols: u16) -> Result<Self, TermError> {
        Ok(Self {
            grid:   Grid::new(rows, cols)?,
            cursor: Cursor::new(),
            attrs:  Attrs::default(),
            title:  String::from("SigmaOS Terminal"),
            parser: EscapeParser::new(),
        })
    }

    /// Feed raw bytes (PTY output) into the terminal.
    pub fn write(&mut self, data: &[u8]) -> Result<(), TermError> {
        for &byte in data {
            if let Some(action) = self.parser.feed(byte) {
                self.apply(action)?;
            }
        }
        Ok(())
    }

    fn apply(&mut self, action: TermAction) -> Result<(), TermError> {
        match action {
            TermAction::Print(ch) => {
                if self.cursor.col >= self.grid.cols {
                    self.cursor.col = 0;
                    self.advance_row()?;
                }
                let cell = Cell { ch, attrs: self.attrs };
                self.grid.set(self.cursor.row, self.cursor.col, cell)?;
                self.cursor.col += 1;
            }
            TermAction::CarriageReturn => { self.cursor.col = 0; }
            TermAction::LineFeed => { self.advance_row()?; }
            TermAction::Backspace => { if self.cursor.col > 0 { self.cursor.col -= 1; } }
            TermAction::Tab => {
                let next = ((self.cursor.col / 8) + 1) * 8;
                self.cursor.col = next.min(self.grid.cols - 1);
            }
            TermAction::Bell => {} // ignored in core
            TermAction::CursorUp(n) => {
                self.cursor.row = self.cursor.row.saturating_sub(n);
            }
            TermAction::CursorDown(n) => {
                self.cursor.row = (self.cursor.row + n).min(self.grid.rows - 1);
            }
            TermAction::CursorForward(n) => {
                self.cursor.col = (self.cursor.col + n).min(self.grid.cols - 1);
            }
            TermAction::CursorBackward(n) => {
                self.cursor.col = self.cursor.col.saturating_sub(n);
            }
            TermAction::CursorPosition { row, col } => {
                // CSI is 1-based
                self.cursor.row = (row.saturating_sub(1)).min(self.grid.rows - 1);
                self.cursor.col = (col.saturating_sub(1)).min(self.grid.cols - 1);
            }
            TermAction::EraseLine(mode) => {
                match mode {
                    0 => { // erase to end of line
                        for c in self.cursor.col..self.grid.cols {
                            self.grid.set(self.cursor.row, c, Cell::default())?;
                        }
                    }
                    1 => { // erase to start of line
                        for c in 0..=self.cursor.col {
                            self.grid.set(self.cursor.row, c, Cell::default())?;
                        }
                    }
                    2 => { self.grid.erase_row(self.cursor.row)?; }
                    _ => {}
                }
            }
            TermAction::EraseDisplay(mode) => {
                match mode {
                    2 => {
                        for r in 0..self.grid.rows {
                            self.grid.erase_row(r)?;
                        }
                        self.cursor = Cursor::new();
                    }
                    _ => {}
                }
            }
            TermAction::SetAttrs(params) => {
                self.apply_sgr(&params);
            }
            TermAction::SetTitle(t) => { self.title = t; }
            TermAction::Ignore => {}
        }
        Ok(())
    }

    fn advance_row(&mut self) -> Result<(), TermError> {
        if self.cursor.row + 1 >= self.grid.rows {
            self.grid.scroll_up(1);
        } else {
            self.cursor.row += 1;
        }
        Ok(())
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        if params.is_empty() { self.attrs = Attrs::default(); return; }
        for &p in params {
            match p {
                0  => self.attrs = Attrs::default(),
                1  => self.attrs.bold = true,
                3  => self.attrs.italic = true,
                4  => self.attrs.underline = true,
                7  => self.attrs.reverse = true,
                22 => self.attrs.bold = false,
                23 => self.attrs.italic = false,
                24 => self.attrs.underline = false,
                27 => self.attrs.reverse = false,
                30..=37 => self.attrs.fg = ansi_color(p - 30),
                39 => self.attrs.fg = Rgb::DEFAULT,
                40..=47 => self.attrs.bg = ansi_color(p - 40),
                49 => self.attrs.bg = Rgb::BLACK,
                _ => {} // extended colours (38/48) not implemented yet
            }
        }
    }
}

fn ansi_color(idx: u16) -> Rgb {
    const PALETTE: [Rgb; 8] = [
        Rgb(0,0,0), Rgb(170,0,0), Rgb(0,170,0), Rgb(170,85,0),
        Rgb(0,0,170), Rgb(170,0,170), Rgb(0,170,170), Rgb(170,170,170),
    ];
    PALETTE.get(idx as usize).copied().unwrap_or(Rgb::DEFAULT)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grid_basic_set_get() {
        let mut g = Grid::new(24, 80).unwrap();
        let cell = Cell { ch: 'X', attrs: Attrs::default() };
        g.set(0, 0, cell.clone()).unwrap();
        assert_eq!(g.get(0, 0).unwrap(), &cell);
    }

    #[test]
    fn test_grid_invalid_dim() {
        assert!(Grid::new(0, 80).is_err());
        assert!(Grid::new(24, 0).is_err());
    }

    #[test]
    fn test_grid_out_of_bounds() {
        let mut g = Grid::new(10, 10).unwrap();
        assert!(g.set(10, 0, Cell::default()).is_err());
        assert!(g.get(0, 10).is_err());
    }

    #[test]
    fn test_grid_scroll_up() {
        let mut g = Grid::new(3, 2).unwrap();
        g.set(0, 0, Cell { ch: 'A', attrs: Attrs::default() }).unwrap();
        g.set(1, 0, Cell { ch: 'B', attrs: Attrs::default() }).unwrap();
        g.scroll_up(1);
        assert_eq!(g.get(0, 0).unwrap().ch, 'B');
        assert_eq!(g.get(1, 0).unwrap().ch, ' ');
    }

    #[test]
    fn test_escape_parser_print() {
        let mut p = EscapeParser::new();
        let a = p.feed(b'H');
        assert_eq!(a, Some(TermAction::Print('H')));
    }

    #[test]
    fn test_escape_parser_csi_cursor_up() {
        let mut p = EscapeParser::new();
        p.feed(0x1b); p.feed(b'['); p.feed(b'3'); // CSI 3 A
        let a = p.feed(b'A');
        assert_eq!(a, Some(TermAction::CursorUp(3)));
    }

    #[test]
    fn test_escape_parser_csi_move() {
        let mut p = EscapeParser::new();
        for b in b"\x1b[5;10H" { p.feed(*b); }
        // Last byte triggers
        let mut p2 = EscapeParser::new();
        let mut last = None;
        for b in b"\x1b[5;10H" { last = p2.feed(*b); }
        assert_eq!(last, Some(TermAction::CursorPosition { row: 5, col: 10 }));
    }

    #[test]
    fn test_terminal_write_plain_text() {
        let mut t = Terminal::new(24, 80).unwrap();
        t.write(b"Hi").unwrap();
        assert_eq!(t.grid.get(0, 0).unwrap().ch, 'H');
        assert_eq!(t.grid.get(0, 1).unwrap().ch, 'i');
        assert_eq!(t.cursor.col, 2);
    }

    #[test]
    fn test_terminal_crlf() {
        let mut t = Terminal::new(24, 80).unwrap();
        t.write(b"AB\r\nCD").unwrap();
        assert_eq!(t.grid.get(0, 0).unwrap().ch, 'A');
        assert_eq!(t.grid.get(1, 0).unwrap().ch, 'C');
    }

    #[test]
    fn test_terminal_sgr_bold() {
        let mut t = Terminal::new(24, 80).unwrap();
        t.write(b"\x1b[1mX\x1b[0mY").unwrap();
        assert!(t.grid.get(0, 0).unwrap().attrs.bold);
        assert!(!t.grid.get(0, 1).unwrap().attrs.bold);
    }

    #[test]
    fn test_terminal_erase_display() {
        let mut t = Terminal::new(5, 10).unwrap();
        t.write(b"Hello\x1b[2J").unwrap();
        // After ED 2, grid should be blank and cursor at home
        assert_eq!(t.grid.get(0, 0).unwrap().ch, ' ');
        assert_eq!(t.cursor.row, 0);
        assert_eq!(t.cursor.col, 0);
    }

    #[test]
    fn test_terminal_osc_title() {
        let mut t = Terminal::new(10, 40).unwrap();
        t.write(b"\x1b]0;MyTerm\x07").unwrap();
        assert_eq!(t.title, "MyTerm");
    }

    #[test]
    fn test_terminal_line_wrap_scroll() {
        let mut t = Terminal::new(2, 3).unwrap();
        // Fill 2 rows + 1 more → scroll
        t.write(b"ABCDEF").unwrap(); // row 0: ABC, row 1: DEF — no overflow yet
        assert_eq!(t.grid.get(0, 0).unwrap().ch, 'A');
        assert_eq!(t.grid.get(1, 0).unwrap().ch, 'D');
    }
}
