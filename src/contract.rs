//! A typed run contract: one resolved representation of guidance, scope,
//! artifact classification, evidence-invalidation rules, validation probes,
//! budgets, and terminal protocol, replacing ad-hoc shell-fence scraping with
//! an explicit, snapshot-testable data structure.
//!
//! `GENERALIZATION_PLAN.md` (Slice 2, "Introduce A Typed Run Contract")
//! requires that existing demo tasks keep resolving to contracts equivalent
//! to today's behavior. This module is representation-and-adapters only: it
//! does not change prompts, budgets, tool availability, validation/repair
//! rules, terminal-token detection, or artifact-classification semantics.
//! Two adapters populate a [`ResolvedRunContract`]:
//!
//! - The **legacy coding adapter**
//!   ([`crate::profile::coding::resolve_legacy_coding_contract`], moved
//!   there in Slice 3 — see `GENERALIZATION_PLAN.md`) wraps today's
//!   shell-fence scraping so existing `task.md` files keep working
//!   unchanged.
//! - The **explicit adapter** ([`resolve_explicit_contract`]) parses a
//!   `SuppliedExplicitContract` JSON document, validates it fully before any
//!   effect, and resolves it using declared probe IDs/commands instead of
//!   scraping.
//!
//! Both adapters produce the same [`ResolvedRunContract`] shape, so the
//! runtime (and `analyze-trace`) can reason about one representation
//! regardless of which adapter supplied it. All domain-specific policy
//! (worker prompt text, validation-command recognition tables, artifact
//! classification defaults) lives behind `crate::profile::DomainProfile`;
//! this module stays generic.

use crate::agent::validation_command_masks_failure;
use crate::profile::ProfileRef;
use crate::profile::coding::is_validation_probe;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

/// Schema marker embedded in every resolved contract. Bump this (e.g.
/// `"run_contract.v2"`) if the resolved shape changes in a way that breaks
/// existing consumers.
pub const SCHEMA_VERSION: &str = "run_contract.v1";

/// Which adapter produced a [`ResolvedRunContract`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    /// LEGACY: remove when explicit contracts replace shell-fence scraping
    /// for the coding profile (see `GENERALIZATION_PLAN.md` Slice 3).
    LegacyCoding,
    Explicit,
}

/// A filesystem access scope. `Unrestricted` is today's actual runtime
/// behavior (`ToolScope::new`, not `ToolScope::new_restricted`); `Rules`
/// describes a set of root-relative path prefixes. This slice keeps the
/// control loop unrestricted — the scope field is descriptive data only,
/// not yet consulted by `ToolScope` construction.
// Adjacently tagged (not internally tagged): serde cannot serialize an
// internally-tagged newtype variant that wraps a sequence
// (`cannot serialize tagged newtype variant Scope::Rules containing a
// sequence`), which broke `resolve-contract` for every explicit contract
// declaring a scope. Adjacent tagging puts the payload in a separate
// `rules` field instead of inline, which serde_json supports for
// newtype-of-sequence variants. Unit variants (`Unrestricted`) are
// unaffected: adjacent tagging omits the content field when there is no
// content, so `{"kind":"unrestricted"}` is unchanged and existing legacy
// snapshots stay byte-identical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "rules", rename_all = "snake_case")]
pub enum Scope {
    Unrestricted,
    Rules(Vec<String>),
}

/// One validation probe: a stable descriptive ID and the command the
/// executor actually runs. Matching for the requested-validation ledger
/// stays on the normalized command string; `id` is descriptive only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    pub id: String,
    pub command: String,
}

/// One named class of writable artifact, with the exemption lists that
/// determine whether a write to it requires validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactClass {
    pub name: String,
    pub exempt_file_names: Vec<String>,
    pub exempt_extensions: Vec<String>,
}

/// A named class of writable artifact and the exemption lists that
/// determine whether a write to it requires validation. Generic shape only
/// — the actual coding-profile contents live in
/// `crate::profile::coding::default_artifact_classes`
/// (`GENERALIZATION_PLAN.md` Slice 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutableArtifactClasses {
    pub classes: Vec<ArtifactClass>,
}

