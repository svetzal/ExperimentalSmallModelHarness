//! The `coding` domain profile: every Rust/Cargo-flavored (and, for legacy
//! shell-fence scraping, other-language) literal the runtime core used to
//! embed directly. `GENERALIZATION_PLAN.md` Slice 3 moved this module's
//! contents here **verbatim** from `agent.rs`/`tools.rs`/`contract.rs` — the
//! logic is unchanged, only its location and, where noted, its name.
//!
//! This is the only domain profile that exists today. Its unit tests are
//! allowed to reference `cargo`, `rust`, `pytest`, etc; the runtime core
//! (`agent.rs`, `tools.rs`, `contract.rs`, ...) must not.

use super::{DomainProfile, ProfileRef};
use crate::agent::{normalize_validation_command, validation_command_masks_failure};
use crate::contract::{
    AdapterKind, ArtifactClass, Budgets, DefaultsProvenance, EvidenceInvalidation,
    MutableArtifactClasses, Probe, ResolvedRunContract, SCHEMA_VERSION, Scope, Terminal,
};
use std::collections::HashMap;
use std::path::Path;

pub const CODING_PROFILE_ID: &str = "coding";
pub const CODING_PROFILE_VERSION: &str = "coding_profile.v1";

/// The coding domain profile. Stateless: every behavior is a pure function
/// of its inputs, so a unit struct is sufficient.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodingProfile;

impl DomainProfile for CodingProfile {
    fn profile_ref(&self) -> ProfileRef {
        profile_ref()
    }

    fn system_guidance(&self) -> String {
        system_prompt()
    }

    fn run_guidance(&self, goal: &str) -> String {
        run_prompt(goal)
    }

    fn post_write_validation_nudge(&self, had_final_text: bool) -> String {
        post_write_validation_nudge(had_final_text).to_string()
    }

    fn repair_ladder_suffix(&self) -> &'static str {
        REPAIR_LADDER_SUFFIX
    }

    fn recognizes_probe(&self, command: &str) -> bool {
        is_validation_probe(command)
    }

    fn command_family(&self, command: &str) -> String {
        command_family(command)
    }

    fn validation_command_family(&self, command: &str) -> String {
        validation_command_family(command)
    }

    fn path_requires_validation_after_write(&self, path: &str) -> bool {
        path_requires_validation_after_write(path)
    }

    fn is_ignored_dir(&self, dir_name: &str) -> bool {
        is_ignored_dir(dir_name)
    }

    fn is_inspection_shell_command(&self, command: &str) -> bool {
        is_inspection_shell_command(command)
    }

    fn is_known_shell_mutation_command(&self, command: &str) -> bool {
        is_known_shell_mutation_command(command)
    }

    fn failure_details(&self, stderr: &str, stdout: &str) -> Vec<String> {
        failure_details(stderr, stdout)
    }

    fn resolve_legacy_contract(&self, goal_text: &str, budgets: Budgets) -> ResolvedRunContract {
        resolve_legacy_coding_contract(goal_text, budgets)
    }

    fn default_artifact_classes(&self) -> MutableArtifactClasses {
        default_artifact_classes()
    }

    fn default_evidence_invalidation(&self) -> EvidenceInvalidation {
        default_evidence_invalidation()
    }

    fn action_intent_phrases(&self) -> &'static [&'static str] {
        ACTION_INTENT_PHRASES
    }
}

pub(crate) fn profile_ref() -> ProfileRef {
    ProfileRef {
        id: CODING_PROFILE_ID.to_string(),
        version: CODING_PROFILE_VERSION.to_string(),
    }
}

// ---------------------------------------------------------------------
// Prompt text. Moved verbatim from `agent.rs`.
// ---------------------------------------------------------------------

