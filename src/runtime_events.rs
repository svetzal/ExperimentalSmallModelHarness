//! Backward-compatible trace adapter for the typed runtime vocabulary.
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

use crate::runtime::{RuntimeEvent, TerminalToken};
use serde::Serialize;
use serde_json::{Value, json};

/// Version of the typed reducer input vocabulary. This does not replace the
/// stable legacy trace schema; [`legacy_trace_events`] is the explicit adapter
/// that preserves those names and payload shapes.
pub const RUNTIME_EVENT_SCHEMA_VERSION: &str = "runtime_event.v1";

/// An isolated, proposal-only semantic advisory was requested.
/// Payload: kind, model, exact messages, schema, budgets, and authority.
pub const SEMANTIC_ADVISORY_REQUESTED: &str = "semantic_advisory.requested";
/// Deterministic preflight rejected an advisory before a provider call.
/// Payload: kind, model, bounded-input measurements, reason, and error.
pub const SEMANTIC_ADVISORY_REJECTED: &str = "semantic_advisory.rejected";
/// A semantic advisory returned one raw structured proposal.
/// Payload: kind, model, duration, and raw proposal.
pub const SEMANTIC_ADVISORY_COMPLETED: &str = "semantic_advisory.completed";
/// A semantic advisory provider call failed.
/// Payload: kind, model, duration, and error.
pub const SEMANTIC_ADVISORY_FAILED: &str = "semantic_advisory.failed";
/// Deterministic policy evaluated a proposed acceptance plan.
/// Payload: schema, accepted plan, item counts, and validation violations.
pub const ACCEPTANCE_PLAN_POLICY_EVALUATED: &str = "acceptance_plan.policy_evaluated";
/// One bounded acceptance-planning attempt finished.
/// Payload: attempt number, maximum attempts, outcome, and optional error.
pub const ACCEPTANCE_PLAN_ATTEMPT_FINISHED: &str = "acceptance_plan.attempt_finished";
/// One bounded acceptance-interaction attempt finished.
/// Payload: attempt number, maximum attempts, outcome, and optional error.
pub const ACCEPTANCE_INTERACTIONS_ATTEMPT_FINISHED: &str =
    "acceptance_interactions.attempt_finished";
/// Deterministic policy evaluated proposed acceptance interactions.
/// Payload: schema, scenarios, validation outcome, and measurement-only authority.
pub const ACCEPTANCE_INTERACTIONS_POLICY_EVALUATED: &str =
    "acceptance_interactions.policy_evaluated";
/// One bounded supplied-interaction evidence attempt finished.
/// Payload: attempt number, maximum attempts, outcome, and optional error.
pub const ACCEPTANCE_INTERACTION_EVIDENCE_ATTEMPT_FINISHED: &str =
    "acceptance_interaction_evidence.attempt_finished";
/// Deterministic policy evaluated evidence for one supplied interaction.
/// Payload: candidate, evidence, validation outcome, and measurement-only authority.
pub const ACCEPTANCE_INTERACTION_EVIDENCE_POLICY_EVALUATED: &str =
    "acceptance_interaction_evidence.policy_evaluated";
/// Initial-context dispositions and budgets were resolved from a catalog.
pub const INITIAL_CONTEXT_CATALOG_RESOLVED: &str = "initial_context.catalog.resolved";
/// No semantic advisory was needed because no selectable guidance existed.
pub const INITIAL_CONTEXT_ADVISORY_SKIPPED: &str = "initial_context.advisory.skipped";
/// Deterministic context authority evaluated an advisory proposal.
pub const INITIAL_CONTEXT_POLICY_EVALUATED: &str = "initial_context.policy.evaluated";
/// A proposal could not be decoded into the typed context-selection shape.
pub const INITIAL_CONTEXT_POLICY_FAILED: &str = "initial_context.policy.failed";
/// The authoritative worker initial-context packet was assembled.
pub const INITIAL_CONTEXT_ASSEMBLED: &str = "initial_context.assembled";

// Legacy Slice-13 vocabulary remains readable by the trace adapter.
/// An isolated semantic context-selection call was configured and started.
/// Payload: analyzer model, exact messages, candidate metadata, and budgets.
pub const SEMANTIC_CONTEXT_ANALYSIS_STARTED: &str = "semantic_context.analysis.started";
/// The provider returned one raw structured semantic-context decision.
/// Payload: analyzer model, duration, and raw decision.
pub const SEMANTIC_CONTEXT_ANALYSIS_COMPLETED: &str = "semantic_context.analysis.completed";
/// The isolated semantic-context call failed before policy evaluation.
/// Payload: analyzer model, duration, and error.
pub const SEMANTIC_CONTEXT_ANALYSIS_FAILED: &str = "semantic_context.analysis.failed";
/// Deterministic gates evaluated the model's proposed candidate IDs.
/// Payload: acceptance, selected IDs, injected characters, and violations.
pub const SEMANTIC_CONTEXT_POLICY_EVALUATED: &str = "semantic_context.policy.evaluated";
/// Accepted semantic guidance was added to the initial worker context.
/// Payload: selected IDs, exact content, and injected characters.
pub const SEMANTIC_CONTEXT_INJECTED: &str = "semantic_context.injected";
/// Semantic context selection was not configured for this run.
pub const SEMANTIC_CONTEXT_DISABLED: &str = "semantic_context.disabled";

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LegacyTraceEvent {
    pub kind: &'static str,
    pub payload: Value,
}

