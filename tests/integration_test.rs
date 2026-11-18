use sigmaos_component::{Terminal, Grid, EscapeParser, TermAction};

#[test]
fn terminal_grid_round_trip() {
    // Terminal can be created, written to, and cells read back correctly.
    let mut t = Terminal::new(24, 80).unwrap();
    t.write(b"SigmaOS").unwrap();
    assert_eq!(t.grid.get(0, 0).unwrap().ch, 'S');
    assert_eq!(t.grid.get(0, 6).unwrap().ch, 'S');
}

#[test]
fn escape_parser_csi_down() {
    let mut p = EscapeParser::new();
    let mut last = None;
    for b in b"\x1b[4B" { last = p.feed(*b); }
    assert_eq!(last, Some(TermAction::CursorDown(4)));
}

#[test]
fn grid_erase_row() {
    let mut g = Grid::new(5, 10).unwrap();
    g.set(0, 0, sigmaos_component::Cell {
        ch: 'Z',
        attrs: sigmaos_component::Attrs::default(),
    }).unwrap();
    g.erase_row(0).unwrap();
    assert_eq!(g.get(0, 0).unwrap().ch, ' ');
}

#[test]
fn terminal_title_via_osc() {
    let mut t = Terminal::new(10, 40).unwrap();
    t.write(b"\x1b]0;SigmaOS VT\x07").unwrap();
    assert_eq!(t.title, "SigmaOS VT");
}