/// Moved from `agent.rs::system_prompt`.
pub(crate) fn system_prompt() -> String {
    [
        "You are an adaptive coding harness instance built on Mojentic.",
        "Use the provided tools to read files, write files, apply unified diffs, and run shell commands.",
        "Use list_tree to map available files before guessing repository contents.",
        "Use read_file line ranges and byte limits to keep context small.",
        "All tool paths are relative to the generated project root.",
        "Shell commands run from the generated project root by default.",
        "Work in small steps. Verify with deterministic shell commands before claiming completion.",
        "Create Cargo.toml, src/lib.rs, and other generated source at the tool root unless the task says otherwise.",
        "Create a project-appropriate .gitignore unless the task explicitly forbids additional files.",
        "Ignore generated build, dependency, cache, and virtual-environment directories such as target/, build/, dist/, node_modules/, .venv/, and __pycache__/. Do not list or inspect ignored paths unless explicitly needed.",
        "Use timeout_secs 1800 for first cargo build, cargo test, or similarly expensive validation probes.",
        "After a validation failure, repair narrowly: cite the failing command and failure text, inspect only relevant code, prefer edit_file for focused existing-source repairs, then rerun validation.",
        "For Rust projects after edits, prefer this validation ladder: cargo fmt --check, cargo clippy --all-targets --all-features -- -D warnings, focused tests, then cargo test.",
        "Use edit_file for existing source edits. Use write_file for new files, or for existing source only after reading the complete current file and preserving unrelated content.",
        "Never end a turn with an empty response. Continue using tools, or reply DONE/FAIL as instructed.",
        "When you have completed the task and verified it, answer exactly DONE.",
        "If the task cannot be completed, answer exactly FAIL with one concise reason.",
    ]
    .join("\n")
}

/// Moved from `agent.rs::run_prompt`.
pub(crate) fn run_prompt(goal: &str) -> String {
    format!(
        "Complete this benchmark task inside the generated project workspace.\n\n{goal}\n\n\
         Required harness behavior:\n\
         - You are already operating inside the generated project's workspace directory.\n\
         - Inspect the project root first.\n\
         - Create a project-appropriate .gitignore early unless this task explicitly forbids additional files.\n\
         - Build or update the Rust project at the current tool root, not in a nested workspace/ directory.\n\
         - Run at least one deterministic validation command.\n\
         - Leave generated project files at the tool root and use DONE only after validation."
    )
}

/// Moved from `agent.rs`: the two post-write validation-nudge wordings that
/// used to be inlined at their call sites (one used when the turn produced
/// no final text at all, one used when it produced final text).
const POST_WRITE_VALIDATION_NUDGE_AFTER_EMPTY_TURN: &str = "You modified files after the most recent shell probe and did not provide final text. \
     Do not edit again yet. Run the validation ladder now: cargo fmt --check, \
     then cargo clippy, then focused tests, then broad tests. Use timeout_secs 1800 \
     for cargo build or cargo test. Reply DONE only if validation passes.";

const POST_WRITE_VALIDATION_NUDGE_AFTER_FINAL_TEXT: &str = "You modified files after the most recent shell probe. Do not edit again yet. \
     Run the validation ladder now: cargo fmt --check, then cargo clippy, \
     then focused tests, then broad tests. Use timeout_secs 1800 for cargo build \
     or cargo test. Reply DONE only if validation passes.";

pub(crate) fn post_write_validation_nudge(had_final_text: bool) -> &'static str {
    if had_final_text {
        POST_WRITE_VALIDATION_NUDGE_AFTER_FINAL_TEXT
    } else {
        POST_WRITE_VALIDATION_NUDGE_AFTER_EMPTY_TURN
    }
}

/// Moved from `agent.rs::validation_repair_prompt`'s trailing clause.
pub(crate) const REPAIR_LADDER_SUFFIX: &str = "If you edit, run the validation ladder immediately afterward: cargo fmt --check, cargo clippy, focused tests, then broad tests.";

/// Moved from `agent.rs::action_intent_signal`'s action-word list: the one
/// coding-specific phrase ("run cargo") layered on top of the
/// domain-neutral verbs the runtime core still recognizes directly.
pub(crate) const ACTION_INTENT_PHRASES: &[&str] = &["run cargo"];

// ---------------------------------------------------------------------
// Validation-probe recognition and command-family normalization. Moved
// verbatim from `tools.rs` / `agent.rs`.
// ---------------------------------------------------------------------

