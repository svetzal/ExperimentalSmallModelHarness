//! Canonical harness source-state provenance.
//!
//! Every run and every comparison summary should carry exactly one
//! representation of "which harness commit produced this evidence." This
//! module is that one representation: [`HarnessSourceState`] plus its single
//! producer, [`capture`].
//!
//! The behavior here is a direct, non-semantic port of the ad hoc
//! `harness_source_state()` JSON builder that used to live in `agent.rs`. It
//! is captured once per run and then reused everywhere provenance is needed
//! (the `run.started` trace event, [`crate::agent::AgentRunSummary`] /
//! `run.finished`, and [`crate::trace_analysis::TraceAnalysis`]) so there is
//! only one code path that can disagree with itself.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Harness git provenance captured at the start of a run.
///
/// For experiment consumers comparing evidence across harness versions:
///
/// - `git_head` is `Some(<40-hex commit sha>)` when the harness manifest
///   directory is a git checkout and `git rev-parse HEAD` succeeds. It is
///   `None` when git is unavailable or the manifest directory is not a git
///   checkout (a released binary or a shallow copy, for example) — this is a
///   deterministic "we don't know", not an error.
/// - `git_dirty` is `Some(true)` when `git status --short` reports any
///   pending changes, `Some(false)` when the checkout is clean, and `None`
///   under the same unavailability conditions as `git_head`. A run recorded
///   with `git_dirty: Some(true)` was produced by a working tree that does
///   not exactly match `git_head` and should be treated as provisional
///   evidence.
/// - `source_state_note` is a short, fixed human-readable string
///   ("git metadata captured" or "not a git checkout or git unavailable")
///   that disambiguates the `None` case without requiring consumers to infer
///   intent from absent fields.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct HarnessSourceState {
    /// Absolute path to the harness's own `Cargo.toml` directory at build
    /// time (`CARGO_MANIFEST_DIR`).
    pub manifest_dir: String,
    /// The harness's own git commit SHA, if determinable.
    pub git_head: Option<String>,
    /// Whether the harness working tree had uncommitted changes, if
    /// determinable.
    pub git_dirty: Option<bool>,
    /// Fixed, deterministic explanation of the two fields above.
    pub source_state_note: String,
}

/// Capture the current harness source state.
///
/// Shells out to `git` against `CARGO_MANIFEST_DIR`; any failure (git
/// missing, not a checkout, non-zero exit) deterministically yields `None`
/// fields rather than an error, since provenance is best-effort metadata and
/// must never fail a run.
pub fn capture() -> HarnessSourceState {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let git_head = std::process::Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let git_dirty = std::process::Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .arg("status")
        .arg("--short")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !String::from_utf8_lossy(&output.stdout).trim().is_empty());

    HarnessSourceState {
        manifest_dir: manifest_dir.display().to_string(),
        source_state_note: if git_head.is_some() {
            "git metadata captured".to_string()
        } else {
            "not a git checkout or git unavailable".to_string()
        },
        git_head,
        git_dirty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_a_deterministic_note_for_the_current_checkout() {
        let state = capture();
        // This repository is a git checkout, so git metadata should resolve.
        assert!(state.git_head.is_some());
        assert_eq!(state.source_state_note, "git metadata captured");
    }

    #[test]
    fn round_trips_through_json() {
        let state = HarnessSourceState {
            manifest_dir: "/tmp/repo".to_string(),
            git_head: Some("a".repeat(40)),
            git_dirty: Some(false),
            source_state_note: "git metadata captured".to_string(),
        };
        let json = serde_json::to_string(&state).unwrap();
        let round_tripped: HarnessSourceState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, round_tripped);
    }
}