/// The requested-validation ledger's evidence freshness rules
/// (`note_source_mutation` plus generation-gated freshness). Generic shape
/// only — the actual coding-profile contents live in
/// `crate::profile::coding::default_evidence_invalidation`
/// (`GENERALIZATION_PLAN.md` Slice 3). Does not invent new unhonored
/// policy: this slice does not make the runtime consult these fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceInvalidation {
    pub invalidated_by_source_mutation: bool,
    pub generation_gated_freshness: bool,
    /// Names of [`MutableArtifactClasses::classes`] whose mutation
    /// invalidates prior validation evidence. Every name here must exist in
    /// the sibling `mutable_artifact_classes`.
    pub tracked_artifact_classes: Vec<String>,
}

/// Subset of `AgentRunConfig` fields the runtime already uses, carried as
/// descriptive data rather than new policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    pub max_iterations: usize,
    pub max_tool_iterations: usize,
    pub context_window_tokens: Option<usize>,
    pub max_thinking_only_tokens: usize,
    pub repair_exit_thinking_tokens: usize,
}

impl Default for Budgets {
    /// Matches the CLI `run` subcommand's own defaults so contracts
    /// resolved outside a live run (e.g. `resolve-contract`) describe the
    /// same numbers a fresh `run` invocation would use.
    fn default() -> Self {
        Self {
            max_iterations: 10,
            max_tool_iterations: 50,
            context_window_tokens: None,
            max_thinking_only_tokens: 4_096,
            repair_exit_thinking_tokens: 16_384,
        }
    }
}

/// Literal terminal-response tokens. Descriptive metadata only: the actual
/// detection in `agent.rs` (`is_done_response`/`is_fail_response`) stays a
/// literal, unparameterized string check this slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    pub done_token: String,
    pub fail_token: String,
}

impl Default for Terminal {
    fn default() -> Self {
        Self {
            done_token: "DONE".to_string(),
            fail_token: "FAIL".to_string(),
        }
    }
}

/// Which `ResolvedRunContract` fields were explicitly declared by the
/// supplied contract versus filled in by adapter defaults. Path-free: names
/// fields, not file locations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefaultsProvenance {
    pub declared_fields: Vec<String>,
    pub defaulted_fields: Vec<String>,
}

/// One resolved run contract: the single representation both adapters
/// produce, and the shape traced by `agent.contract.resolved` /
/// `resolve-contract`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedRunContract {
    pub schema_version: String,
    pub guidance: String,
    pub read_scope: Scope,
    pub write_scope: Scope,
    pub mutable_artifact_classes: MutableArtifactClasses,
    pub evidence_invalidation: EvidenceInvalidation,
    pub probes: Vec<Probe>,
    pub budgets: Budgets,
    pub terminal: Terminal,
    pub adapter_kind: AdapterKind,
    /// Which domain profile resolved this contract. Additive Slice-3 schema
    /// change (`GENERALIZATION_PLAN.md`): `#[serde(default)]` back-fills
    /// contracts persisted before this field existed as `"coding"` (this
    /// crate's only profile at the time) via `ProfileRef::default`, so
    /// `SCHEMA_VERSION` stays `run_contract.v1` rather than bumping to v2.
    #[serde(default)]
    pub profile: ProfileRef,
    pub defaults_provenance: DefaultsProvenance,
}

/// What was supplied to `resolve_contract`, traced separately from the
/// resolved result via `agent.contract.supplied`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuppliedContract {
    Legacy { goal_path: String },
    Explicit { source_path: Option<String> },
}

/// Input to [`resolve_contract`]: either a legacy Markdown goal file's text,
/// or an explicit contract's JSON text.
#[derive(Debug, Clone)]
pub enum ContractSource {
    Legacy {
        goal_path: String,
        goal_text: String,
    },
    Explicit {
        source_path: Option<String>,
        json_text: String,
    },
}

/// The JSON document an explicit contract author supplies. Optional fields
/// fall back to the same defaults the legacy adapter uses for descriptive
/// (not-yet-enforced) data; `guidance` and `probes` are always required.
#[derive(Debug, Clone, Deserialize)]
pub struct SuppliedExplicitContract {
    pub guidance: String,
    #[serde(default)]
    pub read_scope: Option<Vec<String>>,
    #[serde(default)]
    pub write_scope: Option<Vec<String>>,
    #[serde(default)]
    pub mutable_artifact_classes: Option<MutableArtifactClasses>,
    #[serde(default)]
    pub evidence_invalidation: Option<EvidenceInvalidation>,
    pub probes: Vec<Probe>,
    #[serde(default)]
    pub budgets: Option<Budgets>,
    #[serde(default)]
    pub terminal: Option<Terminal>,
}