/// Moved from `tools.rs::is_validation_probe`.
pub(crate) fn is_validation_probe(command: &str) -> bool {
    let lowered = command_detection_text(command).to_ascii_lowercase();
    let trimmed = lowered.trim();
    if trimmed.starts_with("echo ")
        || trimmed == "pwd"
        || trimmed.starts_with("pwd ")
        || trimmed.starts_with("ls ")
        || trimmed == "ls"
        || trimmed.starts_with("cat ")
        || trimmed.starts_with("sed ")
        || trimmed.starts_with("rg ")
        || trimmed.starts_with("grep ")
        || trimmed.starts_with("find ")
        || trimmed.starts_with("head ")
        || trimmed.starts_with("tail ")
    {
        return false;
    }

    [
        "cargo build",
        "cargo check",
        "cargo test",
        "cargo run",
        "cargo clippy",
        "cargo fmt",
        "npm test",
        "npm run",
        "pnpm test",
        "pnpm run",
        "yarn test",
        "yarn run",
        "pytest",
        "python -m pytest",
        "go test",
        "make test",
        "make check",
        "make build",
        "mvn test",
        "gradle test",
        "./gradlew test",
    ]
    .iter()
    .any(|needle| trimmed.contains(needle))
}

/// Moved from `tools.rs::command_family`.
pub(crate) fn command_family(command: &str) -> String {
    let lowered = command_detection_text(command).to_ascii_lowercase();
    let trimmed = lowered.trim();
    for family in [
        "cargo fmt",
        "cargo clippy",
        "cargo test",
        "cargo build",
        "cargo check",
        "cargo run",
        "npm test",
        "npm run",
        "pnpm test",
        "pnpm run",
        "yarn test",
        "yarn run",
        "python -m pytest",
        "pytest",
        "go test",
        "make test",
        "make check",
        "make build",
        "mvn test",
        "gradle test",
        "./gradlew test",
    ] {
        if trimmed.contains(family) {
            return family.to_string();
        }
    }
    trimmed
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Moved from `agent.rs::validation_command_family`.
pub(crate) fn validation_command_family(command: &str) -> String {
    let normalized = normalize_validation_command(command);
    let parts = normalized.split_whitespace().collect::<Vec<_>>();
    match parts.as_slice() {
        ["cargo", "clippy", ..] => "cargo clippy".to_string(),
        ["cargo", "fmt", ..] => "cargo fmt".to_string(),
        ["cargo", "build", ..] => "cargo build".to_string(),
        ["cargo", "test", ..] => "cargo test".to_string(),
        ["cargo", "run", ..] => "cargo run".to_string(),
        ["./cargo", "clippy", ..] => "./cargo clippy".to_string(),
        ["./cargo", "fmt", ..] => "./cargo fmt".to_string(),
        ["./cargo", "build", ..] => "./cargo build".to_string(),
        ["./cargo", "test", ..] => "./cargo test".to_string(),
        ["./cargo", "run", ..] => "./cargo run".to_string(),
        [head, second, ..] => format!("{head} {second}"),
        [head] => (*head).to_string(),
        [] => "validation".to_string(),
    }
}

/// Moved from `tools.rs::command_detection_text` (kept coding-owned since it
/// only exists to serve `is_validation_probe`/`command_family`). Strips
/// heredoc bodies from a shell command before family detection.
fn command_detection_text(command: &str) -> String {
    let mut detected = Vec::new();
    let mut heredoc_end = None;

    for line in command.lines() {
        let trimmed = line.trim();
        if let Some(end) = &heredoc_end {
            if trimmed == end {
                heredoc_end = None;
            }
            continue;
        }

        if let Some((prefix, marker)) = split_heredoc_start(line) {
            if !prefix.trim().is_empty() {
                detected.push(prefix.trim().to_string());
            }
            heredoc_end = Some(marker);
            continue;
        }

        detected.push(trimmed.to_string());
    }

    detected.join("\n")
}

fn split_heredoc_start(line: &str) -> Option<(&str, String)> {
    let (prefix, suffix) = line.split_once("<<")?;
    let marker = suffix
        .trim_start_matches('-')
        .split_whitespace()
        .next()?
        .trim_matches(|ch| ch == '\'' || ch == '"')
        .to_string();
    if marker.is_empty() {
        return None;
    }
    Some((prefix, marker))
}

// ---------------------------------------------------------------------
// Failure-detail parsing. Moved verbatim from `tools.rs`.
// ---------------------------------------------------------------------

/// Moved from `tools.rs::failure_details`.
pub(crate) fn failure_details(stderr: &str, stdout: &str) -> Vec<String> {
    let combined = [stdout, stderr]
        .into_iter()
        .filter(|source| !source.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let lines = combined.lines().map(str::trim).collect::<Vec<_>>();
    let mut details = Vec::new();

    for line in &lines {
        if line.starts_with("---- ") && line.ends_with(" stdout ----") {
            push_unique_detail(
                &mut details,
                line.trim_start_matches("---- ")
                    .trim_end_matches(" stdout ----")
                    .trim(),
            );
            continue;
        }
        if line.starts_with("thread '") && line.contains(" panicked at ") {
            push_unique_detail(&mut details, line);
            continue;
        }
        if line.starts_with("error[") || line.starts_with("error:") || line.starts_with("FAILED") {
            push_unique_detail(&mut details, line);
        }
    }

    if let Some(failures_index) = lines.iter().position(|line| *line == "failures:") {
        for line in lines.iter().skip(failures_index + 1) {
            if line.is_empty() {
                continue;
            }
            if line.starts_with("test result:") {
                break;
            }
            if !line.contains(':') && !line.starts_with('-') {
                push_unique_detail(&mut details, line);
            }
        }
    }

    details
        .into_iter()
        .take(8)
        .map(|detail| crate::tools::limit_text(&detail, 240).content)
        .collect()
}

fn push_unique_detail(details: &mut Vec<String>, detail: &str) {
    let trimmed = detail.trim();
    if !trimmed.is_empty() && !details.iter().any(|existing| existing == trimmed) {
        details.push(trimmed.to_string());
    }
}

// ---------------------------------------------------------------------
// Shell-mutation / inspection classification. Moved verbatim from
// `tools.rs` / `agent.rs`.
// ---------------------------------------------------------------------

/// Moved from `tools.rs::is_known_shell_mutation_command`.
pub(crate) fn is_known_shell_mutation_command(command: &str) -> bool {
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = normalized.to_ascii_lowercase();
    if lowered.contains("sed -i")
        || lowered.contains("perl -i")
        || has_in_place_flag(&lowered, "perl")
        || lowered.contains("ruby -i")
        || has_in_place_flag(&lowered, "ruby")
        || lowered.contains("python -i")
    {
        return true;
    }

    let redirection_mutation = [" > ", " 1> ", " >> ", " 1>> "]
        .iter()
        .any(|needle| lowered.contains(needle));
    let file_write_call = (lowered.contains("python ") || lowered.contains("python3 "))
        && (lowered.contains(".write(")
            || lowered.contains("write_text(")
            || lowered.contains("open(")
                && (lowered.contains(", 'w'") || lowered.contains(", \"w\"")));

    redirection_mutation || file_write_call
}

fn has_in_place_flag(command: &str, program: &str) -> bool {
    let tokens = command.split_whitespace().collect::<Vec<_>>();
    tokens.windows(2).any(|window| {
        window[0] == program
            && window[1].starts_with('-')
            && window[1].chars().skip(1).any(|flag| flag == 'i')
    })
}

/// Moved from `agent.rs::is_inspection_shell_command`.
pub(crate) fn is_inspection_shell_command(command: &str) -> bool {
    let trimmed = command.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed.contains("cargo ") || trimmed.contains("npm ") {
        return false;
    }
    [
        "cat ", "sed ", "head ", "tail ", "rg ", "grep ", "find ", "ls ", "wc ", "pwd",
    ]
    .iter()
    .any(|prefix| trimmed == prefix.trim() || trimmed.starts_with(prefix))
}

/// Moved from `agent.rs`/`tools.rs`: the two divergent copies of
/// `path_requires_validation_after_write` were logically identical (same
/// exemption tables, just written as an early-return chain in one file and
/// a double-`if` in the other); this is the single consolidated copy. See
/// `GENERALIZATION_PLAN.md` Slice 3 discrepancy note.
pub(crate) fn path_requires_validation_after_write(path: &str) -> bool {
    let path = Path::new(path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        file_name.as_str(),
        ".gitignore"
            | ".ignore"
            | "readme"
            | "license"
            | "licence"
            | "changelog"
            | "contributors"
            | "authors"
    ) {
        return false;
    }
    !matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "txt" | "rst" | "adoc")
    )
}

