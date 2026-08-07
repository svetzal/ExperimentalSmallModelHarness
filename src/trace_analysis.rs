use crate::provenance::HarnessSourceState;
use crate::runtime_events as events;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Default)]
pub struct TraceAnalysis {
    pub trace_file: PathBuf,
    pub harness_source_state: Option<HarnessSourceState>,
    pub model: Option<String>,
    pub goal_file: Option<String>,
    pub tool_root: Option<String>,
    pub assembly_policy: Option<String>,
    pub transcript_policy: Option<String>,
    pub context_window_tokens: Option<usize>,
    pub max_tool_iterations: Option<usize>,
    pub packet_type: Option<String>,
    pub expected_output_tokens: Option<usize>,
    pub semantic_advisory_call_count: usize,
    pub semantic_advisory_rejection_count: usize,
    pub semantic_advisory_kinds: Vec<String>,
    pub semantic_advisory_models: Vec<String>,
    pub semantic_advisory_duration_ms_total: u64,
    pub semantic_advisory_errors: Vec<String>,
    pub initial_context_catalog_enabled: bool,
    pub initial_context_required_ids: Vec<String>,
    pub initial_context_advisory_selected_ids: Vec<String>,
    pub initial_context_excluded_ids: Vec<String>,
    pub initial_context_guidance_chars: usize,
    pub initial_context_worker_message_chars: usize,
    pub initial_context_components: Vec<Value>,
    pub initial_context_policy_accepted: Option<bool>,
    pub initial_context_policy_violations: Vec<Value>,
    pub initial_context_policy_errors: Vec<String>,
    // Legacy Slice-13 measurements remain populated for preserved traces.
    pub semantic_context_enabled: bool,
    pub semantic_context_analyzer_model: Option<String>,
    pub semantic_context_candidate_count: usize,
    pub semantic_context_analysis_duration_ms: Option<u64>,
    pub semantic_context_analysis_error: Option<String>,
    pub semantic_context_policy_accepted: Option<bool>,
    pub semantic_context_selected_ids: Vec<String>,
    pub semantic_context_injected_chars: usize,
    pub semantic_context_policy_violations: Vec<Value>,
    pub status: String,
    pub runtime_seconds: Option<f64>,
    pub llm_call_count: usize,
    pub max_llm_call_estimated_tokens: usize,
    pub max_llm_call_utilization: Option<f64>,
    pub max_pressure_band: String,
    pub pressure_band_counts: BTreeMap<String, usize>,
    pub context_delta_chars_total: isize,
    pub appended_message_count: usize,
    pub appended_chars_by_component: BTreeMap<String, usize>,
    pub cumulative_tool_result_estimated_tokens: usize,
    pub largest_tool_result_estimated_tokens: usize,
    pub largest_tool_result_kind: Option<String>,
    pub tool_result_estimated_tokens_by_kind: BTreeMap<String, usize>,
    pub stream_progress_events: usize,
    pub stream_metrics_events: usize,
    pub progress_status_events: usize,
    pub progress_state_counts: BTreeMap<String, usize>,
    pub runner_activity_evidence_events: usize,
    /// Reasoning-only calls interrupted at their protective cap and handed
    /// off to a subsequent action-only turn.
    pub thinking_only_action_transitions: usize,
    pub gpu_utilization_sample_events: usize,
    pub max_gpu_utilization_percent: Option<f64>,
    pub observed_output_tokens: usize,
    pub max_observed_tokens_per_second: Option<f64>,
    pub failed_tool_events: usize,
    pub validation_commands: Vec<ValidationCommandSummary>,
    pub final_summary_preview: Option<String>,
    pub outcome: RunOutcome,

    // Slice 1 canonical measurements. Each `Milestone` records the first
    // occurrence of a named canonical event; `None` means the trace carries
    // no evidence for that measurement (legacy trace, or the milestone never
    // happened), not that it definitely did not occur.
    pub first_tool_call: Option<Milestone>,
    pub first_productive_action: Option<Milestone>,
    pub first_source_mutation: Option<Milestone>,
    pub validation_probe_reached: Option<Milestone>,
    pub validation_probe_passed: Option<Milestone>,
    pub hard_stop: Option<HardStop>,
    pub environment_stop: Option<EnvironmentStop>,
    pub manual_stop: Option<ManualStop>,

    /// Whether the harness itself reached a terminal state, and which kind.
    /// Separate from [`Self::independent_validation`]: a run can be
    /// [`HarnessCompletion::Finished`] while independent validation is still
    /// [`IndependentValidation::Unknown`] — `DONE` is not a pass.
    pub harness_completion: HarnessCompletion,
    /// External evidence of whether the produced artifact actually passed
    /// its independent validation. Remains [`IndependentValidation::Unknown`]
    /// unless an explicit event
    /// ([`crate::runtime_events::AGENT_INDEPENDENT_VALIDATION_OBSERVED`]) or
    /// an explicit matrix result record supplies it.
    pub independent_validation: IndependentValidation,
    /// Whether the validation environment itself (as opposed to the
    /// harness or the generated artifact) was valid.
    pub environment_validity: EnvironmentValidity,

    // Slice 2 typed run contract (GENERALIZATION_PLAN.md). `None` for
    // traces recorded before contract resolution was introduced; the
    // runtime always emits both events together, so all three fields are
    // populated or all three are `None`.
    /// `resolved_contract.adapter_kind`, duplicated here as a plain string
    /// for convenient filtering without deserializing the full contract.
    pub contract_adapter_kind: Option<String>,
    /// The full resolved contract from [`crate::runtime_events::AGENT_CONTRACT_RESOLVED`].
    pub resolved_contract: Option<crate::contract::ResolvedRunContract>,
    /// What was supplied to resolution, from
    /// [`crate::runtime_events::AGENT_CONTRACT_SUPPLIED`].
    pub supplied_contract: Option<crate::contract::SuppliedContract>,
}

/// The first observed occurrence of a canonical measurement: which event
/// kind supplied the evidence, and an optional human-readable detail.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Milestone {
    pub evidence_event: String,
    pub detail: Option<String>,
}

/// Why a run escalated to a hard stop. Each variant corresponds to exactly
/// one runtime `*_hard_failed` event kind; see
/// [`crate::runtime_events`] for the full mapping.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HardStopReason {
    ActionBoundary,
    HiddenOnlyNoAction,
    ValidationRepair,
    ValidationRepairDepth,
    EmptyResponse,
    InspectionLoop,
    ThinkingOnlyStream,
    NoContentStream,
}

