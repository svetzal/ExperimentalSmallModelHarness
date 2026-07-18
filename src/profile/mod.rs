//! Domain profile capability boundary (`GENERALIZATION_PLAN.md` Slice 3,
//! "Extract Coding Policy").
//!
//! The runtime core (`agent.rs`, `tools.rs`, `contract.rs`, `cli.rs`,
//! `trace.rs`, `trace_analysis.rs`, `runtime_events.rs`, `provenance.rs`,
//! `lib.rs`, `main.rs`) must contain no Rust/Cargo/Bevy/test-runner-specific
//! literals. Everything domain-specific — worker/system prompt text, run
//! guidance, validation-command recognition and family normalization,
//! artifact classification, the legacy shell-fence contract adapter, and so
//! on — lives behind the [`DomainProfile`] trait, implemented today only by
//! [`coding::CodingProfile`].

pub mod coding;

use crate::contract::{Budgets, EvidenceInvalidation, MutableArtifactClasses, ResolvedRunContract};
use serde::{Deserialize, Serialize};

pub use coding::CodingProfile;

/// Stable identity of a domain profile, carried on
/// [`crate::contract::ResolvedRunContract`] so traced/serialized contracts
/// record which profile resolved them. `#[serde(default)]` on that field
/// back-fills contracts persisted before this field existed as `"coding"`
/// (this crate's only profile at the time), via this type's `Default` impl.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRef {
    pub id: String,
    pub version: String,
}

impl Default for ProfileRef {
    fn default() -> Self {
        coding::profile_ref()
    }
}

/// The capability boundary a domain profile implements: everything the
/// runtime core needs from domain-specific policy, and nothing more. Every
/// method here existed as a hardcoded literal or duplicated function in
/// `agent.rs`/`tools.rs`/`contract.rs` before Slice 3.
pub trait DomainProfile: Send + Sync {
    /// This profile's stable identity.
    fn profile_ref(&self) -> ProfileRef;

    /// The worker/system prompt text.
    fn system_guidance(&self) -> String;

    /// Run/goal guidance appended to the task-specific goal text.
    fn run_guidance(&self, goal: &str) -> String;

    /// Post-write validation nudge text. `had_final_text` distinguishes the
    /// two current wordings: one used when the turn produced no final text
    /// at all, one used when it produced final text before the nudge fires.
    fn post_write_validation_nudge(&self, had_final_text: bool) -> String;

    /// Suffix naming the validation ladder, appended to the validation-repair
    /// prompt.
    fn repair_ladder_suffix(&self) -> &'static str;

    /// Whether the executor recognizes `command` as a validation probe.
    fn recognizes_probe(&self, command: &str) -> bool;

    /// Collapse a raw shell command into a stable "family" label (used for
    /// repeated-failure bookkeeping).
    fn command_family(&self, command: &str) -> String;

    /// Collapse a normalized validation command into a family label used in
    /// resumed-validation prompts.
    fn validation_command_family(&self, command: &str) -> String;

    /// Whether a write to `path` invalidates prior validation evidence and
    /// therefore requires re-validation before the run may terminate.
    fn path_requires_validation_after_write(&self, path: &str) -> bool;

    /// Whether `dir_name` is an ignored build/dependency/cache directory,
    /// excluded from shell-mutation fingerprinting and inspection.
    fn is_ignored_dir(&self, dir_name: &str) -> bool;

    /// Whether `command` is a read-only inspection command (cat/grep/...)
    /// exempt from action-boundary/mutation heuristics.
    fn is_inspection_shell_command(&self, command: &str) -> bool;

    /// Whether `command` is a known shell-level file-mutation command (sed
    /// -i, redirection, in-place scripting flags, ...) that should trip
    /// validation-required guards.
    fn is_known_shell_mutation_command(&self, command: &str) -> bool;

    /// Extract structured failure-detail lines from probe stdout/stderr.
    fn failure_details(&self, stderr: &str, stdout: &str) -> Vec<String>;

    /// Legacy Markdown-goal adapter: resolve a full `ResolvedRunContract`
    /// from `goal_text` via shell-fence scraping.
    fn resolve_legacy_contract(&self, goal_text: &str, budgets: Budgets) -> ResolvedRunContract;

    /// Default mutable-artifact classification for contracts that don't
    /// declare their own.
    fn default_artifact_classes(&self) -> MutableArtifactClasses;

    /// Default evidence-invalidation rules for contracts that don't declare
    /// their own.
    fn default_evidence_invalidation(&self) -> EvidenceInvalidation;

    /// Additional action-intent phrases beyond the domain-neutral verbs the
    /// runtime core already recognizes (e.g. `"run cargo"`).
    fn action_intent_phrases(&self) -> &'static [&'static str];
}

/// Select the domain profile for a run. Only the coding profile exists this
/// slice; the signature already takes no arguments so a future
/// contract-driven selector can be introduced without changing callers.
pub fn select_profile() -> &'static dyn DomainProfile {
    const PROFILE: CodingProfile = CodingProfile;
    &PROFILE
}