/// Moved from `tools.rs::is_shell_mutation_snapshot_excluded`'s inner
/// directory-name match.
pub(crate) fn is_ignored_dir(dir_name: &str) -> bool {
    matches!(
        dir_name,
        ".git"
            | ".hg"
            | ".jj"
            | ".svn"
            | ".venv"
            | "venv"
            | "target"
            | "node_modules"
            | ".next"
            | ".pytest_cache"
            | ".mypy_cache"
            | "coverage"
            | "dist"
    )
}

// ---------------------------------------------------------------------
// Default artifact classification / evidence invalidation. Moved from
// `contract.rs`'s `impl Default` blocks.
// ---------------------------------------------------------------------

pub(crate) fn default_artifact_classes() -> MutableArtifactClasses {
    MutableArtifactClasses {
        classes: vec![ArtifactClass {
            name: "documentation".to_string(),
            exempt_file_names: vec![
                ".gitignore".to_string(),
                ".ignore".to_string(),
                "readme".to_string(),
                "license".to_string(),
                "licence".to_string(),
                "changelog".to_string(),
                "contributors".to_string(),
                "authors".to_string(),
            ],
            exempt_extensions: vec![
                "md".to_string(),
                "markdown".to_string(),
                "txt".to_string(),
                "rst".to_string(),
                "adoc".to_string(),
            ],
        }],
    }
}