/// Describe what was supplied, without resolving it. Cheap and infallible —
/// used to trace `agent.contract.supplied` alongside the (fallible)
/// resolution.
pub fn supplied_contract_for(source: &ContractSource) -> SuppliedContract {
    match source {
        ContractSource::Legacy { goal_path, .. } => SuppliedContract::Legacy {
            goal_path: goal_path.clone(),
        },
        ContractSource::Explicit { source_path, .. } => SuppliedContract::Explicit {
            source_path: source_path.clone(),
        },
    }
}

/// Resolve a [`ContractSource`] into one [`ResolvedRunContract`], performing
/// all validation before any LLM/tool effect. `budgets` carries the caller's
/// already-resolved budget numbers (from a live `AgentRunConfig`, or
/// [`Budgets::default`] for standalone resolution such as the
/// `resolve-contract` CLI command).
pub fn resolve_contract(source: ContractSource, budgets: Budgets) -> Result<ResolvedRunContract> {
    match source {
        ContractSource::Legacy { goal_text, .. } => {
            Ok(crate::profile::select_profile().resolve_legacy_contract(&goal_text, budgets))
        }
        ContractSource::Explicit { json_text, .. } => {
            resolve_explicit_contract(&json_text, budgets)
        }
    }
}

fn resolve_explicit_contract(
    json_text: &str,
    default_budgets: Budgets,
) -> Result<ResolvedRunContract> {
    let supplied: SuppliedExplicitContract =
        serde_json::from_str(json_text).context("parsing explicit run contract JSON")?;
    validate_explicit_contract(&supplied)?;

    let mut declared = vec!["guidance".to_string(), "probes".to_string()];
    let mut defaulted = Vec::new();

    let read_scope = describe_scope(
        &supplied.read_scope,
        "read_scope",
        &mut declared,
        &mut defaulted,
    );
    let write_scope = describe_scope(
        &supplied.write_scope,
        "write_scope",
        &mut declared,
        &mut defaulted,
    );

    let mutable_artifact_classes = match &supplied.mutable_artifact_classes {
        Some(classes) => {
            declared.push("mutable_artifact_classes".to_string());
            classes.clone()
        }
        None => {
            defaulted.push("mutable_artifact_classes".to_string());
            crate::profile::select_profile().default_artifact_classes()
        }
    };

    let evidence_invalidation = match &supplied.evidence_invalidation {
        Some(invalidation) => {
            declared.push("evidence_invalidation".to_string());
            invalidation.clone()
        }
        None => {
            defaulted.push("evidence_invalidation".to_string());
            crate::profile::select_profile().default_evidence_invalidation()
        }
    };
    validate_artifact_invalidation_consistency(&mutable_artifact_classes, &evidence_invalidation)?;

    let budgets = match &supplied.budgets {
        Some(budgets) => {
            declared.push("budgets".to_string());
            *budgets
        }
        None => {
            defaulted.push("budgets".to_string());
            default_budgets
        }
    };

    let terminal = match &supplied.terminal {
        Some(terminal) => {
            declared.push("terminal".to_string());
            terminal.clone()
        }
        None => {
            defaulted.push("terminal".to_string());
            Terminal::default()
        }
    };

    Ok(ResolvedRunContract {
        schema_version: SCHEMA_VERSION.to_string(),
        guidance: supplied.guidance.clone(),
        read_scope,
        write_scope,
        mutable_artifact_classes,
        evidence_invalidation,
        probes: supplied.probes.clone(),
        budgets,
        terminal,
        adapter_kind: AdapterKind::Explicit,
        profile: crate::profile::select_profile().profile_ref(),
        defaults_provenance: DefaultsProvenance {
            declared_fields: declared,
            defaulted_fields: defaulted,
        },
    })
}

fn describe_scope(
    rules: &Option<Vec<String>>,
    field_name: &str,
    declared: &mut Vec<String>,
    defaulted: &mut Vec<String>,
) -> Scope {
    match rules {
        Some(rules) => {
            declared.push(field_name.to_string());
            Scope::Rules(rules.clone())
        }
        None => {
            defaulted.push(field_name.to_string());
            Scope::Unrestricted
        }
    }
}

