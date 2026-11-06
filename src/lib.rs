// SPDX-License-Identifier: Apache-2.0
//! SigmaOS-terminal-emulator — SigmaOS component.
//!
//! Wires the component's real modules (config/error/service) into the
//! library API. Mechanism here; policy lives in configuration.

pub mod config;
pub mod error;
pub mod service;

pub use config::Config;
pub use error::{ComponentError, Result};
pub use service::Service;

/// Backwards-compatible component handle.
#[derive(Debug, Default)]
pub struct Component {
    name: &'static str,
}

impl Component {
    /// Create the component handle.
    pub fn new() -> Self {
        Self { name: "SigmaOS Component" }
    }
}