pub(crate) fn default_evidence_invalidation() -> EvidenceInvalidation {
    EvidenceInvalidation {
        invalidated_by_source_mutation: true,
        generation_gated_freshness: true,
        tracked_artifact_classes: vec!["documentation".to_string()],
    }
}

// ---------------------------------------------------------------------
// Legacy shell-fence contract adapter. Moved verbatim from `contract.rs`.
// ---------------------------------------------------------------------

// LEGACY: remove when explicit contracts replace shell-fence scraping for
// the coding profile (see GENERALIZATION_PLAN.md Slice 3, which is this
// module).
pub(crate) fn resolve_legacy_coding_contract(
    goal_text: &str,
    budgets: Budgets,
) -> ResolvedRunContract {
    let commands = requested_validation_commands(goal_text);
    let probes = synthesize_probe_ids(&commands);
    ResolvedRunContract {
        schema_version: SCHEMA_VERSION.to_string(),
        guidance: goal_text.to_string(),
        read_scope: Scope::Unrestricted,
        write_scope: Scope::Unrestricted,
        mutable_artifact_classes: default_artifact_classes(),
        evidence_invalidation: default_evidence_invalidation(),
        probes,
        budgets,
        terminal: Terminal::default(),
        adapter_kind: AdapterKind::LegacyCoding,
        profile: profile_ref(),
        defaults_provenance: DefaultsProvenance {
            declared_fields: vec!["guidance".to_string(), "probes".to_string()],
            defaulted_fields: vec![
                "read_scope".to_string(),
                "write_scope".to_string(),
                "mutable_artifact_classes".to_string(),
                "evidence_invalidation".to_string(),
                "terminal".to_string(),
                "budgets".to_string(),
            ],
        },
    }
}

// LEGACY: see `resolve_legacy_coding_contract` above. Moved verbatim from
// `contract.rs`; behavior is unchanged.
pub(crate) fn requested_validation_commands(goal: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut in_fence = false;
    let mut shell_fence = false;

    for line in goal.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
                shell_fence = false;
            } else {
                in_fence = true;
                let info = trimmed.trim_start_matches('`').trim().to_ascii_lowercase();
                shell_fence = matches!(
                    info.as_str(),
                    "" | "sh" | "shell" | "bash" | "zsh" | "console" | "text"
                );
            }
            continue;
        }

        if !in_fence || !shell_fence {
            continue;
        }
        let command = trimmed.trim_start_matches('$').trim();
        if is_requested_validation_command_line(command) {
            let normalized = normalize_validation_command(command);
            if !normalized.is_empty() && !commands.contains(&normalized) {
                commands.push(normalized);
            }
        }
    }

    commands
}

// LEGACY: see `requested_validation_commands` above.
fn is_requested_validation_command_line(command: &str) -> bool {
    if validation_command_masks_failure(command) {
        return false;
    }
    let normalized = normalize_validation_command(command);
    [
        "cargo test",
        "cargo build",
        "cargo check",
        "cargo clippy",
        "cargo fmt",
        "cargo run",
        "./cargo test",
        "./cargo build",
        "./cargo check",
        "./cargo clippy",
        "./cargo fmt",
        "./cargo run",
        "npm test",
        "npm run test",
        "pnpm test",
        "yarn test",
        "pytest",
        "python -m pytest",
        "go test",
        "mix test",
    ]
    .iter()
    .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix} ")))
}

