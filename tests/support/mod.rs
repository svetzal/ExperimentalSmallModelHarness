//! Shared support for end-to-end tests: a pseudo-terminal driver with a
//! rendered screen, a scripted mock provider for oMLX and Ollama, and
//! fixtures for projects, configs, state, and traces.
#![allow(dead_code, unused_imports)]

pub mod fixtures;
pub mod provider;
pub mod terminal;

pub use fixtures::*;
pub use provider::*;
pub use terminal::*;

/// The `microgentic` binary under test.
pub fn microgentic() -> &'static str {
    env!("CARGO_BIN_EXE_microgentic")
}

/// The historical compatibility binary.
pub fn legacy_binary() -> &'static str {
    env!("CARGO_BIN_EXE_adaptive-agent-harness")
}