/// Translate typed reducer input into the pre-Slice-4 public trace vocabulary.
/// Orchestration adapters may add effect-specific payload fields before writing
/// the event, but policy code never consumes these strings.
pub fn legacy_trace_events(event: &RuntimeEvent) -> Vec<LegacyTraceEvent> {
    let one = |kind, payload| vec![LegacyTraceEvent { kind, payload }];
    match event {
        RuntimeEvent::RunStarted => one(RUN_STARTED, json!({})),
        RuntimeEvent::RunFinished => one(RUN_FINISHED, json!({})),
        RuntimeEvent::TurnStarted { turn } => one("agent.turn.started", json!({ "turn": turn })),
        RuntimeEvent::ModelCallStarted { turn, depth } => one(
            "llm.call.started",
            json!({ "turn": turn, "llm_call_depth": depth }),
        ),
        RuntimeEvent::ModelContent { chars } => {
            one("llm.content.observed", json!({ "chars": chars }))
        }
        RuntimeEvent::ModelThinking { chars } => {
            one("llm.thinking.observed", json!({ "chars": chars }))
        }
        RuntimeEvent::ModelToolCall { name } => {
            one("llm.tool_call.observed", json!({ "name": name }))
        }
        RuntimeEvent::ModelNoContent => one("llm.no_content_stream.observed", json!({})),
        RuntimeEvent::ToolRead { path } => one("tool.read_file", json!({ "path": path })),
        RuntimeEvent::ToolMutation {
            paths,
            evidence_invalidating_paths,
            source,
        } => one(
            AGENT_STAGE_FIRST_SOURCE_MUTATION,
            json!({
                "paths": evidence_invalidating_paths,
                "all_paths": paths,
                "action": source,
            }),
        ),
        RuntimeEvent::ValidationProbe {
            command,
            command_family,
            status,
            success,
            ..
        } => one(
            AGENT_VALIDATION_PROBE_OBSERVED,
            json!({
                "command": command,
                "command_family": command_family,
                "status": status,
                "success": success,
            }),
        ),
        RuntimeEvent::RequestedProbeObserved {
            probe_id,
            command,
            status,
            success,
        } => one(
            "agent.requested_validation.observed",
            json!({
                "probe_id": probe_id,
                "command": command,
                "status": status,
                "success": success,
            }),
        ),
        RuntimeEvent::ActionBoundaryInterrupted { turn } => {
            one("agent.action_boundary.interrupted", json!({ "turn": turn }))
        }
        RuntimeEvent::RepairNoContentInterrupted { turn } => one(
            "agent.validation.repair_no_content_interrupted",
            json!({ "turn": turn }),
        ),
        RuntimeEvent::RepairDepthExceeded { turn, reason } => one(
            AGENT_VALIDATION_REPAIR_DEPTH_HARD_FAILED,
            json!({ "turn": turn, "reason": reason }),
        ),
        RuntimeEvent::Inspection { signature } => one(
            "agent.inspection_loop.observed",
            json!({ "signature": signature }),
        ),
        RuntimeEvent::TurnFinished {
            turn,
            content,
            tool_calls,
            mutated,
            probed,
            ..
        } => one(
            "agent.turn.finished",
            json!({
                "turn": turn,
                "content": content,
                "tool_calls": tool_calls,
                "mutated": mutated,
                "probed": probed,
            }),
        ),
        RuntimeEvent::TerminalToken { token } => match token {
            TerminalToken::Done => one("agent.terminal.done_observed", json!({})),
            TerminalToken::Fail => one("agent.terminal.fail_observed", json!({})),
        },
        RuntimeEvent::ManualStop { reason } => {
            one(AGENT_RUN_MANUAL_STOP, json!({ "reason": reason }))
        }
        RuntimeEvent::EnvironmentStop { reason } => {
            one("agent.run.environment_stop", json!({ "reason": reason }))
        }
        RuntimeEvent::EffectFailed { effect, error } => {
            one(RUN_FAILED, json!({ "stage": effect, "error": error }))
        }
    }
}

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

/// A file write. Payload fields: `path`, `bytes_written`, `previous_bytes`,
/// `content_changed`, `before_sha256`, and `after_sha256`. Fingerprints expose
/// file identity without retaining generated source content in the trace.
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

