//! Stable `RuntimeEvent` name registry for canonical trace measurement.
//!
//! `GENERALIZATION_PLAN.md` (Slice 1, "Canonicalize Measurement") requires
//! one documented, stable vocabulary of event names and payload fields that
//! [`crate::trace_analysis::analyze_trace`] reads to populate canonical
//! measurements. This module is that vocabulary: a `pub const` per event
//! kind, with a doc comment describing the payload fields the analyzer
//! consumes.
//!
//! Every constant here names an event the runtime *already* emits (via
//! [`crate::trace::Trace::event`] in `agent.rs`/`tools.rs`) **except** the
//! two documented as additive-only below. Adding a name here never changes
//! when or why the runtime prompts, permits actions, validates, repairs,
//! interrupts, or stops — it only gives the analyzer a stable string to
//! match instead of an inline literal.

/// Emitted once per run with the resolved run configuration and captured
/// [`crate::provenance::HarnessSourceState`]. Payload fields consumed by the
/// analyzer: `model`, `goal_file`, `tool_root`, `context_window_tokens`,
/// `max_tool_iterations`, `assembly_policy`, `transcript_policy`,
/// `packet_type`, `expected_output_tokens`, `harness_source_state`.
pub const RUN_STARTED: &str = "run.started";

/// Emitted once when a run reaches a terminal, successful summary. Payload
/// field consumed: `final_summary`. A `run.finished` event means the harness
/// itself completed — it never implies independent validation success; see
/// [`crate::trace_analysis::IndependentValidation`].
pub const RUN_FINISHED: &str = "run.finished";

/// Emitted once when the run's own trace parsing/execution failed.
pub const RUN_FAILED: &str = "run.failed";

/// Emitted the first time any tool call other than the accounting echo
/// [`TOOL_PAYLOAD_MEASURED`] is observed. Any `tool.*` kind qualifies:
/// [`TOOL_WRITE_FILE`], [`TOOL_EDIT_FILE`], [`TOOL_PATCH_FILE`],
/// [`TOOL_SHELL_COMMAND`], `tool.read_file`, `tool.list_tree`.
pub const TOOL_CALL_KIND_PREFIX: &str = "tool.";

/// Accounting echo emitted alongside every real tool event with estimated
/// token costs. Not itself a tool call — excluded from "first tool call"
/// detection. Payload fields consumed: `kind`, `total_tool_result_estimated_tokens`,
/// `max_tool_result_estimated_tokens`, `max_tool_result_kind`,
/// `result_estimated_tokens`.
pub const TOOL_PAYLOAD_MEASURED: &str = "tool.payload.measured";

/// A file write. Payload fields: `path`, `bytes_written`.
pub const TOOL_WRITE_FILE: &str = "tool.write_file";

/// A file edit (search/replace). Payload fields: `path`.
pub const TOOL_EDIT_FILE: &str = "tool.edit_file";

/// A file patch application. Payload fields: `path`.
pub const TOOL_PATCH_FILE: &str = "tool.patch_file";

/// A shell command invocation, used both for arbitrary commands and
/// validation probes. Payload fields consumed: `command`,
/// `validation_probe` (bool), `status` (process exit code), `success`
/// (bool), `repair_required` (present/non-null when a repair is active).
pub const TOOL_SHELL_COMMAND: &str = "tool.shell_command";

/// Emitted the first time a write or shell mutation touches a path the
/// runtime classifies as requiring validation. Payload fields: `action`
/// (`"write_intent"` or `"shell_mutation"`), `paths`,
/// `total_write_operations`. This is the canonical first-artifact-mutation
/// measurement; the analyzer prefers it over inferring mutation from
/// [`TOOL_WRITE_FILE`]/[`TOOL_EDIT_FILE`]/[`TOOL_PATCH_FILE`] payloads.
pub const AGENT_STAGE_FIRST_SOURCE_MUTATION: &str = "agent.stage.first_source_mutation";

/// Emitted the first time a validation probe (shell command with
/// `validation_probe: true`) is observed. Payload fields: `command`,
/// `command_family`, `status`, `success`, `total_shell_probes`. This is the
/// canonical first-validation-probe-reached measurement; the analyzer
/// prefers it over inferring probe reach from [`TOOL_SHELL_COMMAND`]
/// payloads.
pub const AGENT_STAGE_FIRST_VALIDATION_PROBE: &str = "agent.stage.first_validation_probe";

/// Emitted for every validation probe result, first and subsequent. Payload
/// fields consumed: `command`, `command_family`, `status`, `success`.
pub const AGENT_VALIDATION_PROBE_OBSERVED: &str = "agent.validation_probe.observed";

/// Escalated hard-stop: repeated action-boundary interrupts without source
/// mutation or validation. Maps to
/// [`crate::trace_analysis::HardStopReason::ActionBoundary`].
pub const AGENT_ACTION_BOUNDARY_HARD_FAILED: &str = "agent.action_boundary.hard_failed";

/// Escalated hard-stop: repeated hidden-reasoning turns with no visible
/// action. Maps to
/// [`crate::trace_analysis::HardStopReason::HiddenOnlyNoAction`].
pub const AGENT_TURN_HIDDEN_ONLY_NO_ACTION_HARD_FAILED: &str =
    "agent.turn.hidden_only_no_action_hard_failed";

/// Escalated hard-stop: validation repair could not proceed. Maps to
/// [`crate::trace_analysis::HardStopReason::ValidationRepair`].
pub const AGENT_VALIDATION_REPAIR_HARD_FAILED: &str = "agent.validation.repair_hard_failed";

/// Escalated hard-stop: validation repair exceeded its depth allowance.
/// Maps to
/// [`crate::trace_analysis::HardStopReason::ValidationRepairDepth`].
pub const AGENT_VALIDATION_REPAIR_DEPTH_HARD_FAILED: &str =
    "agent.validation.repair_depth_hard_failed";

/// Escalated hard-stop: repeated empty model responses. Maps to
/// [`crate::trace_analysis::HardStopReason::EmptyResponse`].
pub const AGENT_TURN_EMPTY_RESPONSE_HARD_FAILED: &str = "agent.turn.empty_response_hard_failed";

/// Escalated hard-stop: an inspection-only loop with no productive action.
/// Maps to
/// [`crate::trace_analysis::HardStopReason::InspectionLoop`].
pub const AGENT_INSPECTION_LOOP_HARD_FAILED: &str = "agent.inspection_loop.hard_failed";

/// Additive, documented-only event: **not currently emitted by the
/// runtime**. Reserved for an explicit operator-initiated stop (as opposed
/// to a runtime-detected hard-stop). Payload fields: `reason` (optional
/// string). When present, maps to
/// [`crate::trace_analysis::HarnessCompletion::ManuallyStopped`].
pub const AGENT_RUN_MANUAL_STOP: &str = "agent.run.manual_stop";

/// Additive, documented-only event: **not currently emitted by the
/// runtime**. Reserved for recording independent (external) validation
/// evidence — for example a matrix cell's separately-verified result.
/// Payload fields: `passed` (bool), `exit_status` (optional integer). Only
/// this event, or an explicit matrix result record, may set
/// [`crate::trace_analysis::IndependentValidation`] to a known value; a
/// `run.finished`/`DONE` completion never does.
pub const AGENT_INDEPENDENT_VALIDATION_OBSERVED: &str = "agent.independent_validation.observed";