impl HardStopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            HardStopReason::ActionBoundary => "action_boundary",
            HardStopReason::HiddenOnlyNoAction => "hidden_only_no_action",
            HardStopReason::ValidationRepair => "validation_repair",
            HardStopReason::ValidationRepairDepth => "validation_repair_depth",
            HardStopReason::EmptyResponse => "empty_response",
            HardStopReason::InspectionLoop => "inspection_loop",
            HardStopReason::ThinkingOnlyStream => "thinking_only_stream",
            HardStopReason::NoContentStream => "no_content_stream",
        }
    }
}

/// A hard-stop escalation observed in the trace.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HardStop {
    pub reason: HardStopReason,
    pub evidence_event: String,
}

/// A validation probe that could not execute in its environment at all
/// (POSIX exit code 127, "command not found"), rather than executing and
/// failing on its own merits.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EnvironmentStop {
    pub command: String,
    pub status: i64,
    pub evidence_event: String,
}

/// An explicit, operator-initiated stop. See
/// [`crate::runtime_events::AGENT_RUN_MANUAL_STOP`] — documented and
/// additive only; the runtime does not currently emit this event.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ManualStop {
    pub reason: Option<String>,
    pub evidence_event: String,
}

/// Whether the harness itself reached a terminal state, and which kind.
/// This is strictly about the harness's own control flow — it says nothing
/// about whether the produced artifact was independently validated; see
/// [`IndependentValidation`] for that separate fact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HarnessCompletion {
    /// `run.finished` was observed and no hard-stop or manual stop preceded it.
    Finished,
    /// A hard-stop escalation (`*_hard_failed`) terminated the run.
    HardStopped,
    /// An explicit manual stop terminated the run.
    ManuallyStopped,
    /// `run.failed` was observed.
    Failed,
    /// The trace ended without any terminal event.
    #[default]
    Unfinished,
}

/// Independent (external) evidence of whether the produced artifact passed
/// its validation. This is deliberately never inferred from harness
/// completion: `run.finished`/`DONE` means the harness stopped cleanly, not
/// that anything was independently confirmed correct. It becomes known only
/// from an explicit event
/// ([`crate::runtime_events::AGENT_INDEPENDENT_VALIDATION_OBSERVED`]) or an
/// explicit matrix result record.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IndependentValidation {
    Passed,
    Failed,
    #[default]
    Unknown,
}

/// Whether the validation environment itself (as opposed to the harness or
/// the generated artifact) was valid — a `command not found` probe failure
/// indicates the environment, not the artifact, was untrustworthy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentValidity {
    Valid,
    Invalid,
    /// The environment was initially invalid but a corrected revalidation
    /// later confirmed the artifact (for example, a Homebrew Ruby
    /// interpreter conflict resolved by an explicit system-Ruby rerun).
    Corrected,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationCommandSummary {
    pub command: String,
    pub status: Option<i64>,
    pub success: Option<bool>,
    pub repair_required: bool,
}

/// A single canonical classification of how a run ended, derived
/// deterministically from events and metrics the runtime already emits.
///
/// This is the Slice 0 exit-condition field: it lets the five representative
/// traces (pass, validation-repair-then-pass, action-boundary stop,
/// hidden-only/no-action stop, environment-invalid validation) be
/// reproduced from canonical analyzer output alone, without reading
/// narrative notes or invoking matrix-specific classification code.
///
/// Variants are checked in the order they are declared below; the first
/// matching rule wins. See [`crate::trace_analysis::classify_outcome`] for
/// the precedence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// The run's own trace parsing/execution failed (`run.failed`).
    Failed,
    /// Escalated after repeated hidden-reasoning turns at an action
    /// boundary produced no source mutation or validation probe
    /// (`agent.action_boundary.hard_failed`).
    ActionBoundaryStop,
    /// Escalated after repeated turns produced only hidden reasoning with no
    /// visible action at all (`agent.turn.hidden_only_no_action_hard_failed`).
    HiddenOnlyNoActionStop,
    /// A validation probe could not execute in its environment at all,
    /// rather than executing and failing. Detected via the POSIX shell
    /// convention that exit code 127 means "command not found" — a general,
    /// non-matrix-specific signal that the validation environment itself
    /// (not the harness or the generated code) was invalid.
    EnvironmentInvalidValidation,
    /// The run finished, at least one validation probe required repair, and
    /// a later probe then succeeded.
    ValidationRepairPass,
    /// The run finished and at least one validation probe succeeded.
    Pass,
    /// The run finished but no validation probe is recorded as having
    /// succeeded.
    Finished,
    /// The trace ended without a terminal `run.finished`/`run.failed` event.
    Unfinished,
    /// No classifiable status could be determined.
    #[default]
    Unknown,
}

