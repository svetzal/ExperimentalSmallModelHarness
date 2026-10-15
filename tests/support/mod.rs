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

/// The `microgentic` binary under test. `MICROGENTIC_TEST_BINARY` points the
/// tests at another build, such as the copied release executable that
/// `scripts/smoke-installed.py` checks.
pub fn microgentic() -> &'static str {
    static BINARY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        std::env::var("MICROGENTIC_TEST_BINARY")
            .unwrap_or_else(|_| env!("CARGO_BIN_EXE_microgentic").into())
    })
}

/// The historical compatibility binary.
pub fn legacy_binary() -> &'static str {
    env!("CARGO_BIN_EXE_adaptive-agent-harness")
}