/// A structured lifecycle phase reported by an adapter-declared command probe.
/// The harness accepts these markers only from the exact command registered in
/// the run contract. Payload fields: `probe_id`, `command_family`, `stream`,
/// `phase`, and the complete bounded `evidence` object printed by the probe.
pub const AGENT_VALIDATION_PROBE_PHASE_OBSERVED: &str = "agent.validation_probe.phase_observed";

/// A lifecycle marker from an adapter-declared command probe could not be
/// decoded. Payload fields: `probe_id`, `command_family`, `stream`, `reason`,
/// and `marker_preview`.
pub const AGENT_VALIDATION_PROBE_PHASE_INVALID: &str = "agent.validation_probe.phase_invalid";

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

/// A single model call exhausted its surfaced-reasoning allowance without
/// assistant content or a completed tool call.
pub const LLM_THINKING_ONLY_STREAM_HARD_FAILED: &str = "llm.thinking_only_stream.hard_failed";

/// A reasoning-only call reached its protective cap and was interrupted so
/// the next turn can receive a concrete action-only instruction. This is not
/// itself a hard stop; repeated failure to act is governed by the runtime's
/// hidden-only no-action escalation.
pub const LLM_THINKING_ONLY_STREAM_ACTION_TRANSITIONED: &str =
    "llm.thinking_only_stream.action_transitioned";

/// Provider metrics reported runaway output without assistant content,
/// surfaced reasoning, or a completed tool call.
pub const LLM_NO_CONTENT_STREAM_HARD_FAILED: &str = "llm.no_content_stream.hard_failed";

/// Emitted immediately before each worker provider call. This is the canonical
/// record of the exact request assembled by the harness: ordered messages,
/// active tool descriptors, model, and completion configuration. The context
/// ledger remains the compact measurement surface; this snapshot is the
/// fidelity surface used to reconstruct what the model actually received.
pub const LLM_PROVIDER_REQUEST_ASSEMBLED: &str = "llm.provider_request.assembled";

/// Emitted once for every tool call in the final provider response batch.
/// The payload preserves response index, call identity, tool name, bounded
/// canonical arguments, and a full-argument hash. This is response evidence,
/// not a tool effect; calls may remain unexecuted if policy stops the batch.
pub const LLM_RESPONSE_TOOL_CALL_NORMALIZED: &str = "llm.response.tool_call.normalized";

/// Emitted once when a provider response finishes with a non-empty tool-call
/// batch. Its aggregate count can be compared with normalized per-call events
/// to detect incomplete response evidence.
pub const LLM_STREAM_TOOL_CALLS_COMPLETED: &str = "llm.stream.tool_calls_completed";

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

/// Emitted once per run, immediately after contract resolution and before
/// `ToolScope`/tool construction or any LLM call. Describes what was
/// *supplied* (path-free: which adapter, and non-content metadata like a
/// goal path or explicit-contract source path) — not the resolved contract
/// itself; see [`AGENT_CONTRACT_RESOLVED`]. Payload fields: `adapter_kind`,
/// `supplied` (a [`crate::contract::SuppliedContract`]).
pub const AGENT_CONTRACT_SUPPLIED: &str = "agent.contract.supplied";

/// Emitted once per run, alongside [`AGENT_CONTRACT_SUPPLIED`], carrying the
/// full resolved [`crate::contract::ResolvedRunContract`] — schema version,
/// guidance, scope, artifact classes, evidence invalidation, probes,
/// budgets, terminal tokens, adapter kind, and defaults provenance. Payload
/// fields: `schema_version`, `adapter_kind`, `resolved` (a
/// [`crate::contract::ResolvedRunContract`]).
pub const AGENT_CONTRACT_RESOLVED: &str = "agent.contract.resolved";
/// Adapter-owned validation probes were rendered into the worker's initial
/// context. Payload fields: `probe_ids`, `command_probe_ids`,
/// `assertion_probe_ids`, and `worker_message_chars`.
pub const AGENT_CONTRACT_PROBES_DELIVERED: &str = "agent.contract.probes.delivered";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{RepairDepthReason, RuntimeEvent};

    #[test]
    fn typed_events_adapt_to_stable_legacy_trace_names() {
        let cases = [
            (RuntimeEvent::RunStarted, RUN_STARTED),
            (
                RuntimeEvent::RepairDepthExceeded {
                    turn: 4,
                    reason: RepairDepthReason::MaxLlmCallDepth,
                },
                AGENT_VALIDATION_REPAIR_DEPTH_HARD_FAILED,
            ),
            (
                RuntimeEvent::ManualStop {
                    reason: "operator".to_string(),
                },
                AGENT_RUN_MANUAL_STOP,
            ),
        ];
        for (event, expected_kind) in cases {
            assert_eq!(legacy_trace_events(&event)[0].kind, expected_kind);
        }
    }
}