pub fn analyze_trace(path: impl AsRef<Path>) -> Result<TraceAnalysis> {
    let path = path.as_ref();
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut analysis = TraceAnalysis {
        trace_file: path.to_path_buf(),
        status: "unknown".to_string(),
        max_pressure_band: "unknown".to_string(),
        ..Default::default()
    };
    let mut first_timestamp = None;
    let mut last_timestamp = None;
    let mut saw_action_boundary_hard_failed = false;
    let mut saw_hidden_only_no_action_hard_failed = false;

    for (line_number, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line)
            .with_context(|| format!("parsing {} line {}", path.display(), line_number + 1))?;
        let kind = record["kind"].as_str().unwrap_or_default();
        let payload = &record["payload"];
        if let Some(timestamp) = record["timestamp"].as_str().and_then(parse_timestamp) {
            first_timestamp.get_or_insert(timestamp);
            last_timestamp = Some(timestamp);
        }

        if kind.starts_with(events::TOOL_CALL_KIND_PREFIX) && kind != events::TOOL_PAYLOAD_MEASURED
        {
            analysis.first_tool_call.get_or_insert_with(|| Milestone {
                evidence_event: kind.to_string(),
                detail: value_string(payload, "path").or_else(|| value_string(payload, "command")),
            });
        }

        match kind {
            events::RUN_STARTED => apply_run_started(&mut analysis, payload),
            events::RUN_FINISHED => apply_run_finished(&mut analysis, payload),
            events::RUN_FAILED => {
                analysis.status = "failed".to_string();
            }
            events::AGENT_ACTION_BOUNDARY_HARD_FAILED => {
                saw_action_boundary_hard_failed = true;
                set_hard_stop(&mut analysis, HardStopReason::ActionBoundary, kind);
            }
            events::AGENT_TURN_HIDDEN_ONLY_NO_ACTION_HARD_FAILED => {
                saw_hidden_only_no_action_hard_failed = true;
                set_hard_stop(&mut analysis, HardStopReason::HiddenOnlyNoAction, kind);
            }
            events::AGENT_VALIDATION_REPAIR_HARD_FAILED => {
                set_hard_stop(&mut analysis, HardStopReason::ValidationRepair, kind);
            }
            events::AGENT_VALIDATION_REPAIR_DEPTH_HARD_FAILED => {
                set_hard_stop(&mut analysis, HardStopReason::ValidationRepairDepth, kind);
            }
            events::AGENT_TURN_EMPTY_RESPONSE_HARD_FAILED => {
                set_hard_stop(&mut analysis, HardStopReason::EmptyResponse, kind);
            }
            events::AGENT_INSPECTION_LOOP_HARD_FAILED => {
                set_hard_stop(&mut analysis, HardStopReason::InspectionLoop, kind);
            }
            events::LLM_THINKING_ONLY_STREAM_HARD_FAILED => {
                set_hard_stop(&mut analysis, HardStopReason::ThinkingOnlyStream, kind);
            }
            events::LLM_THINKING_ONLY_STREAM_ACTION_TRANSITIONED => {
                analysis.thinking_only_action_transitions += 1;
            }
            events::LLM_NO_CONTENT_STREAM_HARD_FAILED => {
                set_hard_stop(&mut analysis, HardStopReason::NoContentStream, kind);
            }
            events::AGENT_RUN_MANUAL_STOP => {
                analysis.manual_stop.get_or_insert(ManualStop {
                    reason: value_string(payload, "reason"),
                    evidence_event: kind.to_string(),
                });
            }
            events::AGENT_INDEPENDENT_VALIDATION_OBSERVED => {
                analysis.independent_validation = if payload["passed"].as_bool() == Some(true) {
                    IndependentValidation::Passed
                } else {
                    IndependentValidation::Failed
                };
            }
            events::AGENT_CONTRACT_SUPPLIED => {
                analysis.supplied_contract =
                    serde_json::from_value(payload["supplied"].clone()).ok();
            }
            events::AGENT_CONTRACT_RESOLVED => {
                analysis.contract_adapter_kind = value_string(payload, "adapter_kind");
                analysis.resolved_contract =
                    serde_json::from_value(payload["resolved"].clone()).ok();
            }
            events::SEMANTIC_ADVISORY_REQUESTED => {
                analysis.semantic_advisory_call_count += 1;
                if let Some(kind) = value_string(payload, "advisory_kind")
                    && !analysis.semantic_advisory_kinds.contains(&kind)
                {
                    analysis.semantic_advisory_kinds.push(kind);
                }
                if let Some(model) = value_string(payload, "model")
                    && !analysis.semantic_advisory_models.contains(&model)
                {
                    analysis.semantic_advisory_models.push(model);
                }
            }
            events::SEMANTIC_ADVISORY_REJECTED => {
                analysis.semantic_advisory_rejection_count += 1;
                if let Some(kind) = value_string(payload, "advisory_kind")
                    && !analysis.semantic_advisory_kinds.contains(&kind)
                {
                    analysis.semantic_advisory_kinds.push(kind);
                }
                if let Some(model) = value_string(payload, "model")
                    && !analysis.semantic_advisory_models.contains(&model)
                {
                    analysis.semantic_advisory_models.push(model);
                }
                if let Some(error) = value_string(payload, "error") {
                    analysis.semantic_advisory_errors.push(error);
                }
            }
            events::SEMANTIC_ADVISORY_COMPLETED => {
                analysis.semantic_advisory_duration_ms_total = analysis
                    .semantic_advisory_duration_ms_total
                    .saturating_add(payload["duration_ms"].as_u64().unwrap_or_default());
            }
            events::SEMANTIC_ADVISORY_FAILED => {
                analysis.semantic_advisory_duration_ms_total = analysis
                    .semantic_advisory_duration_ms_total
                    .saturating_add(payload["duration_ms"].as_u64().unwrap_or_default());
                if let Some(error) = value_string(payload, "error") {
                    analysis.semantic_advisory_errors.push(error);
                }
            }
            events::INITIAL_CONTEXT_CATALOG_RESOLVED => {
                analysis.initial_context_catalog_enabled = true;
                analysis.initial_context_required_ids = value_string_array(payload, "required_ids");
                analysis.initial_context_excluded_ids = value_string_array(payload, "excluded_ids");
            }
            events::INITIAL_CONTEXT_POLICY_EVALUATED => {
                analysis.initial_context_policy_accepted = payload["accepted"].as_bool();
                analysis.initial_context_required_ids = value_string_array(payload, "required_ids");
                analysis.initial_context_advisory_selected_ids =
                    value_string_array(payload, "advisory_selected_ids");
                analysis.initial_context_excluded_ids = value_string_array(payload, "excluded_ids");
                analysis.initial_context_guidance_chars = payload["total_guidance_chars"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_default();
                analysis.initial_context_policy_violations = payload["violations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            events::INITIAL_CONTEXT_POLICY_FAILED => {
                if let Some(error) = value_string(payload, "error") {
                    analysis.initial_context_policy_errors.push(error);
                }
            }
            events::INITIAL_CONTEXT_ASSEMBLED => {
                analysis.initial_context_catalog_enabled =
                    payload["catalog_enabled"].as_bool().unwrap_or_default();
                analysis.initial_context_required_ids = value_string_array(payload, "required_ids");
                analysis.initial_context_advisory_selected_ids =
                    value_string_array(payload, "advisory_selected_ids");
                analysis.initial_context_excluded_ids = value_string_array(payload, "excluded_ids");
                analysis.initial_context_guidance_chars = payload["guidance_chars"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_default();
                analysis.initial_context_worker_message_chars = payload["worker_message_chars"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_default();
                analysis.initial_context_components = payload["components"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            events::SEMANTIC_CONTEXT_ANALYSIS_STARTED => {
                analysis.semantic_context_enabled = true;
                analysis.semantic_context_analyzer_model = value_string(payload, "analyzer_model");
                analysis.semantic_context_candidate_count = payload["candidate_count"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_default();
            }
            events::SEMANTIC_CONTEXT_ANALYSIS_COMPLETED => {
                analysis.semantic_context_analysis_duration_ms = payload["duration_ms"].as_u64();
            }
            events::SEMANTIC_CONTEXT_ANALYSIS_FAILED => {
                analysis.semantic_context_analysis_duration_ms = payload["duration_ms"].as_u64();
                analysis.semantic_context_analysis_error = value_string(payload, "error");
            }
            events::SEMANTIC_CONTEXT_POLICY_EVALUATED => {
                analysis.semantic_context_policy_accepted = payload["accepted"].as_bool();
                analysis.semantic_context_selected_ids =
                    value_string_array(payload, "selected_ids");
                analysis.semantic_context_injected_chars = payload["injected_chars"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_default();
                analysis.semantic_context_policy_violations = payload["violations"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            events::SEMANTIC_CONTEXT_INJECTED => {
                analysis.semantic_context_injected_chars = payload["injected_chars"]
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .unwrap_or_default();
            }
            events::AGENT_STAGE_FIRST_SOURCE_MUTATION => {
                note_productive_milestone(
                    &mut analysis.first_source_mutation,
                    &mut analysis.first_productive_action,
                    kind,
                    value_string(payload, "action"),
                );
            }
            events::AGENT_STAGE_FIRST_VALIDATION_PROBE => {
                note_productive_milestone(
                    &mut analysis.validation_probe_reached,
                    &mut analysis.first_productive_action,
                    kind,
                    value_string(payload, "command"),
                );
            }
            events::AGENT_VALIDATION_PROBE_OBSERVED => {
                if payload["success"].as_bool() == Some(true) {
                    analysis.validation_probe_passed.get_or_insert(Milestone {
                        evidence_event: kind.to_string(),
                        detail: value_string(payload, "command"),
                    });
                }
            }
            "llm.context_assembly.ledger" => apply_context_ledger(&mut analysis, payload),
            "llm.context_assembly.appended" => apply_context_append(&mut analysis, payload),
            "llm.stream.progress" => {
                analysis.stream_progress_events += 1;
            }
            "llm.progress.status" => apply_progress_status(&mut analysis, payload),
            "llm.stream.metrics" => apply_stream_metrics(&mut analysis, payload),
            events::TOOL_PAYLOAD_MEASURED => apply_tool_payload(&mut analysis, payload),
            events::TOOL_WRITE_FILE | events::TOOL_EDIT_FILE | events::TOOL_PATCH_FILE => {
                apply_legacy_mutation(&mut analysis, kind, payload);
            }
            events::TOOL_SHELL_COMMAND => apply_shell_command(&mut analysis, kind, payload),
            kind if kind.starts_with(events::TOOL_CALL_KIND_PREFIX)
                && is_failed_tool_payload(payload) =>
            {
                analysis.failed_tool_events += 1;
            }
            _ => {}
        }
    }

    if analysis.status == "unknown" {
        analysis.status = "unfinished".to_string();
    }
    analysis.runtime_seconds = first_timestamp
        .zip(last_timestamp)
        .map(|(first, last)| (last - first).num_milliseconds() as f64 / 1000.0);
    analysis.outcome = classify_outcome(
        &analysis,
        saw_action_boundary_hard_failed,
        saw_hidden_only_no_action_hard_failed,
    );
    analysis.harness_completion = classify_harness_completion(&analysis);
    Ok(analysis)
}

/// Record a hard-stop, first occurrence wins.
fn set_hard_stop(analysis: &mut TraceAnalysis, reason: HardStopReason, kind: &str) {
    analysis.hard_stop.get_or_insert(HardStop {
        reason,
        evidence_event: kind.to_string(),
    });
}

/// Record a canonical milestone (source mutation or validation-probe-reach)
/// and, on its first occurrence anywhere, also seed the overall
/// first-productive-action milestone. First occurrence wins for both.
fn note_productive_milestone(
    slot: &mut Option<Milestone>,
    productive_action: &mut Option<Milestone>,
    kind: &str,
    detail: Option<String>,
) {
    if slot.is_some() {
        return;
    }
    let milestone = Milestone {
        evidence_event: kind.to_string(),
        detail,
    };
    productive_action.get_or_insert_with(|| milestone.clone());
    *slot = Some(milestone);
}

/// Legacy adapter: when the canonical `agent.stage.first_source_mutation`
/// event is absent from an older trace, infer first source mutation from the
/// first observed file-mutating tool call instead.
fn apply_legacy_mutation(analysis: &mut TraceAnalysis, kind: &str, payload: &Value) {
    if analysis.first_source_mutation.is_none() {
        note_productive_milestone(
            &mut analysis.first_source_mutation,
            &mut analysis.first_productive_action,
            kind,
            value_string(payload, "path"),
        );
    }
}

/// Finalize [`HarnessCompletion`] from state already collected during the
/// scan: failure and stop signals take precedence over a clean finish.
fn classify_harness_completion(analysis: &TraceAnalysis) -> HarnessCompletion {
    if analysis.status == "failed" {
        HarnessCompletion::Failed
    } else if analysis.manual_stop.is_some() {
        HarnessCompletion::ManuallyStopped
    } else if analysis.hard_stop.is_some() {
        HarnessCompletion::HardStopped
    } else if analysis.status == "finished" {
        HarnessCompletion::Finished
    } else {
        HarnessCompletion::Unfinished
    }
}

/// Deterministically classify how a run ended from already-collected
/// analysis state. See [`RunOutcome`] for the meaning of each variant; rules
/// are checked in the order below and the first match wins.
fn classify_outcome(
    analysis: &TraceAnalysis,
    saw_action_boundary_hard_failed: bool,
    saw_hidden_only_no_action_hard_failed: bool,
) -> RunOutcome {
    if analysis.status == "failed" {
        return RunOutcome::Failed;
    }
    if saw_action_boundary_hard_failed {
        return RunOutcome::ActionBoundaryStop;
    }
    if saw_hidden_only_no_action_hard_failed {
        return RunOutcome::HiddenOnlyNoActionStop;
    }
    let environment_invalid = analysis
        .validation_commands
        .iter()
        .any(|probe| probe.success == Some(false) && probe.status == Some(127));
    if environment_invalid {
        return RunOutcome::EnvironmentInvalidValidation;
    }
    if analysis.status == "finished" {
        let repair_then_pass = analysis
            .validation_commands
            .iter()
            .position(|probe| probe.repair_required)
            .is_some_and(|repair_index| {
                analysis.validation_commands[repair_index + 1..]
                    .iter()
                    .any(|probe| probe.success == Some(true))
            });
        if repair_then_pass {
            return RunOutcome::ValidationRepairPass;
        }
        let any_pass = analysis
            .validation_commands
            .iter()
            .any(|probe| probe.success == Some(true));
        if any_pass {
            return RunOutcome::Pass;
        }
        return RunOutcome::Finished;
    }
    if analysis.status == "unfinished" {
        return RunOutcome::Unfinished;
    }
    RunOutcome::Unknown
}

fn apply_run_started(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.harness_source_state =
        serde_json::from_value(payload["harness_source_state"].clone()).ok();
    analysis.model = value_string(payload, "model");
    analysis.goal_file = value_string(payload, "goal_file");
    analysis.tool_root = value_string(payload, "tool_root");
    analysis.context_window_tokens = value_usize(payload, "context_window_tokens");
    analysis.max_tool_iterations = value_usize(payload, "max_tool_iterations");
    analysis.assembly_policy = value_string(payload, "assembly_policy");
    analysis.transcript_policy = value_string(payload, "transcript_policy");
    analysis.packet_type = value_string(payload, "packet_type");
    analysis.expected_output_tokens = value_usize(payload, "expected_output_tokens");
}

fn apply_run_finished(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.status = "finished".to_string();
    analysis.final_summary_preview = value_string(payload, "final_summary")
        .map(|summary| summary.chars().take(240).collect::<String>());
}

fn apply_context_ledger(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.llm_call_count += 1;
    if let Some(tokens) = value_usize(payload, "estimated_tokens") {
        analysis.max_llm_call_estimated_tokens = analysis.max_llm_call_estimated_tokens.max(tokens);
    }
    if let Some(utilization) = payload["utilization"].as_f64() {
        analysis.max_llm_call_utilization = Some(
            analysis
                .max_llm_call_utilization
                .unwrap_or(0.0)
                .max(utilization),
        );
    }
    if let Some(band) = payload["pressure_band"].as_str() {
        *analysis
            .pressure_band_counts
            .entry(band.to_string())
            .or_insert(0) += 1;
        if pressure_rank(band) > pressure_rank(&analysis.max_pressure_band) {
            analysis.max_pressure_band = band.to_string();
        }
    }
    if let Some(delta) = payload["delta_chars_from_previous_call"].as_i64() {
        analysis.context_delta_chars_total += delta as isize;
    }
    if analysis.assembly_policy.is_none() {
        analysis.assembly_policy = value_string(payload, "assembly_policy");
    }
    if analysis.transcript_policy.is_none() {
        analysis.transcript_policy = value_string(payload, "transcript_policy");
    }
}

fn apply_context_append(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.appended_message_count += 1;
    let component = payload["component"].as_str().unwrap_or("unknown");
    let chars = value_usize(payload, "message_chars")
        .or_else(|| value_usize(payload, "content_chars"))
        .unwrap_or_default();
    *analysis
        .appended_chars_by_component
        .entry(component.to_string())
        .or_insert(0) += chars;
}

fn apply_tool_payload(analysis: &mut TraceAnalysis, payload: &Value) {
    if let Some(tokens) = value_usize(payload, "total_tool_result_estimated_tokens") {
        analysis.cumulative_tool_result_estimated_tokens =
            analysis.cumulative_tool_result_estimated_tokens.max(tokens);
    }
    if let Some(tokens) = value_usize(payload, "max_tool_result_estimated_tokens") {
        analysis.largest_tool_result_estimated_tokens =
            analysis.largest_tool_result_estimated_tokens.max(tokens);
    }
    if let Some(kind) = value_string(payload, "max_tool_result_kind") {
        analysis.largest_tool_result_kind = Some(kind);
    }
    if let Some(kind) = value_string(payload, "kind") {
        *analysis
            .tool_result_estimated_tokens_by_kind
            .entry(kind)
            .or_insert(0) += value_usize(payload, "result_estimated_tokens").unwrap_or_default();
    }
}

fn apply_progress_status(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.progress_status_events += 1;
    if let Some(state) = value_string(payload, "progress_state") {
        *analysis.progress_state_counts.entry(state).or_insert(0) += 1;
    }
    if payload["runner_activity_evidence"].as_bool() == Some(true) {
        analysis.runner_activity_evidence_events += 1;
    }
    if let Some(utilization) = payload["runner_activity"]["gpu_utilization_percent"].as_f64() {
        analysis.gpu_utilization_sample_events += 1;
        analysis.max_gpu_utilization_percent = Some(
            analysis
                .max_gpu_utilization_percent
                .unwrap_or(0.0)
                .max(utilization),
        );
    }
}

fn apply_stream_metrics(analysis: &mut TraceAnalysis, payload: &Value) {
    analysis.stream_metrics_events += 1;
    analysis.observed_output_tokens += value_usize(payload, "eval_count").unwrap_or_default();
    if let Some(tokens_per_second) = payload["tokens_per_second"].as_f64() {
        analysis.max_observed_tokens_per_second = Some(
            analysis
                .max_observed_tokens_per_second
                .unwrap_or(0.0)
                .max(tokens_per_second),
        );
    }
    if analysis.packet_type.is_none() {
        analysis.packet_type = value_string(payload, "packet_type");
    }
    if analysis.expected_output_tokens.is_none() {
        analysis.expected_output_tokens = value_usize(payload, "expected_output_tokens");
    }
}

fn apply_shell_command(analysis: &mut TraceAnalysis, kind: &str, payload: &Value) {
    if payload["validation_probe"].as_bool().unwrap_or(false) {
        let command = payload["command"].as_str().unwrap_or_default().to_string();
        let status = payload["status"].as_i64();
        let success = payload["success"].as_bool();
        analysis.validation_commands.push(ValidationCommandSummary {
            command: command.clone(),
            status,
            success,
            repair_required: !payload["repair_required"].is_null(),
        });

        // Legacy adapter: older traces may lack
        // `agent.stage.first_validation_probe` / `agent.validation_probe.observed`.
        // Infer the same milestones from the raw shell-command payload.
        note_productive_milestone(
            &mut analysis.validation_probe_reached,
            &mut analysis.first_productive_action,
            kind,
            Some(command.clone()),
        );
        if success == Some(true) {
            analysis.validation_probe_passed.get_or_insert(Milestone {
                evidence_event: kind.to_string(),
                detail: Some(command.clone()),
            });
        }

        // POSIX convention: exit 127 means "command not found" — the
        // validation environment itself was invalid, not the harness or the
        // generated artifact.
        if success == Some(false) && status == Some(127) {
            analysis.environment_stop.get_or_insert(EnvironmentStop {
                command,
                status: 127,
                evidence_event: kind.to_string(),
            });
            analysis.environment_validity = EnvironmentValidity::Invalid;
        }
    }
    if is_failed_tool_payload(payload) {
        analysis.failed_tool_events += 1;
    }
}

fn is_failed_tool_payload(payload: &Value) -> bool {
    payload.get("error").is_some() || payload["success"].as_bool() == Some(false)
}

fn value_string(payload: &Value, key: &str) -> Option<String> {
    payload[key].as_str().map(ToString::to_string)
}

fn value_string_array(payload: &Value, key: &str) -> Vec<String> {
    payload[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn value_usize(payload: &Value, key: &str) -> Option<usize> {
    payload[key].as_u64().map(|value| value as usize)
}

fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

fn pressure_rank(band: &str) -> u8 {
    match band {
        "green" => 1,
        "yellow" => 2,
        "orange" => 3,
        "red" => 4,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzes_context_assembly_trace_events() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-test.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-06-11T00:00:00Z","kind":"run.started","payload":{"model":"qwen","goal_file":"/tmp/task.md","max_tool_iterations":50,"context_window_tokens":1000,"packet_type":"narrow-patch","expected_output_tokens":2048,"assembly_policy":"append_summarized_tool_transcript"}}
{"timestamp":"2026-06-11T00:00:01Z","kind":"llm.context_assembly.ledger","payload":{"estimated_tokens":100,"utilization":0.10,"pressure_band":"green","delta_chars_from_previous_call":null,"assembly_policy":"append_summarized_tool_transcript"}}
{"timestamp":"2026-06-11T00:00:02Z","kind":"llm.context_assembly.appended","payload":{"component":"tool_result","message_chars":400}}
{"timestamp":"2026-06-11T00:00:03Z","kind":"tool.payload.measured","payload":{"kind":"tool.read_file","result_estimated_tokens":100,"total_tool_result_estimated_tokens":100,"max_tool_result_estimated_tokens":100,"max_tool_result_kind":"tool.read_file"}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.context_assembly.ledger","payload":{"estimated_tokens":250,"utilization":0.25,"pressure_band":"orange","delta_chars_from_previous_call":600,"assembly_policy":"append_summarized_tool_transcript"}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.stream.progress","payload":{"frame_index":1}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.progress.status","payload":{"progress_state":"ProgressUnknown","runner_activity_evidence":true,"runner_activity":{"gpu_utilization_percent":38.5}}}
{"timestamp":"2026-06-11T00:00:04Z","kind":"llm.stream.metrics","payload":{"eval_count":200,"tokens_per_second":4.5,"packet_type":"narrow-patch","expected_output_tokens":2048}}
{"timestamp":"2026-06-11T00:00:05Z","kind":"tool.shell_command","payload":{"command":"cargo test","validation_probe":true,"status":101,"success":false,"repair_required":{"active":true}}}
{"timestamp":"2026-06-11T00:00:06Z","kind":"run.finished","payload":{"final_summary":"DONE"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();

        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.model.as_deref(), Some("qwen"));
        assert_eq!(analysis.llm_call_count, 2);
        assert_eq!(analysis.max_llm_call_estimated_tokens, 250);
        assert_eq!(analysis.max_pressure_band, "orange");
        assert_eq!(analysis.appended_message_count, 1);
        assert_eq!(analysis.cumulative_tool_result_estimated_tokens, 100);
        assert_eq!(analysis.packet_type.as_deref(), Some("narrow-patch"));
        assert_eq!(analysis.expected_output_tokens, Some(2048));
        assert_eq!(analysis.stream_progress_events, 1);
        assert_eq!(analysis.progress_status_events, 1);
        assert_eq!(
            analysis.progress_state_counts.get("ProgressUnknown"),
            Some(&1)
        );
        assert_eq!(analysis.runner_activity_evidence_events, 1);
        assert_eq!(analysis.gpu_utilization_sample_events, 1);
        assert_eq!(analysis.max_gpu_utilization_percent, Some(38.5));
        assert_eq!(analysis.stream_metrics_events, 1);
        assert_eq!(analysis.observed_output_tokens, 200);
        assert_eq!(analysis.max_observed_tokens_per_second, Some(4.5));
        assert_eq!(analysis.validation_commands.len(), 1);
        assert!(analysis.validation_commands[0].repair_required);
        assert_eq!(analysis.runtime_seconds, Some(6.0));
        assert_eq!(analysis.outcome, RunOutcome::Finished);
        assert!(analysis.harness_source_state.is_none());
    }

    #[test]
    fn analyzes_semantic_context_selection_metrics() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-semantic-context.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-08-06T00:00:00Z","kind":"run.started","payload":{"model":"worker"}}
{"timestamp":"2026-08-06T00:00:01Z","kind":"semantic_context.analysis.started","payload":{"analyzer_model":"curator","candidate_count":3}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"semantic_context.analysis.completed","payload":{"duration_ms":875}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"semantic_context.policy.evaluated","payload":{"accepted":true,"selected_ids":["format","api"],"injected_chars":640,"violations":[]}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"semantic_context.injected","payload":{"selected_ids":["format","api"],"injected_chars":680}}
{"timestamp":"2026-08-06T00:00:03Z","kind":"run.finished","payload":{"final_summary":"DONE"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();

        assert!(analysis.semantic_context_enabled);
        assert_eq!(
            analysis.semantic_context_analyzer_model.as_deref(),
            Some("curator")
        );
        assert_eq!(analysis.semantic_context_candidate_count, 3);
        assert_eq!(analysis.semantic_context_analysis_duration_ms, Some(875));
        assert_eq!(analysis.semantic_context_policy_accepted, Some(true));
        assert_eq!(
            analysis.semantic_context_selected_ids,
            vec!["format", "api"]
        );
        assert_eq!(analysis.semantic_context_injected_chars, 680);
        assert!(analysis.semantic_context_policy_violations.is_empty());
    }

    #[test]
    fn analyzes_native_initial_context_and_semantic_advisory_metrics() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-initial-context.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-08-06T00:00:00Z","kind":"run.started","payload":{"model":"worker"}}
{"timestamp":"2026-08-06T00:00:01Z","kind":"initial_context.catalog.resolved","payload":{"required_ids":["policy"],"selectable_ids":["format"],"excluded_ids":["private"]}}
{"timestamp":"2026-08-06T00:00:01Z","kind":"semantic_advisory.requested","payload":{"advisory_kind":"initial_context_selection","model":"small"}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"semantic_advisory.completed","payload":{"advisory_kind":"initial_context_selection","duration_ms":425}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"initial_context.policy.evaluated","payload":{"accepted":true,"required_ids":["policy"],"advisory_selected_ids":["format"],"excluded_ids":["private"],"total_guidance_chars":420,"violations":[]}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"initial_context.assembled","payload":{"catalog_enabled":true,"required_ids":["policy"],"advisory_selected_ids":["format"],"excluded_ids":["private"],"guidance_chars":420,"worker_message_chars":900,"components":[{"id":"policy"},{"id":"format"}]}}
{"timestamp":"2026-08-06T00:00:03Z","kind":"run.finished","payload":{"final_summary":"DONE"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();

        assert_eq!(analysis.semantic_advisory_call_count, 1);
        assert_eq!(
            analysis.semantic_advisory_kinds,
            vec!["initial_context_selection"]
        );
        assert_eq!(analysis.semantic_advisory_models, vec!["small"]);
        assert_eq!(analysis.semantic_advisory_duration_ms_total, 425);
        assert!(analysis.initial_context_catalog_enabled);
        assert_eq!(analysis.initial_context_required_ids, vec!["policy"]);
        assert_eq!(
            analysis.initial_context_advisory_selected_ids,
            vec!["format"]
        );
        assert_eq!(analysis.initial_context_excluded_ids, vec!["private"]);
        assert_eq!(analysis.initial_context_guidance_chars, 420);
        assert_eq!(analysis.initial_context_worker_message_chars, 900);
        assert_eq!(analysis.initial_context_components.len(), 2);
        assert_eq!(analysis.initial_context_policy_accepted, Some(true));
    }

    #[test]
    fn analyzes_semantic_advisory_preflight_rejection() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-advisory-rejected.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-08-06T00:00:00Z","kind":"run.started","payload":{"model":"worker"}}
{"timestamp":"2026-08-06T00:00:01Z","kind":"semantic_advisory.rejected","payload":{"advisory_kind":"failure_classification","model":"small","reason":"input_budget_exceeded","error":"request exceeds budget"}}
{"timestamp":"2026-08-06T00:00:01Z","kind":"run.failed","payload":{"stage":"semantic_advisory","error":"request exceeds budget"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();

        assert_eq!(analysis.semantic_advisory_call_count, 0);
        assert_eq!(analysis.semantic_advisory_rejection_count, 1);
        assert_eq!(
            analysis.semantic_advisory_kinds,
            vec!["failure_classification"]
        );
        assert_eq!(analysis.semantic_advisory_models, vec!["small"]);
        assert_eq!(
            analysis.semantic_advisory_errors,
            vec!["request exceeds budget"]
        );
        assert_eq!(analysis.outcome, RunOutcome::Failed);
    }

    #[test]
    fn analyzes_semantic_context_failure_as_failed_run() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-semantic-context-failed.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-08-06T00:00:00Z","kind":"run.started","payload":{"model":"worker"}}
{"timestamp":"2026-08-06T00:00:01Z","kind":"semantic_context.analysis.started","payload":{"analyzer_model":"curator","candidate_count":2}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"semantic_context.analysis.failed","payload":{"duration_ms":700,"error":"structured output decode failed"}}
{"timestamp":"2026-08-06T00:00:02Z","kind":"run.failed","payload":{"stage":"semantic_context","error":"structured output decode failed"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();

        assert_eq!(analysis.status, "failed");
        assert_eq!(analysis.outcome, RunOutcome::Failed);
        assert_eq!(analysis.semantic_context_analysis_duration_ms, Some(700));
        assert_eq!(
            analysis.semantic_context_analysis_error.as_deref(),
            Some("structured output decode failed")
        );
    }

    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/traces")
            .join(name)
    }

    #[test]
    fn classifies_a_passing_run() {
        let analysis = analyze_trace(fixture_path("pass.jsonl")).unwrap();

        assert_eq!(analysis.outcome, RunOutcome::Pass);
        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.validation_commands.len(), 1);
        assert_eq!(analysis.validation_commands[0].success, Some(true));
    }

    #[test]
    fn classifies_a_repair_then_pass_run() {
        let analysis = analyze_trace(fixture_path("validation_repair_pass.jsonl")).unwrap();

        assert_eq!(analysis.outcome, RunOutcome::ValidationRepairPass);
        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.validation_commands.len(), 2);
        assert!(analysis.validation_commands[0].repair_required);
        assert_eq!(analysis.validation_commands[1].success, Some(true));
    }

    #[test]
    fn classifies_an_action_boundary_stop() {
        let analysis = analyze_trace(fixture_path("action_boundary_stop.jsonl")).unwrap();

        assert_eq!(analysis.outcome, RunOutcome::ActionBoundaryStop);
        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.validation_commands.len(), 0);
    }

    #[test]
    fn classifies_a_hidden_only_no_action_stop() {
        let analysis = analyze_trace(fixture_path("hidden_only_no_action_stop.jsonl")).unwrap();

        assert_eq!(analysis.outcome, RunOutcome::HiddenOnlyNoActionStop);
        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.validation_commands.len(), 0);
    }

    #[test]
    fn classifies_an_environment_invalid_validation() {
        let analysis = analyze_trace(fixture_path("environment_invalid_validation.jsonl")).unwrap();

        assert_eq!(analysis.outcome, RunOutcome::EnvironmentInvalidValidation);
        assert_eq!(analysis.status, "finished");
        assert_eq!(analysis.validation_commands.len(), 1);
        assert_eq!(analysis.validation_commands[0].status, Some(127));
    }

    #[test]
    fn surfaces_harness_provenance_from_run_started() {
        let analysis = analyze_trace(fixture_path("action_boundary_stop.jsonl")).unwrap();

        let source_state = analysis
            .harness_source_state
            .expect("fixture carries harness_source_state");
        assert_eq!(
            source_state.git_head.as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef01")
        );
        assert_eq!(source_state.git_dirty, Some(true));
    }

    #[test]
    fn classifies_missing_git_provenance() {
        let temp = tempfile::tempdir().unwrap();
        let trace = temp.path().join("run-no-provenance.jsonl");
        std::fs::write(
            &trace,
            r#"{"timestamp":"2026-07-17T00:00:00Z","kind":"run.started","payload":{"model":"qwen","harness_source_state":{"manifest_dir":"/repo","git_head":null,"git_dirty":null,"source_state_note":"not a git checkout or git unavailable"}}}
{"timestamp":"2026-07-17T00:00:01Z","kind":"run.finished","payload":{"final_summary":"DONE"}}
"#,
        )
        .unwrap();

        let analysis = analyze_trace(&trace).unwrap();
        let source_state = analysis
            .harness_source_state
            .expect("null git fields still deserialize into a present HarnessSourceState");
        assert_eq!(source_state.git_head, None);
        assert_eq!(source_state.git_dirty, None);
        assert_eq!(
            source_state.source_state_note,
            "not a git checkout or git unavailable"
        );
    }

    #[test]
    fn analyzer_output_is_deterministic_across_invocations() {
        let first = analyze_trace(fixture_path("validation_repair_pass.jsonl")).unwrap();
        let second = analyze_trace(fixture_path("validation_repair_pass.jsonl")).unwrap();

        assert_eq!(
            serde_json::to_string_pretty(&first).unwrap(),
            serde_json::to_string_pretty(&second).unwrap()
        );
    }

    #[test]
    fn populates_canonical_milestones_for_a_passing_run() {
        let analysis = analyze_trace(fixture_path("pass.jsonl")).unwrap();

        let first_tool_call = analysis.first_tool_call.expect("first tool call recorded");
        assert_eq!(first_tool_call.evidence_event, "tool.write_file");

        let mutation = analysis
            .first_source_mutation
            .clone()
            .expect("first source mutation recorded");
        assert_eq!(mutation.evidence_event, "tool.write_file");

        let probe_reached = analysis
            .validation_probe_reached
            .expect("validation probe reach recorded");
        assert_eq!(probe_reached.evidence_event, "tool.shell_command");

        let probe_passed = analysis
            .validation_probe_passed
            .expect("validation probe pass recorded");
        assert_eq!(probe_passed.evidence_event, "tool.shell_command");

        // The first productive action is whichever of mutation/probe-reach
        // happened first in trace order; here that is the source mutation.
        assert_eq!(
            analysis.first_productive_action,
            analysis.first_source_mutation
        );

        assert_eq!(analysis.harness_completion, HarnessCompletion::Finished);
        assert_eq!(
            analysis.independent_validation,
            IndependentValidation::Unknown
        );
        assert_eq!(analysis.environment_validity, EnvironmentValidity::Unknown);
        assert!(analysis.hard_stop.is_none());
        assert!(analysis.manual_stop.is_none());
        assert!(analysis.environment_stop.is_none());
    }

    #[test]
    fn classifies_hard_stop_reason_for_action_boundary() {
        let analysis = analyze_trace(fixture_path("action_boundary_stop.jsonl")).unwrap();

        let hard_stop = analysis.hard_stop.expect("hard stop recorded");
        assert_eq!(hard_stop.reason, HardStopReason::ActionBoundary);
        assert_eq!(hard_stop.reason.as_str(), "action_boundary");
        assert_eq!(
            hard_stop.evidence_event,
            "agent.action_boundary.hard_failed"
        );
        assert_eq!(analysis.harness_completion, HarnessCompletion::HardStopped);
        assert!(analysis.first_source_mutation.is_none());
        assert!(analysis.validation_probe_reached.is_none());
    }

    #[test]
    fn classifies_hard_stop_reason_for_hidden_only_no_action() {
        let analysis = analyze_trace(fixture_path("hidden_only_no_action_stop.jsonl")).unwrap();

        let hard_stop = analysis.hard_stop.expect("hard stop recorded");
        assert_eq!(hard_stop.reason, HardStopReason::HiddenOnlyNoAction);
        assert_eq!(hard_stop.reason.as_str(), "hidden_only_no_action");
        assert_eq!(analysis.harness_completion, HarnessCompletion::HardStopped);
    }

    #[test]
    fn classifies_thinking_only_stream_hard_stop_before_run_failure() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/traces_slice6/thinking_only_stream_stop.jsonl");
        let analysis = analyze_trace(path).unwrap();
        let hard_stop = analysis.hard_stop.expect("hard stop recorded");
        assert_eq!(hard_stop.reason, HardStopReason::ThinkingOnlyStream);
        assert_eq!(hard_stop.reason.as_str(), "thinking_only_stream");
        assert_eq!(
            hard_stop.evidence_event,
            "llm.thinking_only_stream.hard_failed"
        );
        assert_eq!(analysis.status, "failed");
    }

    #[test]
    fn classifies_an_environment_stop_from_exit_127() {
        let analysis = analyze_trace(fixture_path("environment_invalid_validation.jsonl")).unwrap();

        let environment_stop = analysis
            .environment_stop
            .expect("environment stop recorded");
        assert_eq!(environment_stop.command, "rspec spec");
        assert_eq!(environment_stop.status, 127);
        assert_eq!(analysis.environment_validity, EnvironmentValidity::Invalid);
        // The harness itself still finished cleanly; only the validation
        // environment was invalid.
        assert_eq!(analysis.harness_completion, HarnessCompletion::Finished);
        assert!(analysis.hard_stop.is_none());
    }

    #[test]
    fn classifies_an_explicit_manual_stop() {
        let analysis = analyze_trace(fixture_path("manual_stop.jsonl")).unwrap();

        let manual_stop = analysis.manual_stop.expect("manual stop recorded");
        assert_eq!(
            manual_stop.reason.as_deref(),
            Some("operator requested stop")
        );
        assert_eq!(manual_stop.evidence_event, "agent.run.manual_stop");
        assert_eq!(
            analysis.harness_completion,
            HarnessCompletion::ManuallyStopped
        );
        assert!(analysis.first_source_mutation.is_some());
    }

    #[test]
    fn legacy_trace_with_only_a_terminal_summary_leaves_evidence_explicitly_absent() {
        let analysis = analyze_trace(fixture_path("legacy_missing_evidence.jsonl")).unwrap();

        assert!(analysis.first_tool_call.is_none());
        assert!(analysis.first_productive_action.is_none());
        assert!(analysis.first_source_mutation.is_none());
        assert!(analysis.validation_probe_reached.is_none());
        assert!(analysis.validation_probe_passed.is_none());
        assert!(analysis.hard_stop.is_none());
        assert!(analysis.environment_stop.is_none());
        assert!(analysis.manual_stop.is_none());

        // The harness finished (`DONE`), but that is not independent
        // validation evidence: it must stay Unknown, not be inferred as a
        // pass.
        assert_eq!(analysis.harness_completion, HarnessCompletion::Finished);
        assert_eq!(analysis.outcome, RunOutcome::Finished);
        assert_eq!(
            analysis.independent_validation,
            IndependentValidation::Unknown
        );
        assert_eq!(analysis.environment_validity, EnvironmentValidity::Unknown);
    }

    #[test]
    fn explicit_independent_validation_event_sets_the_typed_fact() {
        let analysis =
            analyze_trace(fixture_path("independent_validation_observed.jsonl")).unwrap();

        assert_eq!(
            analysis.independent_validation,
            IndependentValidation::Passed
        );
    }

    #[test]
    fn canonical_measurement_fields_round_trip_deterministically() {
        let first = analyze_trace(fixture_path("pass.jsonl")).unwrap();
        let second = analyze_trace(fixture_path("pass.jsonl")).unwrap();

        assert_eq!(
            serde_json::to_string_pretty(&first).unwrap(),
            serde_json::to_string_pretty(&second).unwrap()
        );
    }
}