/// Synthesize stable, descriptive probe IDs from ordered probe commands: a
/// slug of the normalized command, with a `-2`, `-3`, ... ordinal suffix on
/// collision (first occurrence keeps the bare slug). IDs are descriptive
/// only — ledger matching always stays on the normalized command string.
/// Moved verbatim from `contract.rs`.
pub(crate) fn synthesize_probe_ids(commands: &[String]) -> Vec<Probe> {
    let mut seen_counts: HashMap<String, usize> = HashMap::new();
    commands
        .iter()
        .map(|command| {
            let base = slugify(command);
            let base = if base.is_empty() {
                "probe".to_string()
            } else {
                base
            };
            let count = seen_counts.entry(base.clone()).or_insert(0);
            *count += 1;
            let id = if *count == 1 {
                base
            } else {
                format!("{base}-{count}")
            };
            Probe {
                id,
                command: command.clone(),
            }
        })
        .collect()
}

pub(crate) fn slugify(input: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal text used to reproduce `fixtures/prompts/run.txt`. Must match
    /// the placeholder baked into that fixture.
    const SAMPLE_GOAL: &str = "SAMPLE_GOAL_PLACEHOLDER";

    fn fixture(relative_path: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/prompts")
            .join(relative_path);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
    }

    #[test]
    fn system_guidance_matches_committed_fixture_byte_for_byte() {
        assert_eq!(system_prompt(), fixture("system.txt"));
    }

    #[test]
    fn run_guidance_matches_committed_fixture_byte_for_byte() {
        assert_eq!(run_prompt(SAMPLE_GOAL), fixture("run.txt"));
    }

    #[test]
    fn post_write_nudge_after_empty_turn_matches_committed_fixture() {
        assert_eq!(
            post_write_validation_nudge(false),
            fixture("post_write_nudge_after_empty_turn.txt")
        );
    }

    #[test]
    fn post_write_nudge_after_final_text_matches_committed_fixture() {
        assert_eq!(
            post_write_validation_nudge(true),
            fixture("post_write_nudge.txt")
        );
    }

    #[test]
    fn repair_ladder_suffix_matches_committed_fixture() {
        assert_eq!(REPAIR_LADDER_SUFFIX, fixture("repair.txt"));
    }

    #[test]
    fn prompt_fixtures_are_stable_across_repeated_calls() {
        // Parity/determinism: calling twice must produce identical text,
        // matching the same committed fixture both times.
        assert_eq!(system_prompt(), system_prompt());
        assert_eq!(system_prompt(), fixture("system.txt"));
        assert_eq!(run_prompt(SAMPLE_GOAL), run_prompt(SAMPLE_GOAL));
        assert_eq!(run_prompt(SAMPLE_GOAL), fixture("run.txt"));
    }

    #[test]
    fn recognizes_cargo_and_pytest_probes() {
        assert!(is_validation_probe("cargo check 2>&1 | tail -30"));
        assert!(is_validation_probe("cargo run -- --simulate 600"));
        assert!(is_validation_probe("pytest tests"));
        assert!(!is_validation_probe("echo \"test\""));
        assert!(!is_validation_probe("pwd && ls -la"));
        assert!(!is_validation_probe(
            "tee README.md << 'EOF'\n```sh\ncargo test\n```\nEOF"
        ));
    }

    #[test]
    fn normalizes_command_families() {
        assert_eq!(
            command_family("cargo clippy --all-targets -- -D warnings"),
            "cargo clippy"
        );
        assert_eq!(command_family("python -m pytest tests"), "python -m pytest");
        assert_eq!(
            command_family("tee README.md << 'EOF'\ncargo test\nEOF"),
            "tee"
        );
    }

    #[test]
    fn validation_command_family_collapses_cargo_subcommands() {
        assert_eq!(
            validation_command_family("cargo test --workspace 2>&1"),
            "cargo test"
        );
        assert_eq!(
            validation_command_family("./cargo clippy --all-targets"),
            "./cargo clippy"
        );
        assert_eq!(validation_command_family("make check"), "make check");
        assert_eq!(validation_command_family(""), "validation");
    }

    #[test]
    fn documentation_paths_are_exempt_from_validation() {
        assert!(!path_requires_validation_after_write("README.md"));
        assert!(!path_requires_validation_after_write("LICENSE"));
        assert!(!path_requires_validation_after_write("notes.txt"));
        assert!(path_requires_validation_after_write("src/lib.rs"));
        assert!(path_requires_validation_after_write("Cargo.toml"));
    }

    #[test]
    fn ignores_build_and_dependency_directories() {
        assert!(is_ignored_dir("target"));
        assert!(is_ignored_dir("node_modules"));
        assert!(is_ignored_dir(".venv"));
        assert!(!is_ignored_dir("src"));
    }

    #[test]
    fn requested_validation_parser_collects_ordered_shell_commands() {
        let task = r#"
Edit the existing Rust project.

```rust
fn cargo_test_is_not_a_command() {}
```

Run:

```sh
cargo test test_deterministic_simulation_terminates_and_reports_summary
cargo run -- --simulate 600
```
"#;

        assert_eq!(
            requested_validation_commands(task),
            vec![
                "cargo test test_deterministic_simulation_terminates_and_reports_summary",
                "cargo run -- --simulate 600",
            ]
        );
    }

    #[test]
    fn requested_validation_parser_ignores_masked_success_commands() {
        let task = r#"
Run:

```sh
cargo build || true
cargo test
cargo run -- --version-check ; true
```
"#;

        assert_eq!(
            requested_validation_commands(task),
            vec!["cargo test".to_string()]
        );
    }

    #[test]
    fn legacy_probe_ids_are_slugified_and_ordered() {
        let resolved = resolve_legacy_coding_contract(
            "```sh\ncargo fmt --check\ncargo test\n```\n",
            Budgets::default(),
        );
        assert_eq!(
            resolved.probes,
            vec![
                Probe {
                    id: "cargo-fmt-check".to_string(),
                    command: "cargo fmt --check".to_string(),
                },
                Probe {
                    id: "cargo-test".to_string(),
                    command: "cargo test".to_string(),
                },
            ]
        );
        assert_eq!(resolved.profile, profile_ref());
    }

    #[test]
    fn legacy_probe_id_collisions_get_ordinal_suffixes() {
        let resolved = resolve_legacy_coding_contract(
            "```sh\ncargo test foo-bar\ncargo test foo_bar\n```\n",
            Budgets::default(),
        );
        let ids: Vec<&str> = resolved
            .probes
            .iter()
            .map(|probe| probe.id.as_str())
            .collect();
        assert_eq!(ids, vec!["cargo-test-foo-bar", "cargo-test-foo-bar-2"]);
    }

    #[test]
    fn ruby_and_python_make_test_tasks_resolve_to_empty_probes() {
        // `make test` is not in the legacy scraping allowlist, so both
        // languages resolve identically: an empty, preserved probe list.
        let ruby = resolve_legacy_coding_contract("```sh\nmake test\n```\n", Budgets::default());
        let python = resolve_legacy_coding_contract("```sh\nmake test\n```\n", Budgets::default());
        assert_eq!(ruby.probes, Vec::new());
        assert_eq!(python.probes, Vec::new());
    }

    #[test]
    fn artifact_classification_matches_path_validation_exemptions() {
        let classes = default_artifact_classes();
        let documentation = &classes.classes[0];
        assert_eq!(documentation.name, "documentation");
        for name in &documentation.exempt_file_names {
            assert!(!path_requires_validation_after_write(name));
        }
        for extension in &documentation.exempt_extensions {
            assert!(!path_requires_validation_after_write(&format!(
                "notes.{extension}"
            )));
        }
    }

    #[test]
    fn extracts_targeted_cargo_test_failure_details() {
        let stdout = r#"
running 21 tests
test tests::test_invader_shot_removed_when_leaving_bottom_edge ... FAILED

failures:

---- tests::test_invader_shot_removed_when_leaving_bottom_edge stdout ----

thread 'tests::test_invader_shot_removed_when_leaving_bottom_edge' (31479173) panicked at src/lib.rs:739:9:
Invader shot should be removed when leaving bottom edge

failures:
    tests::test_invader_shot_removed_when_leaving_bottom_edge

test result: FAILED. 20 passed; 1 failed; finished in 0.00s
"#;

        let details = failure_details("", stdout);

        assert!(details
            .iter()
            .any(|detail| detail == "tests::test_invader_shot_removed_when_leaving_bottom_edge"));
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("src/lib.rs:739:9"))
        );
    }
}