/// All explicit-contract validation, performed before any LLM/tool effect.
/// Each failure mode returns an actionable, specific error.
fn validate_explicit_contract(contract: &SuppliedExplicitContract) -> Result<()> {
    let mut seen_ids = HashSet::new();
    for probe in &contract.probes {
        if probe.id.trim().is_empty() {
            bail!(
                "explicit contract: probe id must not be empty (command {:?})",
                probe.command
            );
        }
        if !seen_ids.insert(probe.id.clone()) {
            bail!("explicit contract: duplicate probe id {:?}", probe.id);
        }
        if validation_command_masks_failure(&probe.command) {
            bail!(
                "explicit contract: probe {:?} command {:?} masks failure (e.g. `|| true`, `; true`, `|| exit 0`)",
                probe.id,
                probe.command
            );
        }
        if !is_validation_probe(&probe.command) {
            bail!(
                "explicit contract: probe {:?} command {:?} will never be recognized as a validation probe by the executor",
                probe.id,
                probe.command
            );
        }
    }

    for (field_name, scope) in [
        ("read_scope", &contract.read_scope),
        ("write_scope", &contract.write_scope),
    ] {
        if let Some(rules) = scope {
            for rule in rules {
                if rule.contains("..") || Path::new(rule).is_absolute() {
                    bail!(
                        "explicit contract: {field_name} rule {:?} escapes the workspace root",
                        rule
                    );
                }
            }
        }
    }

    if let Some(terminal) = &contract.terminal {
        if terminal.done_token.trim().is_empty() {
            bail!("explicit contract: terminal.done_token must not be empty");
        }
        if terminal.fail_token.trim().is_empty() {
            bail!("explicit contract: terminal.fail_token must not be empty");
        }
        if terminal.done_token == terminal.fail_token {
            bail!("explicit contract: terminal.done_token and terminal.fail_token must differ");
        }
    }

    if let Some(budgets) = &contract.budgets {
        if budgets.max_iterations == 0 {
            bail!("explicit contract: budgets.max_iterations must be greater than zero");
        }
        if budgets.max_tool_iterations == 0 {
            bail!("explicit contract: budgets.max_tool_iterations must be greater than zero");
        }
    }

    Ok(())
}

fn validate_artifact_invalidation_consistency(
    classes: &MutableArtifactClasses,
    invalidation: &EvidenceInvalidation,
) -> Result<()> {
    let names: HashSet<&str> = classes
        .classes
        .iter()
        .map(|class| class.name.as_str())
        .collect();
    for tracked in &invalidation.tracked_artifact_classes {
        if !names.contains(tracked.as_str()) {
            bail!(
                "explicit contract: evidence_invalidation references unknown artifact class {:?}",
                tracked
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy(goal_text: &str) -> ResolvedRunContract {
        resolve_contract(
            ContractSource::Legacy {
                goal_path: "task.md".to_string(),
                goal_text: goal_text.to_string(),
            },
            Budgets::default(),
        )
        .expect("legacy resolution never fails")
    }

    fn explicit(json_text: &str) -> Result<ResolvedRunContract> {
        resolve_contract(
            ContractSource::Explicit {
                source_path: Some("contract.json".to_string()),
                json_text: json_text.to_string(),
            },
            Budgets::default(),
        )
    }

    // The shell-fence scraper itself (`requested_validation_commands`) moved
    // to `crate::profile::coding` in Slice 3, along with its unit tests; the
    // tests below stay here because they exercise `resolve_contract`, the
    // generic entry point.

    #[test]
    fn schema_marker_present_and_round_trips() {
        let resolved = legacy("```sh\ncargo test\n```\n");
        assert_eq!(resolved.schema_version, SCHEMA_VERSION);
        let json = serde_json::to_string(&resolved).unwrap();
        let parsed: ResolvedRunContract = serde_json::from_str(&json).unwrap();
        let json_again = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, json_again);
        assert_eq!(parsed, resolved);
    }

    #[test]
    fn scope_rules_variant_round_trips_through_serde_json() {
        // Regression test: an internally-tagged newtype variant wrapping a
        // sequence (the old `#[serde(tag = "kind")]` representation) cannot
        // be serialized by serde_json at all, which broke `resolve-contract`
        // for every explicit contract that declared a scope. Adjacent
        // tagging (`tag = "kind", content = "rules"`) fixes this; assert it
        // round-trips and produces the expected shape.
        let scope = Scope::Rules(vec!["src".to_string(), "tests".to_string()]);
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(json, r#"{"kind":"rules","rules":["src","tests"]}"#);
        let parsed: Scope = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, scope);
    }

    #[test]
    fn scope_unrestricted_variant_serializes_without_content_field() {
        // Unit variants under adjacent tagging omit the `content` field
        // entirely, so switching to adjacent tagging must not change the
        // wire shape of `Unrestricted` — legacy snapshots depend on this
        // staying `{"kind":"unrestricted"}`.
        let json = serde_json::to_string(&Scope::Unrestricted).unwrap();
        assert_eq!(json, r#"{"kind":"unrestricted"}"#);
    }

    #[test]
    fn explicit_contract_with_scope_serializes_via_resolve_contract() {
        // Regression test for the resolve-contract CLI failure: resolving an
        // explicit contract that declares read/write scope must produce a
        // ResolvedRunContract that serde_json can actually serialize.
        let json = std::fs::read_to_string("fixtures/contracts/explicit/rust-cargo-task.json")
            .expect("fixture present");
        let resolved = resolve_explicit_contract(&json, Budgets::default()).unwrap();
        let serialized = serde_json::to_string(&resolved);
        assert!(
            serialized.is_ok(),
            "expected resolved explicit contract to serialize, got {:?}",
            serialized.err()
        );
    }

    #[test]
    fn resolving_the_same_legacy_goal_twice_is_deterministic() {
        let goal = "```sh\ncargo fmt --check\ncargo test\n```\n";
        assert_eq!(legacy(goal), legacy(goal));
    }

    #[test]
    fn legacy_probe_ids_are_slugified_and_ordered() {
        let resolved = legacy("```sh\ncargo fmt --check\ncargo test\n```\n");
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
    }

    #[test]
    fn legacy_probe_id_collisions_get_ordinal_suffixes() {
        // Two distinct normalized commands (differ only by `-` vs `_`) that
        // slugify identically.
        let resolved = legacy("```sh\ncargo test foo-bar\ncargo test foo_bar\n```\n");
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
        let ruby = legacy("```sh\nmake test\n```\n");
        let python = legacy("```sh\nmake test\n```\n");
        assert_eq!(ruby.probes, Vec::new());
        assert_eq!(python.probes, Vec::new());
    }

    #[test]
    fn explicit_ruby_contract_can_declare_make_test_as_a_recognized_probe() {
        let json = r#"{
            "guidance": "Fix the Ruby INI parser.",
            "probes": [{"id": "make-test", "command": "make test"}]
        }"#;
        let resolved = explicit(json).expect("make test is a recognized probe");
        assert_eq!(
            resolved.probes,
            vec![Probe {
                id: "make-test".to_string(),
                command: "make test".to_string(),
            }]
        );
        assert!(!resolved.probes.is_empty());
    }

    #[test]
    fn explicit_and_legacy_agree_on_semantic_core_for_a_cargo_task() {
        let goal = "```sh\ncargo test\n```\n";
        let legacy_resolved = legacy(goal);
        let json = r#"{
            "guidance": "```sh\ncargo test\n```\n",
            "probes": [{"id": "cargo-test", "command": "cargo test"}]
        }"#;
        let explicit_resolved = explicit(json).unwrap();
        assert_eq!(explicit_resolved.guidance, legacy_resolved.guidance);
        assert_eq!(
            explicit_resolved
                .probes
                .iter()
                .map(|probe| probe.command.clone())
                .collect::<Vec<_>>(),
            legacy_resolved
                .probes
                .iter()
                .map(|probe| probe.command.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(explicit_resolved.read_scope, legacy_resolved.read_scope);
        assert_eq!(explicit_resolved.write_scope, legacy_resolved.write_scope);
    }

    #[test]
    fn duplicate_probe_id_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [
                {"id": "a", "command": "cargo test"},
                {"id": "a", "command": "cargo build"}
            ]
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(error.contains("duplicate probe id"), "{error}");
    }

    #[test]
    fn empty_probe_id_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [{"id": "", "command": "cargo test"}]
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(error.contains("probe id must not be empty"), "{error}");
    }

    #[test]
    fn masks_failure_command_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [{"id": "a", "command": "cargo build || true"}]
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(error.contains("masks failure"), "{error}");
    }

    #[test]
    fn unrecognized_probe_command_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [{"id": "a", "command": "echo hello"}]
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(
            error.contains("will never be recognized as a validation probe"),
            "{error}"
        );
    }

    #[test]
    fn scope_escaping_root_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [],
            "read_scope": ["../outside"]
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(error.contains("escapes the workspace root"), "{error}");
    }

    #[test]
    fn invalid_terminal_token_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [],
            "terminal": {"done_token": "", "fail_token": "FAIL"}
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(error.contains("done_token must not be empty"), "{error}");
    }

    #[test]
    fn inconsistent_artifact_invalidation_reference_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [],
            "mutable_artifact_classes": {"classes": []},
            "evidence_invalidation": {
                "invalidated_by_source_mutation": true,
                "generation_gated_freshness": true,
                "tracked_artifact_classes": ["documentation"]
            }
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(error.contains("unknown artifact class"), "{error}");
    }

    #[test]
    fn malformed_budget_is_rejected() {
        let json = r#"{
            "guidance": "g",
            "probes": [],
            "budgets": {
                "max_iterations": 0,
                "max_tool_iterations": 50,
                "context_window_tokens": null,
                "max_thinking_only_tokens": 4096,
                "repair_exit_thinking_tokens": 16384
            }
        }"#;
        let error = explicit(json).unwrap_err().to_string();
        assert!(
            error.contains("max_iterations must be greater than zero"),
            "{error}"
        );
    }

    #[test]
    fn fixture_files_resolve_and_fail_as_expected() {
        let fixtures_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts");

        for entry in std::fs::read_dir(fixtures_dir.join("explicit")).unwrap() {
            let path = entry.unwrap().path();
            let json = std::fs::read_to_string(&path).unwrap();
            explicit(&json).unwrap_or_else(|error| {
                panic!("expected {} to resolve, got {error}", path.display())
            });
        }

        for entry in std::fs::read_dir(fixtures_dir.join("invalid")).unwrap() {
            let path = entry.unwrap().path();
            let json = std::fs::read_to_string(&path).unwrap();
            let result = explicit(&json);
            assert!(
                result.is_err(),
                "expected {} to fail validation",
                path.display()
            );
        }
    }

    #[test]
    fn legacy_task_fixtures_match_committed_snapshots() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let legacy_dir = repo_root.join("fixtures/contracts/legacy");
        let snapshots_dir = repo_root.join("fixtures/contracts/snapshots");

        let mut task_dirs = std::fs::read_dir(&legacy_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        task_dirs.sort();
        assert!(!task_dirs.is_empty(), "expected legacy task fixtures");

        for task_dir in task_dirs {
            let task_name = task_dir.file_name().unwrap().to_str().unwrap();
            let goal_text = std::fs::read_to_string(task_dir.join("task.md")).unwrap();
            let resolved_once = legacy(&goal_text);
            let resolved_twice = legacy(&goal_text);
            assert_eq!(
                resolved_once, resolved_twice,
                "{task_name}: not deterministic"
            );

            let snapshot_path = snapshots_dir.join(format!("{task_name}.json"));
            let snapshot_text = std::fs::read_to_string(&snapshot_path)
                .unwrap_or_else(|error| panic!("reading {}: {error}", snapshot_path.display()));
            let snapshot: ResolvedRunContract = serde_json::from_str(&snapshot_text).unwrap();
            assert_eq!(resolved_once, snapshot, "{task_name}: snapshot drifted");
        }
    }

    #[test]
    fn resolved_contract_carries_the_coding_profile_ref() {
        let resolved = legacy("```sh\ncargo test\n```\n");
        assert_eq!(resolved.profile.id, "coding");
        assert_eq!(resolved.profile.version, "coding_profile.v1");
    }

    #[test]
    fn contract_json_missing_profile_field_back_fills_as_coding() {
        // Migration/back-compat: a contract persisted before Slice 3 added
        // `profile` has no such field in its JSON at all. `#[serde(default)]`
        // must still deserialize it, filling in the coding profile via
        // `ProfileRef::default`.
        let resolved = legacy("```sh\ncargo test\n```\n");
        let mut value = serde_json::to_value(&resolved).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .remove("profile")
            .expect("fixture must have had a profile field to remove");
        assert!(
            !value.to_string().contains("\"profile\""),
            "profile field must actually be absent from the JSON under test"
        );

        let parsed: ResolvedRunContract = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.profile.id, "coding");
        assert_eq!(parsed.profile.version, "coding_profile.v1");
    }
}
