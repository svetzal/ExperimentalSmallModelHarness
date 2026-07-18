use crate::tools::{
    SuccessfulValidationSnapshot, ToolPolicySnapshot, ToolScope, ValidationRepairSnapshot,
    coding_tools,
};
use crate::trace::TraceRecorder;
use anyhow::{Context, Result};
use futures::StreamExt;
use mojentic::MojenticError;
use mojentic::llm::gateway::{StreamChunk, StreamMetrics};
use mojentic::llm::gateways::OllamaGateway;
use mojentic::llm::models::{LlmMessage, LlmToolCall, MessageRole};
use mojentic::llm::tools::{LlmTool, ToolRunCtx};
use mojentic::llm::{CompletionConfig, LlmGateway};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::time::MissedTickBehavior;

const APPROX_CHARS_PER_TOKEN: usize = 4;
const CONTEXT_INSTRUMENTATION_VERSION: &str = "generation4.context_ledger.v1";
const DEFAULT_THROUGHPUT_TPS: f64 = 1.0;
const MODEL_PROGRESS_WARMUP_SECONDS: f64 = 60.0;
const MODEL_PROGRESS_TOOL_JSON_SLACK_SECONDS: f64 = 120.0;
const MODEL_PROGRESS_VARIANCE_MULTIPLIER: f64 = 2.0;
const DEFAULT_PROGRESS_STATUS_INTERVAL_SECONDS: u64 = 30;
const RUNNER_SAMPLE_TIMEOUT_SECONDS: u64 = 3;
const MACMON_SAMPLE_TIMEOUT_SECONDS: u64 = 5;
const STALLED_CONFIRMATION_CHECKS: usize = 2;
const RETAIN_RAW_TOOL_RESULT_RECENT_COUNT: usize = 4;
const RETAIN_RAW_TOOL_RESULT_MAX_CHARS: usize = 6_000;
const REPAIR_HANDOFF_RAW_TOOL_RESULT_RECENT_COUNT: usize = 2;
const REPAIR_HANDOFF_RAW_TOOL_RESULT_MAX_CHARS: usize = 3_000;
const TOOL_RESULT_SUMMARY_PREFIX: &str = "[harness-retained-tool-result-summary]";
const EMPTY_RESPONSE_ESCALATION_TURNS: usize = 3;
const HIDDEN_ONLY_NO_ACTION_ESCALATION_TURNS: usize = 2;
const NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER: usize = 20;
const REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT: usize = 1_024;
const MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH: usize = 12;
const DEFAULT_REPAIR_EXIT_THINKING_TOKENS: usize = 16_384;
const ACTION_BOUNDARY_INTENT_HIT_LIMIT: usize = 2;
const ACTION_BOUNDARY_INTENT_BUFFER_CHARS: usize = 4_096;
const ACTION_BOUNDARY_INTENT_HIT_GAP_TOKENS: usize = 512;

#[derive(Debug, Clone)]
pub struct AgentRunConfig {
    pub experiment_dir: PathBuf,
    pub goal_file: PathBuf,
    pub model: String,
    pub max_iterations: usize,
    pub max_tool_iterations: usize,
    pub context_window_tokens: Option<usize>,
    pub packet_type: String,
    pub expected_output_tokens: usize,
    pub num_predict: Option<usize>,
    pub max_thinking_only_tokens: usize,
    pub repair_exit_thinking_tokens: usize,
    pub action_boundary_interrupt_tokens: usize,
    pub transcript_policy: TranscriptPolicy,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentRunSummary {
    pub experiment_dir: PathBuf,
    pub tool_root: PathBuf,
    pub goal_file: PathBuf,
    pub trace_file: PathBuf,
    pub model: String,
    pub max_iterations: usize,
    pub max_tool_iterations: usize,
    pub context_window_tokens: Option<usize>,
    pub packet_type: String,
    pub expected_output_tokens: usize,
    pub num_predict: Option<usize>,
    pub max_thinking_only_tokens: usize,
    pub repair_exit_thinking_tokens: usize,
    pub action_boundary_interrupt_tokens: usize,
    pub transcript_policy: TranscriptPolicy,
    pub final_summary: String,
    pub harness_source_state: crate::provenance::HarnessSourceState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptPolicy {
    FullTranscript,
    #[default]
    SummarizedTranscript,
    SummarizedRepairHandoff,
    ValidationRepairPacket,
}

impl TranscriptPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full-transcript" | "full" => Some(Self::FullTranscript),
            "summarized-transcript" | "summarized" | "summary" => Some(Self::SummarizedTranscript),
            "summarized-repair-handoff" | "repair-handoff" | "summarized-handoff" => {
                Some(Self::SummarizedRepairHandoff)
            }
            "validation-repair-packet" | "validation-repair" | "repair-packet" => {
                Some(Self::ValidationRepairPacket)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullTranscript => "append_full_tool_transcript",
            Self::SummarizedTranscript => "append_summarized_tool_transcript",
            Self::SummarizedRepairHandoff => {
                "append_summarized_tool_transcript_with_red_repair_handoff"
            }
            Self::ValidationRepairPacket => "append_validation_repair_packet",
        }
    }

    fn compaction(self) -> ToolResultCompaction {
        match self {
            Self::FullTranscript => ToolResultCompaction {
                enabled: false,
                raw_recent_count: usize::MAX,
                max_raw_tool_result_chars: usize::MAX,
                preserve_latest_failed_validation: false,
            },
            Self::SummarizedTranscript => ToolResultCompaction {
                enabled: true,
                raw_recent_count: RETAIN_RAW_TOOL_RESULT_RECENT_COUNT,
                max_raw_tool_result_chars: RETAIN_RAW_TOOL_RESULT_MAX_CHARS,
                preserve_latest_failed_validation: false,
            },
            Self::SummarizedRepairHandoff => ToolResultCompaction {
                enabled: true,
                raw_recent_count: RETAIN_RAW_TOOL_RESULT_RECENT_COUNT,
                max_raw_tool_result_chars: RETAIN_RAW_TOOL_RESULT_MAX_CHARS,
                preserve_latest_failed_validation: false,
            },
            Self::ValidationRepairPacket => ToolResultCompaction {
                enabled: true,
                raw_recent_count: REPAIR_HANDOFF_RAW_TOOL_RESULT_RECENT_COUNT,
                max_raw_tool_result_chars: REPAIR_HANDOFF_RAW_TOOL_RESULT_MAX_CHARS,
                preserve_latest_failed_validation: true,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ToolResultCompaction {
    enabled: bool,
    raw_recent_count: usize,
    max_raw_tool_result_chars: usize,
    preserve_latest_failed_validation: bool,
}

pub fn default_expected_output_tokens(packet_type: &str) -> usize {
    match packet_type {
        "diagnosis-only" => 512,
        "narrow-patch" => 2_048,
        "multi-file-edit" => 4_096,
        "multi-file-patch" => 4_096,
        "full-small-project" => 8_192,
        "validation-repair" => 2_048,
        _ => 4_096,
    }
}

pub fn default_max_thinking_only_tokens(
    expected_output_tokens: usize,
    num_predict: Option<usize>,
) -> usize {
    num_predict
        .filter(|tokens| *tokens > 0)
        .map(|tokens| tokens.div_ceil(4).max(expected_output_tokens))
        .unwrap_or(expected_output_tokens)
}

pub fn default_repair_exit_thinking_tokens() -> usize {
    DEFAULT_REPAIR_EXIT_THINKING_TOKENS
}

pub async fn run_coding_agent(config: AgentRunConfig) -> Result<AgentRunSummary> {
    let gateway = OllamaGateway::new();
    let tool_root = PathBuf::from(".")
        .canonicalize()
        .context("canonicalizing harness cwd")?;
    run_coding_agent_with_gateway(config, &gateway, tool_root).await
}

async fn run_coding_agent_with_gateway<G: LlmGateway + ?Sized>(
    config: AgentRunConfig,
    gateway: &G,
    tool_root: PathBuf,
) -> Result<AgentRunSummary> {
    let experiment_dir = config
        .experiment_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", config.experiment_dir.display()))?;
    let goal_file = canonicalize_goal(&experiment_dir, &config.goal_file)?;
    let goal = tokio::fs::read_to_string(&goal_file)
        .await
        .with_context(|| format!("reading goal file {}", goal_file.display()))?;

    // Resolve the typed run contract before any tool scope, prompt, or LLM
    // effect (GENERALIZATION_PLAN.md Slice 2). The legacy coding adapter
    // wraps today's shell-fence scraping, so `requested_validation_commands`
    // below carries exactly the same ordered, deduped command strings as
    // before this slice.
    let contract_source = crate::contract::ContractSource::Legacy {
        goal_path: goal_file.display().to_string(),
        goal_text: goal.clone(),
    };
    let contract_budgets = crate::contract::Budgets {
        max_iterations: config.max_iterations,
        max_tool_iterations: config.max_tool_iterations,
        context_window_tokens: config.context_window_tokens,
        max_thinking_only_tokens: config.max_thinking_only_tokens,
        repair_exit_thinking_tokens: config.repair_exit_thinking_tokens,
    };
    let supplied_contract = crate::contract::supplied_contract_for(&contract_source);
    let resolved_contract = crate::contract::resolve_contract(contract_source, contract_budgets)?;
    let requested_validation_commands: Vec<String> = resolved_contract
        .probes
        .iter()
        .map(|probe| probe.command.clone())
        .collect();

    let tool_root = tool_root
        .canonicalize()
        .with_context(|| format!("canonicalizing tool root {}", tool_root.display()))?;

    let trace = Arc::new(TraceRecorder::create(&experiment_dir.join("traces"))?);
    let harness_source_state = crate::provenance::capture();
    trace.event(
        "run.started",
        serde_json::json!({
            "experiment_dir": experiment_dir,
            "tool_root": tool_root,
            "process_cwd": std::env::current_dir().ok(),
            "goal_file": goal_file,
            "model": config.model,
            "max_iterations": config.max_iterations,
            "max_tool_iterations": config.max_tool_iterations,
            "context_window_tokens": config.context_window_tokens,
            "packet_type": config.packet_type,
            "expected_output_tokens": config.expected_output_tokens,
            "num_predict": config.num_predict,
            "max_thinking_only_tokens": config.max_thinking_only_tokens,
            "repair_exit_thinking_tokens": config.repair_exit_thinking_tokens,
            "action_boundary_interrupt_tokens": config.action_boundary_interrupt_tokens,
            "assembly_policy": config.transcript_policy.as_str(),
            "transcript_policy": config.transcript_policy,
            "requested_validation_commands": &requested_validation_commands,
            "context_instrumentation_version": CONTEXT_INSTRUMENTATION_VERSION,
            "harness_package_version": env!("CARGO_PKG_VERSION"),
            "harness_source_state": &harness_source_state,
        }),
    )?;

    trace.event(
        "packet.scope",
        serde_json::json!({
            "tool_root": tool_root,
            "read_allow": ["."],
            "write_allow": ["."],
            "note": "Tools are rooted at the generated project workspace."
        }),
    )?;

    trace.event(
        crate::runtime_events::AGENT_CONTRACT_SUPPLIED,
        serde_json::json!({
            "adapter_kind": resolved_contract.adapter_kind,
            "supplied": &supplied_contract,
        }),
    )?;
    trace.event(
        crate::runtime_events::AGENT_CONTRACT_RESOLVED,
        serde_json::json!({
            "schema_version": &resolved_contract.schema_version,
            "adapter_kind": resolved_contract.adapter_kind,
            "resolved": &resolved_contract,
        }),
    )?;

    let scope = ToolScope::new(tool_root.clone(), Arc::clone(&trace))?;
    let requested_probe_ids_by_command = resolved_contract
        .probes
        .iter()
        .map(|probe| (probe.command.clone(), probe.id.clone()))
        .collect::<BTreeMap<_, _>>();
    scope.configure_requested_probes(
        resolved_contract
            .probes
            .iter()
            .map(|probe| probe.id.clone())
            .collect(),
    );
    let profile = crate::profile::select_profile();
    let system_prompt = profile.system_guidance();
    let tools = coding_tools(&scope);
    let mut messages = vec![
        LlmMessage::system(system_prompt),
        LlmMessage::user(profile.run_guidance(&goal)),
    ];
    let num_predict = config
        .num_predict
        .map(i32::try_from)
        .transpose()
        .context("num_predict exceeds i32 range")?;
    let completion_config = CompletionConfig {
        temperature: 0.2,
        max_tool_iterations: config.max_tool_iterations,
        num_predict,
        ..Default::default()
    };

    let mut final_summary = String::new();
    let mut final_response_only_next_turn = false;
    let mut requested_validation_ledger =
        RequestedValidationLedger::new(requested_validation_commands.clone());
    scope.observe_runtime(crate::runtime::RuntimeEvent::RunStarted);
    let requested_validation_completed_write_operations = 0usize;
    let mut exhausted_iterations = true;
    let no_tools: Vec<Box<dyn LlmTool>> = Vec::new();
    for turn in 1..=config.max_iterations {
        scope.observe_runtime(crate::runtime::RuntimeEvent::TurnStarted { turn });
        let runtime_before_turn = scope.runtime_state_snapshot();
        let policy_before_turn = scope.policy_snapshot();
        let requested_validation_ledger_before_turn = requested_validation_ledger.clone();
        let active_tools = if final_response_only_next_turn {
            &no_tools
        } else {
            &tools
        };
        trace.event(
            "agent.context.estimated",
            context_snapshot(
                &messages,
                active_tools,
                &policy_before_turn,
                config.context_window_tokens,
                config.transcript_policy,
            ),
        )?;
        trace.event(
            "agent.turn.started",
            serde_json::json!({
                "turn": turn,
                "max_iterations": config.max_iterations,
                "final_response_only": final_response_only_next_turn,
            }),
        )?;
        let turn_result = match stream_response(StreamResponseRequest {
            gateway,
            model: &config.model,
            messages: &messages,
            tools: active_tools,
            completion_config: completion_config.clone(),
            context_window_tokens: config.context_window_tokens,
            packet_type: &config.packet_type,
            expected_output_tokens: config.expected_output_tokens,
            max_thinking_only_tokens: config.max_thinking_only_tokens,
            repair_exit_thinking_tokens: config.repair_exit_thinking_tokens,
            action_boundary_interrupt_tokens: config.action_boundary_interrupt_tokens,
            validation_repair_active: policy_before_turn.validation_repair.is_some(),
            transcript_policy: config.transcript_policy,
            throughput_registry_path: experiment_dir.join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn,
            requested_validation_commands: &requested_validation_commands,
            requested_validation_pending_after_write: !requested_validation_commands.is_empty()
                && policy_before_turn.total_write_operations
                    > requested_validation_completed_write_operations,
            requested_validation_ledger: requested_validation_ledger.clone(),
        })
        .await
        {
            Ok(turn_result) => turn_result,
            Err(error) => {
                let _ = trace_run_failed(
                    &trace,
                    "agent.stream",
                    Some(turn),
                    &error,
                    &config,
                    &tool_root,
                    &goal_file,
                );
                return Err(error);
            }
        };
        final_response_only_next_turn = false;
        let repair_no_content_interrupted = turn_result.repair_no_content_interrupted;
        let action_boundary_interrupted = turn_result.action_boundary_interrupted;
        let repair_depth_hard_stop = turn_result.repair_depth_hard_stop;
        let thinking_chars_this_turn = turn_result.thinking_chars;
        let response = turn_result.response;
        requested_validation_ledger = turn_result.requested_validation_ledger;
        for entry in &requested_validation_ledger.entries {
            if matches!(
                entry.status,
                RequestedValidationStatus::Passed | RequestedValidationStatus::Failed
            ) && entry.generation == Some(requested_validation_ledger.generation)
            {
                scope.observe_runtime(crate::runtime::RuntimeEvent::RequestedProbeObserved {
                    probe_id: requested_probe_ids_by_command
                        .get(&entry.command)
                        .cloned()
                        .unwrap_or_else(|| entry.command.clone()),
                    command: entry
                        .observed_command
                        .clone()
                        .unwrap_or_else(|| entry.command.clone()),
                    status: entry.status_code,
                    success: entry.status == RequestedValidationStatus::Passed,
                });
            }
        }
        messages = turn_result.messages;
        trace.event(
            "agent.turn.finished",
            serde_json::json!({
                "turn": turn,
                "response": response,
                "thinking_chars_this_turn": thinking_chars_this_turn,
            }),
        )?;
        let policy = scope.policy_snapshot();
        let tool_calls_this_turn = policy.total_tool_calls - policy_before_turn.total_tool_calls;
        let wrote_this_turn =
            policy.total_write_operations > policy_before_turn.total_write_operations;
        let probed_this_turn = policy.total_shell_probes > policy_before_turn.total_shell_probes;
        let runtime_after_effects = scope.runtime_state_snapshot();
        let terminalize_after_validation = (!runtime_before_turn.terminal_readiness
            && runtime_after_effects.terminal_readiness
            && policy.total_shell_probes > policy_before_turn.total_shell_probes)
            .then(|| policy.latest_successful_validation_after_write.clone())
            .flatten()
            .or_else(|| {
                (!requested_validation_commands.is_empty()
                    && !requested_validation_ledger_before_turn.is_satisfied()
                    && requested_validation_ledger.is_satisfied())
                .then(|| requested_validation_ledger.latest_successful_validation())
                .flatten()
            });
        trace.event(
            "agent.context.turn_pressure",
            serde_json::json!({
                "turn": turn,
                "tool_calls_this_turn": tool_calls_this_turn,
                "tool_result_chars_this_turn": policy.total_tool_result_chars.saturating_sub(policy_before_turn.total_tool_result_chars),
                "tool_result_estimated_tokens_this_turn": estimate_tokens(policy.total_tool_result_chars.saturating_sub(policy_before_turn.total_tool_result_chars)),
                "cumulative_tool_result_chars": policy.total_tool_result_chars,
                "cumulative_tool_result_estimated_tokens": policy.total_tool_result_estimated_tokens,
                "max_tool_result_chars": policy.max_tool_result_chars,
                "max_tool_result_estimated_tokens": policy.max_tool_result_estimated_tokens,
                "max_tool_result_kind": policy.max_tool_result_kind,
                "tool_result_chars_by_kind": policy.tool_result_chars_by_kind,
                "thinking_chars_this_turn": thinking_chars_this_turn,
            }),
        )?;
        if let Some(interrupt) = &action_boundary_interrupted {
            scope.observe_runtime(crate::runtime::RuntimeEvent::ActionBoundaryInterrupted {
                turn: interrupt.turn,
            });
        }
        if repair_no_content_interrupted {
            scope
                .observe_runtime(crate::runtime::RuntimeEvent::RepairNoContentInterrupted { turn });
        }
        let turn_decision = scope.observe_runtime(crate::runtime::RuntimeEvent::TurnFinished {
            turn,
            content: !response.trim().is_empty(),
            thinking: thinking_chars_this_turn > 0,
            tool_calls: tool_calls_this_turn,
            mutated: wrote_this_turn,
            probed: probed_this_turn,
            repair_was_active_before: policy_before_turn.validation_repair.is_some(),
            repair_interrupted: repair_no_content_interrupted,
            action_boundary_interrupted: action_boundary_interrupted.is_some(),
        });
        let runtime_after_turn = scope.runtime_state_snapshot();
        let consecutive_empty_responses = runtime_after_turn.consecutive_empty_responses;
        let consecutive_hidden_only_no_action_turns =
            runtime_after_turn.consecutive_hidden_only_no_action_turns;
        let repair_no_action = matches!(
            turn_decision,
            crate::runtime::RuntimeDecision::EscalateRepair
                | crate::runtime::RuntimeDecision::HardStopRepairNoAction
        )
        .then(|| RepairNoActionDecision {
            turn,
            tool_calls_this_turn,
            reason: if repair_no_content_interrupted {
                RepairNoActionReason::NoContentInterrupted
            } else {
                RepairNoActionReason::NoRepairAction
            },
            consecutive_no_action_turns: runtime_after_turn.consecutive_repair_no_action_turns,
            escalation_required: matches!(
                turn_decision,
                crate::runtime::RuntimeDecision::HardStopRepairNoAction
            ),
            active_repair: policy
                .validation_repair
                .clone()
                .expect("repair decision requires active repair"),
            validation_repair_read_paths: policy.validation_repair_read_paths.clone(),
            total_write_operations_before_turn: policy_before_turn.total_write_operations,
            total_write_operations_after_turn: policy.total_write_operations,
            total_shell_probes_before_turn: policy_before_turn.total_shell_probes,
            total_shell_probes_after_turn: policy.total_shell_probes,
        });
        if let Some(decision) = &repair_no_action {
            trace.event("agent.validation.repair_no_action", decision)?;
            if decision.escalation_required {
                trace.event("agent.validation.repair_escalated", decision)?;
            }
        }
        if let Some(decision) = &repair_no_action
            && decision.escalation_required
            && !is_fail_response(&response)
        {
            final_summary = repair_hard_failure_summary(decision);
            trace.event("agent.validation.repair_hard_failed", decision)?;
            exhausted_iterations = false;
            break;
        }
        if let Some(decision) = &repair_depth_hard_stop {
            final_summary = repair_depth_failure_summary(decision);
            exhausted_iterations = false;
            break;
        }
        let action_boundary_no_action = action_boundary_interrupted.clone().and_then(|interrupt| {
            matches!(
                turn_decision,
                crate::runtime::RuntimeDecision::PromptActionBoundary
                    | crate::runtime::RuntimeDecision::HardStopActionBoundary
            )
            .then(|| ActionBoundaryNoActionDecision {
                turn,
                tool_calls_this_turn,
                consecutive_no_action_turns: runtime_after_turn
                    .consecutive_action_boundary_no_action_turns,
                escalation_required: matches!(
                    turn_decision,
                    crate::runtime::RuntimeDecision::HardStopActionBoundary
                ),
                interrupt,
                total_write_operations_before_turn: policy_before_turn.total_write_operations,
                total_write_operations_after_turn: policy.total_write_operations,
                total_shell_probes_before_turn: policy_before_turn.total_shell_probes,
                total_shell_probes_after_turn: policy.total_shell_probes,
            })
        });
        if let Some(decision) = &action_boundary_no_action {
            trace.event("agent.action_boundary.no_action", decision)?;
            if decision.escalation_required {
                final_summary = action_boundary_no_action_failure_summary(decision);
                trace.event("agent.action_boundary.hard_failed", decision)?;
                exhausted_iterations = false;
                break;
            }
            final_summary = format!(
                "turn {turn} action-boundary interrupt after hidden reasoning with no source mutation or validation probe"
            );
            trace.event("agent.action_boundary.prompted", &decision.interrupt)?;
            if !response.trim().is_empty() {
                messages.push(LlmMessage::assistant(response.clone()));
            }
            messages.push(LlmMessage::user(action_boundary_interrupt_prompt(decision)));
            continue;
        }
        if response.trim().is_empty() {
            if policy.validation_required_after_write {
                trace.event(
                    "agent.validation.required_after_edit",
                    serde_json::json!({
                        "turn": turn,
                        "tool_calls_this_turn": tool_calls_this_turn,
                        "policy": policy,
                    }),
                )?;
                final_summary = format!("turn {turn} modified files without follow-up validation");
                messages.push(LlmMessage::user(
                    crate::profile::select_profile().post_write_validation_nudge(false),
                ));
                continue;
            }
            if let Some(decision) = &repair_no_action {
                final_summary = format!(
                    "turn {turn} made no validation-repair edit or probe after validation failure"
                );
                messages.push(LlmMessage::user(validation_repair_no_action_prompt(
                    decision,
                )));
                continue;
            }
            if let Some(interrupt) = &action_boundary_interrupted {
                final_summary = format!(
                    "turn {turn} action-boundary interrupt after hidden reasoning with no visible action"
                );
                trace.event("agent.action_boundary.prompted", interrupt)?;
                messages.push(LlmMessage::user(
                    action_boundary_interrupt_prompt_for_interrupt(interrupt),
                ));
                continue;
            }
            if thinking_chars_this_turn > 0 && !wrote_this_turn && !probed_this_turn {
                let escalation_required = consecutive_hidden_only_no_action_turns
                    >= HIDDEN_ONLY_NO_ACTION_ESCALATION_TURNS;
                trace.event(
                    "agent.turn.thinking_only_response",
                    serde_json::json!({
                        "turn": turn,
                        "thinking_chars_this_turn": thinking_chars_this_turn,
                        "consecutive_empty_responses": consecutive_empty_responses,
                        "consecutive_hidden_only_no_action_turns": consecutive_hidden_only_no_action_turns,
                    }),
                )?;
                trace.event(
                    "agent.turn.hidden_only_no_action",
                    serde_json::json!({
                        "turn": turn,
                        "thinking_chars_this_turn": thinking_chars_this_turn,
                        "tool_calls_this_turn": tool_calls_this_turn,
                        "wrote_this_turn": wrote_this_turn,
                        "probed_this_turn": probed_this_turn,
                        "consecutive_hidden_only_no_action_turns": consecutive_hidden_only_no_action_turns,
                        "escalation_required": escalation_required,
                        "policy": policy,
                    }),
                )?;
                if escalation_required {
                    final_summary = hidden_only_no_action_hard_failure_summary(
                        turn,
                        consecutive_hidden_only_no_action_turns,
                    );
                    trace.event(
                        "agent.turn.hidden_only_no_action_escalated",
                        serde_json::json!({
                            "turn": turn,
                            "consecutive_hidden_only_no_action_turns": consecutive_hidden_only_no_action_turns,
                            "thinking_chars_this_turn": thinking_chars_this_turn,
                            "tool_calls_this_turn": tool_calls_this_turn,
                        }),
                    )?;
                    trace.event(
                        "agent.turn.hidden_only_no_action_hard_failed",
                        serde_json::json!({
                            "turn": turn,
                            "consecutive_hidden_only_no_action_turns": consecutive_hidden_only_no_action_turns,
                            "thinking_chars_this_turn": thinking_chars_this_turn,
                            "tool_calls_this_turn": tool_calls_this_turn,
                            "final_summary": final_summary,
                        }),
                    )?;
                    exhausted_iterations = false;
                    break;
                }
                final_summary = format!(
                    "turn {turn} produced hidden reasoning but no source mutation, validation probe, or final text"
                );
                messages.push(LlmMessage::user(hidden_only_no_action_prompt(
                    consecutive_hidden_only_no_action_turns,
                    tool_calls_this_turn,
                )));
                continue;
            }
            if tool_calls_this_turn > 0 {
                trace.event(
                    "agent.turn.tool_only_response",
                    serde_json::json!({
                        "turn": turn,
                        "tool_calls_this_turn": tool_calls_this_turn,
                        "policy": policy,
                    }),
                )?;
                final_summary = format!("turn {turn} used tools but produced no final text");
                if let Some(validation) = terminalize_after_validation.as_ref() {
                    trace.event(
                        "agent.validation.success_terminal_prompted",
                        serde_json::json!({
                            "turn": turn,
                            "tool_calls_this_turn": tool_calls_this_turn,
                            "validation": validation,
                            "requested_validation_commands": &requested_validation_commands,
                        }),
                    )?;
                    final_response_only_next_turn = true;
                    messages.push(LlmMessage::user(successful_validation_done_prompt(
                        validation,
                    )));
                    continue;
                }
                messages.push(LlmMessage::user(
                    "You used tools but produced no final text. Continue from the current project state. \
                     If validation passed, reply exactly DONE. If validation failed, fix the cause and validate again.",
                ));
                continue;
            }
            let empty_response_decision = EmptyResponseDecision {
                escalation_required: matches!(
                    turn_decision,
                    crate::runtime::RuntimeDecision::HardStopEmptyResponse
                ),
                prompt: empty_response_prompt(consecutive_empty_responses),
            };
            if thinking_chars_this_turn > 0 {
                trace.event(
                    "agent.turn.thinking_only_response",
                    serde_json::json!({
                        "turn": turn,
                        "thinking_chars_this_turn": thinking_chars_this_turn,
                        "consecutive_empty_responses": consecutive_empty_responses,
                    }),
                )?;
            }
            trace.event(
                "agent.turn.empty_response",
                serde_json::json!({
                    "turn": turn,
                    "thinking_chars_this_turn": thinking_chars_this_turn,
                    "consecutive_empty_responses": consecutive_empty_responses,
                    "escalation_required": empty_response_decision.escalation_required,
                }),
            )?;
            if empty_response_decision.escalation_required {
                trace.event(
                    "agent.turn.empty_response_escalated",
                    serde_json::json!({
                        "turn": turn,
                        "consecutive_empty_responses": consecutive_empty_responses,
                    }),
                )?;
                final_summary =
                    empty_response_hard_failure_summary(turn, consecutive_empty_responses);
                trace.event(
                    "agent.turn.empty_response_hard_failed",
                    serde_json::json!({
                        "turn": turn,
                        "consecutive_empty_responses": consecutive_empty_responses,
                        "thinking_chars_this_turn": thinking_chars_this_turn,
                        "final_summary": final_summary,
                    }),
                )?;
                exhausted_iterations = false;
                break;
            }
            messages.push(LlmMessage::user(empty_response_decision.prompt));
            continue;
        }
        final_summary = response.clone();
        messages.push(LlmMessage::assistant(response));
        if policy.validation_required_after_write {
            trace.event(
                "agent.validation.required_after_edit",
                serde_json::json!({
                    "turn": turn,
                    "tool_calls_this_turn": tool_calls_this_turn,
                    "policy": policy,
                }),
            )?;
            messages.push(LlmMessage::user(
                crate::profile::select_profile().post_write_validation_nudge(true),
            ));
            continue;
        }
        if is_done_response(&final_summary)
            && !scope.runtime_state_snapshot().requested_probes_satisfied()
        {
            trace.event(
                "agent.validation.done_rejected",
                serde_json::json!({
                    "turn": turn,
                    "response": final_summary,
                    "ledger": requested_validation_ledger,
                }),
            )?;
            messages.push(LlmMessage::user(done_rejected_prompt(
                &requested_validation_ledger,
            )));
            continue;
        }
        if is_terminal_response(&final_summary) {
            if let Some(token) = crate::runtime::terminal_token(&final_summary) {
                scope.observe_runtime(crate::runtime::RuntimeEvent::TerminalToken { token });
            }
            exhausted_iterations = false;
            break;
        }
        if let Some(validation) = terminalize_after_validation.as_ref() {
            trace.event(
                "agent.validation.success_terminal_prompted",
                serde_json::json!({
                    "turn": turn,
                    "tool_calls_this_turn": tool_calls_this_turn,
                    "validation": validation,
                    "requested_validation_commands": &requested_validation_commands,
                }),
            )?;
            final_response_only_next_turn = true;
            messages.push(LlmMessage::user(successful_validation_done_prompt(
                validation,
            )));
            continue;
        }
        if let Some(repair) = &policy.validation_repair
            && should_prompt_validation_repair(&policy, &final_summary)
        {
            trace.event(
                "agent.validation.repair_prompted",
                serde_json::json!({
                    "turn": turn,
                    "tool_calls_this_turn": tool_calls_this_turn,
                    "policy": policy,
                }),
            )?;
            if let Some(decision) = &repair_no_action {
                messages.push(LlmMessage::user(validation_repair_no_action_prompt(
                    decision,
                )));
            } else {
                messages.push(LlmMessage::user(validation_repair_prompt(repair)));
            }
            continue;
        }
        messages.push(LlmMessage::user(
            "Continue from the current experiment state. Use tools as needed. \
             Run deterministic validation before replying DONE. Reply FAIL only if blocked.",
        ));
    }

    if exhausted_iterations {
        let final_policy = scope.policy_snapshot();
        if final_policy.validation_required_after_write {
            trace.event(
                "agent.validation.required_after_edit_at_max_iterations",
                serde_json::json!({
                    "max_iterations": config.max_iterations,
                    "policy": final_policy,
                    "final_summary": final_summary,
                }),
            )?;
        }
    }
    scope.observe_runtime(crate::runtime::RuntimeEvent::RunFinished);

    let summary = AgentRunSummary {
        experiment_dir,
        tool_root,
        goal_file,
        trace_file: trace.path().to_path_buf(),
        model: config.model,
        max_iterations: config.max_iterations,
        max_tool_iterations: config.max_tool_iterations,
        context_window_tokens: config.context_window_tokens,
        packet_type: config.packet_type,
        expected_output_tokens: config.expected_output_tokens,
        num_predict: config.num_predict,
        max_thinking_only_tokens: config.max_thinking_only_tokens,
        repair_exit_thinking_tokens: config.repair_exit_thinking_tokens,
        action_boundary_interrupt_tokens: config.action_boundary_interrupt_tokens,
        transcript_policy: config.transcript_policy,
        final_summary,
        harness_source_state,
    };
    trace.event("run.finished", &summary)?;
    Ok(summary)
}

fn trace_run_failed(
    trace: &TraceRecorder,
    phase: &str,
    turn: Option<usize>,
    error: &anyhow::Error,
    config: &AgentRunConfig,
    tool_root: &Path,
    goal_file: &Path,
) -> Result<()> {
    trace.event(
        "run.failed",
        serde_json::json!({
            "phase": phase,
            "turn": turn,
            "error": error.to_string(),
            "model": &config.model,
            "experiment_dir": &config.experiment_dir,
            "tool_root": tool_root,
            "goal_file": goal_file,
        }),
    )
}

fn is_terminal_response(response: &str) -> bool {
    is_fail_response(response)
        || response
            .lines()
            .map(str::trim)
            .any(|line| line.eq_ignore_ascii_case("DONE"))
}

fn is_done_response(response: &str) -> bool {
    crate::runtime::terminal_token(response) == Some(crate::runtime::TerminalToken::Done)
}

fn is_fail_response(response: &str) -> bool {
    crate::runtime::terminal_token(response) == Some(crate::runtime::TerminalToken::Fail)
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RequestedValidationLedger {
    generation: usize,
    entries: Vec<RequestedValidationEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RequestedValidationEntry {
    command: String,
    status: RequestedValidationStatus,
    observed_command: Option<String>,
    status_code: Option<i32>,
    generation: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RequestedValidationStatus {
    Pending,
    Passed,
    Failed,
    Stale,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RequestedValidationObservation {
    command: String,
    matched_requested_command: Option<String>,
    status: RequestedValidationStatus,
    status_code: Option<i32>,
    generation: usize,
    source_mutation: bool,
}

impl RequestedValidationLedger {
    fn new(commands: Vec<String>) -> Self {
        Self {
            generation: 0,
            entries: commands
                .into_iter()
                .map(|command| RequestedValidationEntry {
                    command,
                    status: RequestedValidationStatus::Pending,
                    observed_command: None,
                    status_code: None,
                    generation: None,
                })
                .collect(),
        }
    }

    fn note_source_mutation(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.generation += 1;
        for entry in &mut self.entries {
            if entry.status == RequestedValidationStatus::Passed {
                entry.status = RequestedValidationStatus::Stale;
            }
        }
    }

    fn observe_tool_result(
        &mut self,
        result: &ToolCallRunResult,
    ) -> Option<RequestedValidationObservation> {
        let value = serde_json::from_str::<serde_json::Value>(&result.content).ok()?;
        let source_mutation = value
            .get("shell_mutation_requires_validation")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        if source_mutation {
            self.note_source_mutation();
        }
        if value
            .get("validation_probe")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        {
            return source_mutation.then(|| RequestedValidationObservation {
                command: value
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("shell command")
                    .to_string(),
                matched_requested_command: None,
                status: RequestedValidationStatus::Stale,
                status_code: value
                    .get("status")
                    .and_then(serde_json::Value::as_i64)
                    .and_then(|status| i32::try_from(status).ok()),
                generation: self.generation,
                source_mutation,
            });
        }

        let command = value
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("validation command")
            .to_string();
        let status_code = value
            .get("status")
            .and_then(serde_json::Value::as_i64)
            .and_then(|status| i32::try_from(status).ok());
        let matched_index = self.entries.iter().position(|entry| {
            validation_matches_requested_command(&command, std::slice::from_ref(&entry.command))
        });
        let Some(index) = matched_index else {
            return Some(RequestedValidationObservation {
                command,
                matched_requested_command: None,
                status: if result.ok && !source_mutation {
                    RequestedValidationStatus::Passed
                } else {
                    RequestedValidationStatus::Failed
                },
                status_code,
                generation: self.generation,
                source_mutation,
            });
        };

        let status = if result.ok
            && value.get("success").and_then(serde_json::Value::as_bool) == Some(true)
            && !source_mutation
        {
            RequestedValidationStatus::Passed
        } else {
            RequestedValidationStatus::Failed
        };
        let matched_requested_command = self.entries[index].command.clone();
        self.entries[index].status = status;
        self.entries[index].observed_command = Some(command.clone());
        self.entries[index].status_code = status_code;
        self.entries[index].generation = Some(self.generation);

        Some(RequestedValidationObservation {
            command,
            matched_requested_command: Some(matched_requested_command),
            status,
            status_code,
            generation: self.generation,
            source_mutation,
        })
    }

    fn is_satisfied(&self) -> bool {
        !self.entries.is_empty()
            && self.entries.iter().all(|entry| {
                entry.status == RequestedValidationStatus::Passed
                    && entry.generation == Some(self.generation)
            })
    }

    fn latest_successful_validation(&self) -> Option<SuccessfulValidationSnapshot> {
        self.entries
            .iter()
            .rev()
            .find(|entry| {
                entry.status == RequestedValidationStatus::Passed
                    && entry.generation == Some(self.generation)
            })
            .map(|entry| SuccessfulValidationSnapshot {
                command: entry
                    .observed_command
                    .clone()
                    .unwrap_or_else(|| entry.command.clone()),
                command_family: crate::profile::coding::validation_command_family(&entry.command),
                status: entry.status_code,
                total_shell_probes: 0,
                total_write_operations: self.generation,
            })
    }

    fn incomplete_entries(&self) -> Vec<&RequestedValidationEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.status != RequestedValidationStatus::Passed
                    || entry.generation != Some(self.generation)
            })
            .collect()
    }
}

fn should_prompt_validation_repair(policy: &ToolPolicySnapshot, response: &str) -> bool {
    policy.validation_repair.is_some() && !is_terminal_response(response)
}

#[cfg(test)]
fn should_terminalize_after_successful_validation(
    before: &ToolPolicySnapshot,
    after: &ToolPolicySnapshot,
    requested_validation_commands: &[String],
) -> Option<SuccessfulValidationSnapshot> {
    let validation = after.latest_successful_validation_after_write.as_ref()?;
    if validation.total_shell_probes <= before.total_shell_probes {
        return None;
    }
    if after.validation_required_after_write || after.validation_repair.is_some() {
        return None;
    }
    if !validation_matches_requested_command(&validation.command, requested_validation_commands) {
        return None;
    }
    Some(validation.clone())
}

pub(crate) fn validation_matches_requested_command(command: &str, requested: &[String]) -> bool {
    if validation_command_masks_failure(command) {
        return false;
    }
    if requested.is_empty() {
        return true;
    }
    let actual = normalize_validation_command(command);
    requested
        .iter()
        .any(|expected| actual == *expected || actual.starts_with(&format!("{expected} ")))
}

pub(crate) fn validation_command_masks_failure(command: &str) -> bool {
    let spaced = command
        .replace("||", " || ")
        .replace("&&", " && ")
        .replace(';', " ; ");
    let parts = spaced.split_whitespace().collect::<Vec<_>>();
    parts.iter().enumerate().any(|(index, part)| {
        let part = part.to_ascii_lowercase();
        let next = parts.get(index + 1).map(|value| value.to_ascii_lowercase());
        let after_next = parts.get(index + 2).map(|value| value.to_ascii_lowercase());
        match part.as_str() {
            "||" => {
                matches!(next.as_deref(), Some("true" | ":"))
                    || (matches!(next.as_deref(), Some("exit"))
                        && matches!(after_next.as_deref(), Some("0")))
            }
            ";" => {
                matches!(next.as_deref(), Some("true"))
                    || (matches!(next.as_deref(), Some("exit"))
                        && matches!(after_next.as_deref(), Some("0")))
            }
            _ => false,
        }
    })
}

pub(crate) fn normalize_validation_command(command: &str) -> String {
    let command = command
        .split_once('|')
        .map(|(head, _)| head)
        .unwrap_or(command)
        .trim();
    let mut parts = command.split_whitespace().collect::<Vec<_>>();
    while parts.last().is_some_and(|part| {
        matches!(
            *part,
            "2>&1" | "1>&2" | ">/dev/null" | "1>/dev/null" | "2>/dev/null"
        )
    }) {
        parts.pop();
    }
    parts.join(" ")
}

fn successful_validation_done_prompt(validation: &SuccessfulValidationSnapshot) -> String {
    format!(
        "The requested validation command has passed after the source edits.\n\
         Passing command: {command}\n\
         Command family: {command_family}\n\
         Do not call any more tools. Do not run broader validation or formatting cleanup. \
         Reply exactly DONE if the requested task is complete, or exactly FAIL with one concise blocker.",
        command = validation.command,
        command_family = validation.command_family,
    )
}

fn done_rejected_prompt(ledger: &RequestedValidationLedger) -> String {
    let incomplete = ledger
        .incomplete_entries()
        .into_iter()
        .map(|entry| {
            let status = match entry.status {
                RequestedValidationStatus::Pending => "missing",
                RequestedValidationStatus::Passed => "stale",
                RequestedValidationStatus::Failed => "failed",
                RequestedValidationStatus::Stale => "stale",
            };
            format!("- {} ({status})", entry.command)
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "DONE is not accepted yet. Required validation is incomplete for the \
         latest source state:\n{incomplete}\n\
         Run only the missing, stale, or failed validation commands above. \
         Reply DONE only after every listed command passes after the latest \
         source mutation. Reply FAIL only if blocked."
    )
}

fn repair_no_action_failure_summary(turn: usize) -> String {
    format!("turn {turn} made no validation-repair edit or probe after validation failure")
}

fn empty_response_hard_failure_summary(turn: usize, consecutive_empty_responses: usize) -> String {
    format!(
        "turn {turn} produced {consecutive_empty_responses} consecutive empty responses with no tool calls or final text"
    )
}

fn hidden_only_no_action_hard_failure_summary(
    turn: usize,
    consecutive_hidden_only_no_action_turns: usize,
) -> String {
    format!(
        "turn {turn} produced {consecutive_hidden_only_no_action_turns} consecutive hidden-only no-action responses without source mutation, validation probe, or final text"
    )
}

fn action_boundary_no_action_failure_summary(decision: &ActionBoundaryNoActionDecision) -> String {
    format!(
        "turn {} produced {} consecutive action-boundary interrupts without source mutation or validation probe",
        decision.turn, decision.consecutive_no_action_turns
    )
}

fn repair_hard_failure_summary(decision: &RepairNoActionDecision) -> String {
    match decision.reason {
        RepairNoActionReason::NoRepairAction => repair_no_action_failure_summary(decision.turn),
        RepairNoActionReason::NoContentInterrupted => format!(
            "turn {} validation repair produced no content or tool call after repeated interrupts",
            decision.turn
        ),
    }
}

fn repair_depth_failure_summary(decision: &RepairDepthDecision) -> String {
    match decision.reason {
        RepairDepthReason::MaxLlmCallDepth => format!(
            "turn {} validation repair exceeded in-turn LLM call depth limit {}",
            decision.turn, decision.limit
        ),
        RepairDepthReason::RedContextAfterRepairAction => format!(
            "turn {} validation repair reached red context pressure after in-turn repair action",
            decision.turn
        ),
    }
}

fn canonicalize_goal(experiment_dir: &Path, goal_file: &Path) -> Result<PathBuf> {
    let candidate = if goal_file.is_absolute() {
        goal_file.to_path_buf()
    } else {
        experiment_dir.join(goal_file)
    };
    let canonical = candidate
        .canonicalize()
        .with_context(|| format!("canonicalizing goal file {}", candidate.display()))?;
    if !canonical.starts_with(experiment_dir) {
        anyhow::bail!("goal file must be inside experiment dir");
    }
    Ok(canonical)
}

struct StreamResponseRequest<'a, G: LlmGateway + ?Sized> {
    gateway: &'a G,
    model: &'a str,
    messages: &'a [LlmMessage],
    tools: &'a [Box<dyn LlmTool>],
    completion_config: CompletionConfig,
    context_window_tokens: Option<usize>,
    packet_type: &'a str,
    expected_output_tokens: usize,
    max_thinking_only_tokens: usize,
    repair_exit_thinking_tokens: usize,
    action_boundary_interrupt_tokens: usize,
    validation_repair_active: bool,
    transcript_policy: TranscriptPolicy,
    throughput_registry_path: PathBuf,
    progress_projection_override: Option<ModelProgressProjection>,
    progress_status_interval_override: Option<Duration>,
    runner_activity_override: Option<RunnerActivitySample>,
    trace: &'a TraceRecorder,
    turn: usize,
    requested_validation_commands: &'a [String],
    requested_validation_pending_after_write: bool,
    requested_validation_ledger: RequestedValidationLedger,
}

#[derive(Debug)]
struct StreamResponseResult {
    response: String,
    messages: Vec<LlmMessage>,
    thinking_chars: usize,
    repair_no_content_interrupted: bool,
    action_boundary_interrupted: Option<ActionBoundaryInterrupt>,
    repair_depth_hard_stop: Option<RepairDepthDecision>,
    requested_validation_ledger: RequestedValidationLedger,
}

#[derive(Debug, Clone, Serialize)]
struct ActionBoundaryInterrupt {
    turn: usize,
    llm_call_depth: usize,
    call_thinking_chars: usize,
    call_thinking_estimated_tokens: usize,
    action_boundary_interrupt_tokens: usize,
    action_intent_hits: usize,
    hit_limit: usize,
    latest_preview: String,
}

#[derive(Debug, Clone, Serialize)]
struct ActionBoundaryNoActionDecision {
    turn: usize,
    tool_calls_this_turn: usize,
    consecutive_no_action_turns: usize,
    escalation_required: bool,
    interrupt: ActionBoundaryInterrupt,
    total_write_operations_before_turn: usize,
    total_write_operations_after_turn: usize,
    total_shell_probes_before_turn: usize,
    total_shell_probes_after_turn: usize,
}

#[derive(Debug, Clone, Serialize)]
struct RepairNoActionDecision {
    turn: usize,
    tool_calls_this_turn: usize,
    reason: RepairNoActionReason,
    consecutive_no_action_turns: usize,
    escalation_required: bool,
    active_repair: ValidationRepairSnapshot,
    validation_repair_read_paths: BTreeMap<String, usize>,
    total_write_operations_before_turn: usize,
    total_write_operations_after_turn: usize,
    total_shell_probes_before_turn: usize,
    total_shell_probes_after_turn: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RepairNoActionReason {
    NoRepairAction,
    NoContentInterrupted,
}

#[derive(Debug, Clone, Serialize)]
struct RepairDepthDecision {
    turn: usize,
    llm_call_depth: usize,
    reason: RepairDepthReason,
    limit: usize,
    estimated_tokens: usize,
    context_window_tokens: Option<usize>,
    utilization: Option<f64>,
    pressure_band: &'static str,
    message_count: usize,
    max_tool_iterations: usize,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum RepairDepthReason {
    MaxLlmCallDepth,
    RedContextAfterRepairAction,
}

async fn stream_response<G: LlmGateway + ?Sized>(
    request: StreamResponseRequest<'_, G>,
) -> Result<StreamResponseResult> {
    let StreamResponseRequest {
        gateway,
        model,
        messages,
        tools,
        completion_config,
        context_window_tokens,
        packet_type,
        expected_output_tokens,
        max_thinking_only_tokens,
        repair_exit_thinking_tokens,
        action_boundary_interrupt_tokens,
        validation_repair_active,
        transcript_policy,
        throughput_registry_path,
        progress_projection_override,
        progress_status_interval_override,
        runner_activity_override,
        trace,
        turn,
        requested_validation_commands,
        requested_validation_pending_after_write,
        requested_validation_ledger,
    } = request;
    trace.event(
        "agent.stream.started",
        serde_json::json!({
            "turn": turn,
        }),
    )?;
    let mut current_messages = messages.to_vec();
    let mut response = String::new();
    let mut chunk_count = 0usize;
    let mut content_chars = 0usize;
    let mut thinking_chunk_count = 0usize;
    let mut thinking_chars = 0usize;
    let mut stream_progress_frame_count = 0usize;
    let mut tool_call_progress_frame_count = 0usize;
    let mut no_content_segment_eval_count = 0usize;
    let mut repair_no_content_interrupted = false;
    let mut action_boundary_interrupted = None;
    let no_assistant_content_limit =
        expected_output_tokens.saturating_mul(NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER);
    let mut inspection_loop_tracker = InspectionLoopTracker::default();
    let mut final_response_only_after_validation: Option<SuccessfulValidationSnapshot> = None;
    let mut requested_validation_pending_after_write = requested_validation_pending_after_write;
    let mut requested_validation_ledger = requested_validation_ledger;
    let no_tools: Vec<Box<dyn LlmTool>> = Vec::new();
    let correlation_id = format!(
        "harness-turn-{turn}-{}",
        chrono::Utc::now().timestamp_millis()
    );
    let mut previous_call_total_chars = None;

    for depth in 0..=completion_config.max_tool_iterations {
        if depth >= completion_config.max_tool_iterations {
            return Err(MojenticError::MaxToolIterationsExceeded {
                limit: completion_config.max_tool_iterations,
            }
            .into());
        }

        let active_tools = if final_response_only_after_validation.is_some() {
            &no_tools
        } else {
            tools
        };
        let ledger = context_assembly_ledger(ContextAssemblyInput {
            model,
            turn,
            llm_call_depth: depth,
            messages: &current_messages,
            tools: active_tools,
            completion_config: &completion_config,
            context_window_tokens,
            previous_call_total_chars,
            transcript_policy,
        });
        previous_call_total_chars = ledger.total_chars();
        trace.event("llm.context_assembly.ledger", &ledger)?;
        if let Some(decision) = repair_depth_decision(validation_repair_active, &ledger) {
            trace.event("agent.validation.repair_depth_hard_failed", &decision)?;
            trace.event(
                "agent.stream.finished",
                serde_json::json!({
                    "turn": turn,
                    "chunks": chunk_count,
                    "chars": content_chars,
                    "thinking_chunks": thinking_chunk_count,
                    "thinking_chars": thinking_chars,
                    "llm_call_count": depth,
                    "repair_depth_hard_stop": &decision,
                }),
            )?;
            return Ok(StreamResponseResult {
                response,
                messages: current_messages,
                thinking_chars,
                repair_no_content_interrupted,
                action_boundary_interrupted,
                repair_depth_hard_stop: Some(decision),
                requested_validation_ledger,
            });
        }
        let projection = progress_projection_override.clone().unwrap_or_else(|| {
            model_progress_projection(
                model,
                expected_output_tokens,
                ledger.pressure_band,
                &throughput_registry_path,
            )
        });
        trace.event(
            "llm.stream.projection",
            serde_json::json!({
                "turn": turn,
                "llm_call_depth": depth,
                "model": model,
                "packet_type": packet_type,
                "expected_output_tokens": expected_output_tokens,
                "context_band": ledger.pressure_band,
                "warmup_seconds": MODEL_PROGRESS_WARMUP_SECONDS,
                "tool_call_json_slack_seconds": MODEL_PROGRESS_TOOL_JSON_SLACK_SECONDS,
                "variance_multiplier": MODEL_PROGRESS_VARIANCE_MULTIPLIER,
                "conservative_tokens_per_second": projection.conservative_tokens_per_second,
                "throughput_sample_count": projection.sample_count,
                "expected_max_seconds": projection.expected_max_seconds,
                "allowed_seconds": projection.allowed_seconds,
                "initial_progress_state": "WaitingForFirstToken",
            }),
        )?;

        let mut call_content = String::new();
        let mut accumulated_tool_calls = Vec::new();
        let started = Instant::now();
        let mut latest_progress_state = ModelProgressState::WaitingForFirstToken;
        let mut last_observable_progress = started;
        let mut stalled_candidate_checks = 0usize;
        let mut call_stream_progress_frame_count = 0usize;
        let mut call_tool_call_progress_frame_count = 0usize;
        let mut call_thinking_chunk_count = 0usize;
        let mut call_thinking_chars = 0usize;
        let mut call_action_intent_hits = 0usize;
        let mut call_action_intent_buffer = String::new();
        let mut last_action_intent_hit_token: Option<usize> = None;
        trace_model_progress_status(ModelProgressStatusInput {
            trace,
            turn,
            llm_call_depth: depth,
            model,
            progress_state: latest_progress_state,
            elapsed: started.elapsed(),
            seconds_since_observable_progress: 0.0,
            projection: &projection,
            runner_activity: None,
            stalled_candidate_checks,
            automatic_interrupt: false,
        })?;
        let mut stream = gateway.complete_stream(
            model,
            &current_messages,
            Some(active_tools),
            &completion_config,
        );
        let mut status_interval = tokio::time::interval(
            progress_status_interval_override.unwrap_or_else(progress_status_interval),
        );
        status_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        status_interval.tick().await;

        loop {
            let chunk_result = tokio::select! {
                chunk_result = stream.next() => chunk_result,
                _ = status_interval.tick() => {
                    let runner_activity = if let Some(sample) = &runner_activity_override {
                        sample.clone()
                    } else {
                        sample_runner_activity(model).await
                    };
                    latest_progress_state = classify_model_progress(
                        latest_progress_state,
                        started.elapsed(),
                        last_observable_progress.elapsed(),
                        projection.allowed_seconds,
                        Some(&runner_activity),
                        stalled_candidate_checks,
                    );
                    if latest_progress_state == ModelProgressState::PossiblyStalled {
                        stalled_candidate_checks += 1;
                        latest_progress_state = classify_model_progress(
                            latest_progress_state,
                            started.elapsed(),
                            last_observable_progress.elapsed(),
                            projection.allowed_seconds,
                            Some(&runner_activity),
                            stalled_candidate_checks,
                        );
                    } else if latest_progress_state.has_progress_evidence() {
                        stalled_candidate_checks = 0;
                    }
                    let automatic_interrupt =
                        latest_progress_state == ModelProgressState::Stalled;
                    trace_model_progress_status(ModelProgressStatusInput {
                        trace,
                        turn,
                        llm_call_depth: depth,
                        model,
                        progress_state: latest_progress_state,
                        elapsed: started.elapsed(),
                        seconds_since_observable_progress: last_observable_progress
                            .elapsed()
                            .as_secs_f64(),
                        projection: &projection,
                        runner_activity: Some(&runner_activity),
                        stalled_candidate_checks,
                        automatic_interrupt,
                    })?;
                    if automatic_interrupt {
                        trace.event(
                            "llm.progress.interrupted",
                            serde_json::json!({
                                "turn": turn,
                                "llm_call_depth": depth,
                                "model": model,
                                "progress_state": latest_progress_state.as_str(),
                                "elapsed_seconds": started.elapsed().as_secs_f64(),
                                "allowed_seconds": projection.allowed_seconds,
                                "seconds_since_observable_progress": last_observable_progress
                                    .elapsed()
                                    .as_secs_f64(),
                                "stalled_candidate_checks": stalled_candidate_checks,
                                "runner_activity": &runner_activity,
                            }),
                        )?;
                        anyhow::bail!(
                            "interrupted stalled model call after {:.3}s; projected allowance was {:.3}s",
                            started.elapsed().as_secs_f64(),
                            projection.allowed_seconds
                        );
                    }
                    continue;
                }
            };
            let Some(chunk_result) = chunk_result else {
                break;
            };
            match chunk_result {
                Ok(StreamChunk::Content(chunk)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    latest_progress_state = ModelProgressState::Generating;
                    no_content_segment_eval_count = 0;
                    chunk_count += 1;
                    content_chars += chunk.len();
                    if call_content.is_empty() && !response.is_empty() && !response.ends_with('\n')
                    {
                        response.push('\n');
                    }
                    call_content.push_str(&chunk);
                    response.push_str(&chunk);
                    trace.event(
                        "agent.stream.chunk",
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "chunk": chunk_count,
                            "chars": chunk.len(),
                            "total_chars": content_chars,
                            "preview": limit_preview(&chunk, 120),
                        }),
                    )?;
                    eprint!("{}", chunk);
                }
                Ok(StreamChunk::ToolCalls(tool_calls)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    latest_progress_state = ModelProgressState::GeneratingToolCall;
                    no_content_segment_eval_count = 0;
                    trace.event(
                        "llm.stream.tool_calls_completed",
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "tool_call_count": tool_calls.len(),
                            "tool_call_names": tool_calls.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        }),
                    )?;
                    accumulated_tool_calls = tool_calls;
                }
                Ok(StreamChunk::Thinking(thinking)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    latest_progress_state = ModelProgressState::Generating;
                    no_content_segment_eval_count = 0;
                    thinking_chunk_count += 1;
                    thinking_chars += thinking.len();
                    call_thinking_chunk_count += 1;
                    call_thinking_chars += thinking.len();
                    push_bounded_buffer(
                        &mut call_action_intent_buffer,
                        &thinking,
                        ACTION_BOUNDARY_INTENT_BUFFER_CHARS,
                    );
                    trace.event(
                        "llm.stream.thinking",
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "chunk": thinking_chunk_count,
                            "call_chunk": call_thinking_chunk_count,
                            "chars": thinking.len(),
                            "total_thinking_chars": thinking_chars,
                            "call_thinking_chars": call_thinking_chars,
                            "preview": limit_preview(&thinking, 240),
                        }),
                    )?;
                    let call_thinking_estimated_tokens = estimate_tokens(call_thinking_chars);
                    if action_intent_signal(&call_action_intent_buffer)
                        && last_action_intent_hit_token.is_none_or(|last_hit_token| {
                            call_thinking_estimated_tokens
                                >= last_hit_token + ACTION_BOUNDARY_INTENT_HIT_GAP_TOKENS
                        })
                    {
                        call_action_intent_hits += 1;
                        last_action_intent_hit_token = Some(call_thinking_estimated_tokens);
                    }
                    if !validation_repair_active
                        && action_boundary_interrupt_tokens > 0
                        && call_content.is_empty()
                        && accumulated_tool_calls.is_empty()
                        && call_tool_call_progress_frame_count == 0
                        && call_action_intent_hits >= ACTION_BOUNDARY_INTENT_HIT_LIMIT
                        && call_thinking_estimated_tokens > action_boundary_interrupt_tokens
                    {
                        let interrupt = ActionBoundaryInterrupt {
                            turn,
                            llm_call_depth: depth,
                            call_thinking_chars,
                            call_thinking_estimated_tokens,
                            action_boundary_interrupt_tokens,
                            action_intent_hits: call_action_intent_hits,
                            hit_limit: ACTION_BOUNDARY_INTENT_HIT_LIMIT,
                            latest_preview: limit_preview(&call_action_intent_buffer, 240),
                        };
                        trace.event("agent.action_boundary.interrupted", &interrupt)?;
                        action_boundary_interrupted = Some(interrupt);
                        break;
                    }
                    if max_thinking_only_tokens > 0
                        && call_content.is_empty()
                        && accumulated_tool_calls.is_empty()
                        && call_tool_call_progress_frame_count == 0
                        && call_thinking_estimated_tokens > max_thinking_only_tokens
                    {
                        trace.event(
                            "llm.thinking_only_stream.hard_failed",
                            serde_json::json!({
                                "turn": turn,
                                "llm_call_depth": depth,
                                "call_thinking_chars": call_thinking_chars,
                                "call_thinking_estimated_tokens": call_thinking_estimated_tokens,
                                "max_thinking_only_tokens": max_thinking_only_tokens,
                                "expected_output_tokens": expected_output_tokens,
                                "thinking_chunk_count": call_thinking_chunk_count,
                                "content_chars": call_content.len(),
                                "tool_call_count": accumulated_tool_calls.len(),
                                "call_tool_call_progress_frame_count": call_tool_call_progress_frame_count,
                                "latest_preview": limit_preview(&thinking, 240),
                            }),
                        )?;
                        anyhow::bail!(
                            "thinking-only stream exceeded {max_thinking_only_tokens} estimated tokens without assistant content or tool calls"
                        );
                    }
                    if validation_repair_active
                        && repair_exit_thinking_tokens > 0
                        && call_content.is_empty()
                        && accumulated_tool_calls.is_empty()
                        && call_tool_call_progress_frame_count == 0
                        && call_thinking_estimated_tokens > repair_exit_thinking_tokens
                    {
                        repair_no_content_interrupted = true;
                        trace.event(
                            "agent.validation.repair_exit_interrupted",
                            serde_json::json!({
                                "turn": turn,
                                "llm_call_depth": depth,
                                "call_thinking_chars": call_thinking_chars,
                                "call_thinking_estimated_tokens": call_thinking_estimated_tokens,
                                "repair_exit_thinking_tokens": repair_exit_thinking_tokens,
                                "max_thinking_only_tokens": max_thinking_only_tokens,
                                "expected_output_tokens": expected_output_tokens,
                                "thinking_chunk_count": call_thinking_chunk_count,
                                "content_chars": call_content.len(),
                                "tool_call_count": accumulated_tool_calls.len(),
                                "call_tool_call_progress_frame_count": call_tool_call_progress_frame_count,
                                "latest_preview": limit_preview(&thinking, 240),
                            }),
                        )?;
                        break;
                    }
                }
                Ok(StreamChunk::Progress(progress)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    stream_progress_frame_count += 1;
                    call_stream_progress_frame_count += 1;
                    let progress_has_tool_call =
                        progress.tool_call_count > 0 || progress.accumulated_tool_call_count > 0;
                    let progress_has_content = progress.content_chars > 0;
                    let progress_has_thinking = progress.thinking_chars > 0;
                    if progress.tool_call_count > 0 || progress.accumulated_tool_call_count > 0 {
                        tool_call_progress_frame_count += 1;
                        call_tool_call_progress_frame_count += 1;
                        latest_progress_state = ModelProgressState::GeneratingToolCall;
                    } else if progress.content_chars > 0 || progress.thinking_chars > 0 {
                        latest_progress_state = ModelProgressState::Generating;
                    }
                    trace.event(
                        "llm.stream.progress",
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "provider": progress.provider,
                            "frame_index": progress.frame_index,
                            "done": progress.done,
                            "content_chars": progress.content_chars,
                            "thinking_chars": progress.thinking_chars,
                            "tool_call_count": progress.tool_call_count,
                            "accumulated_tool_call_count": progress.accumulated_tool_call_count,
                            "stream_progress_frame_count": stream_progress_frame_count,
                            "call_stream_progress_frame_count": call_stream_progress_frame_count,
                            "tool_call_progress_frame_count": tool_call_progress_frame_count,
                            "call_tool_call_progress_frame_count": call_tool_call_progress_frame_count,
                            "progress_state": if progress.done { "DoneFrame" } else { latest_progress_state.as_str() },
                        }),
                    )?;
                    if validation_repair_active
                        && !progress_has_content
                        && !progress_has_thinking
                        && !progress_has_tool_call
                        && call_content.is_empty()
                        && call_thinking_chars == 0
                        && accumulated_tool_calls.is_empty()
                        && call_stream_progress_frame_count
                            >= REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT
                    {
                        repair_no_content_interrupted = true;
                        trace.event(
                            "agent.validation.repair_no_content_interrupted",
                            serde_json::json!({
                                "turn": turn,
                                "llm_call_depth": depth,
                                "expected_output_tokens": expected_output_tokens,
                                "progress_frame_limit": REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT,
                                "call_stream_progress_frame_count": call_stream_progress_frame_count,
                                "call_tool_call_progress_frame_count": call_tool_call_progress_frame_count,
                                "stream_progress_frame_count": stream_progress_frame_count,
                                "tool_call_progress_frame_count": tool_call_progress_frame_count,
                                "turn_content_chars": content_chars,
                                "call_content_chars": call_content.len(),
                                "turn_thinking_chars": thinking_chars,
                                "call_thinking_chars": call_thinking_chars,
                                "validation_repair_active": validation_repair_active,
                            }),
                        )?;
                        break;
                    }
                }
                Ok(StreamChunk::Metrics(metrics)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    if call_content.is_empty()
                        && call_thinking_chars == 0
                        && accumulated_tool_calls.is_empty()
                    {
                        no_content_segment_eval_count = no_content_segment_eval_count
                            .saturating_add(metrics.eval_count.unwrap_or_default() as usize);
                    }
                    trace.event(
                        "llm.stream.metrics",
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "provider": &metrics.provider,
                            "total_duration_ns": metrics.total_duration_ns,
                            "load_duration_ns": metrics.load_duration_ns,
                            "prompt_eval_count": metrics.prompt_eval_count,
                            "prompt_eval_duration_ns": metrics.prompt_eval_duration_ns,
                            "eval_count": metrics.eval_count,
                            "eval_duration_ns": metrics.eval_duration_ns,
                            "tokens_per_second": metrics.tokens_per_second,
                            "packet_type": packet_type,
                            "expected_output_tokens": expected_output_tokens,
                            "context_band": ledger.pressure_band,
                            "turn_thinking_chars": thinking_chars,
                            "call_thinking_chars": call_thinking_chars,
                        }),
                    )?;
                    if let Some(sample) = throughput_sample(
                        model,
                        packet_type,
                        expected_output_tokens,
                        ledger.pressure_band,
                        turn,
                        depth,
                        &metrics,
                    ) {
                        append_throughput_sample(&throughput_registry_path, &sample)?;
                        trace.event("llm.throughput.sample", &sample)?;
                    }
                    if call_content.is_empty()
                        && call_thinking_chars == 0
                        && accumulated_tool_calls.is_empty()
                        && no_assistant_content_limit > 0
                        && no_content_segment_eval_count > no_assistant_content_limit
                    {
                        trace.event(
                            "llm.no_content_stream.hard_failed",
                            serde_json::json!({
                                "turn": turn,
                                "llm_call_depth": depth,
                                "observed_output_tokens_without_assistant_content": no_content_segment_eval_count,
                                "expected_output_tokens": expected_output_tokens,
                                "multiplier": NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER,
                                "limit": no_assistant_content_limit,
                                "turn_content_chars": content_chars,
                                "call_content_chars": call_content.len(),
                                "turn_thinking_chars": thinking_chars,
                                "call_thinking_chars": call_thinking_chars,
                                "tool_call_progress_frame_count": tool_call_progress_frame_count,
                                "stream_progress_frame_count": stream_progress_frame_count,
                            }),
                        )?;
                        anyhow::bail!(
                            "no assistant content after {no_content_segment_eval_count} observed output tokens since the latest content or tool call; expected output budget was {expected_output_tokens}"
                        );
                    }
                }
                Err(error) => {
                    let _ = trace.event(
                        "agent.stream.failed",
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "chunks": chunk_count,
                            "chars": content_chars,
                            "error": error.to_string(),
                        }),
                    );
                    return Err(error.into());
                }
            }
        }
        drop(stream);

        trace.event(
            "llm.context_assembly.response",
            serde_json::json!({
                "turn": turn,
                "llm_call_depth": depth,
                "duration_ms": started.elapsed().as_millis(),
                "content_chars": call_content.len(),
                "content_estimated_tokens": estimate_tokens(call_content.len()),
                "thinking_chars": call_thinking_chars,
                "turn_thinking_chars": thinking_chars,
                "thinking_chunk_count": call_thinking_chunk_count,
                "stream_progress_frame_count": stream_progress_frame_count,
                "call_stream_progress_frame_count": call_stream_progress_frame_count,
                "tool_call_progress_frame_count": tool_call_progress_frame_count,
                "call_tool_call_progress_frame_count": call_tool_call_progress_frame_count,
                "final_progress_state": latest_progress_state.as_str(),
                "tool_call_count": accumulated_tool_calls.len(),
                "tool_call_names": accumulated_tool_calls.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
            }),
        )?;

        if accumulated_tool_calls.is_empty() {
            trace.event(
                "agent.stream.finished",
                serde_json::json!({
                    "turn": turn,
                    "chunks": chunk_count,
                    "chars": content_chars,
                    "thinking_chunks": thinking_chunk_count,
                    "thinking_chars": thinking_chars,
                    "llm_call_count": depth + 1,
                }),
            )?;
            return Ok(StreamResponseResult {
                response,
                messages: current_messages,
                thinking_chars,
                repair_no_content_interrupted,
                action_boundary_interrupted,
                repair_depth_hard_stop: None,
                requested_validation_ledger,
            });
        }

        let assistant_message = LlmMessage {
            role: MessageRole::Assistant,
            content: Some(call_content),
            tool_calls: Some(accumulated_tool_calls.clone()),
            image_paths: None,
        };
        let assistant_chars = message_chars(&assistant_message);
        current_messages.push(assistant_message);
        trace.event(
            "llm.context_assembly.appended",
            serde_json::json!({
                "turn": turn,
                "llm_call_depth": depth,
                "component": "assistant_tool_request",
                "reason": "retained model tool request so the next LLM call can see which tools were requested",
                "message_chars": assistant_chars,
                "estimated_tokens": estimate_tokens(assistant_chars),
                "message_count_after_append": current_messages.len(),
            }),
        )?;

        for call in &accumulated_tool_calls {
            let tool_result = run_tool_call(call, active_tools, &correlation_id).await;
            let tool_message = LlmMessage {
                role: MessageRole::Tool,
                content: Some(tool_result.content.clone()),
                tool_calls: Some(vec![call.clone()]),
                image_paths: None,
            };
            let tool_message_chars = message_chars(&tool_message);
            current_messages.push(tool_message);
            trace.event(
                "llm.context_assembly.appended",
                serde_json::json!({
                    "turn": turn,
                    "llm_call_depth": depth,
                    "component": "tool_result",
                    "reason": "retained raw tool result for the next in-turn LLM call",
                    "tool_call_id": &call.id,
                    "tool_name": &call.name,
                    "ok": tool_result.ok,
                    "duration_ms": tool_result.duration_ms,
                    "content_chars": tool_result.content.len(),
                    "content_estimated_tokens": estimate_tokens(tool_result.content.len()),
                    "message_chars": tool_message_chars,
                    "estimated_tokens": estimate_tokens(tool_message_chars),
                    "message_count_after_append": current_messages.len(),
                }),
            )?;
            if let Some(decision) = inspection_loop_tracker.observe(turn, depth, call, &tool_result)
            {
                trace.event("agent.inspection_loop.hard_failed", &decision)?;
                anyhow::bail!("{}", inspection_loop_failure_summary(&decision));
            }
            if is_meaningful_source_edit(call, &tool_result) {
                requested_validation_pending_after_write = true;
                requested_validation_ledger.note_source_mutation();
            }
            let requested_validation_observation =
                requested_validation_ledger.observe_tool_result(&tool_result);
            if let Some(observation) = &requested_validation_observation {
                if observation.source_mutation {
                    requested_validation_pending_after_write = true;
                }
                trace.event(
                    "agent.validation.ledger_observed",
                    serde_json::json!({
                        "turn": turn,
                        "llm_call_depth": depth,
                        "tool_call_id": &call.id,
                        "tool_name": &call.name,
                        "observation": observation,
                        "ledger": &requested_validation_ledger,
                    }),
                )?;
            }
            let successful_validation = successful_validation_from_tool_result(
                &tool_result,
                requested_validation_commands,
                requested_validation_pending_after_write,
            );
            let should_terminalize_for_validation = if requested_validation_commands.is_empty() {
                successful_validation.clone()
            } else if requested_validation_ledger.is_satisfied() {
                requested_validation_ledger.latest_successful_validation()
            } else {
                None
            };
            if final_response_only_after_validation.is_none()
                && let Some(validation) = should_terminalize_for_validation
            {
                trace.event(
                    "agent.validation.success_terminal_prompted",
                    serde_json::json!({
                        "turn": turn,
                        "llm_call_depth": depth,
                        "tool_call_id": &call.id,
                        "tool_name": &call.name,
                        "validation": &validation,
                        "requested_validation_commands": requested_validation_commands,
                        "scope": "in_turn",
                    }),
                )?;
                current_messages.push(LlmMessage::user(successful_validation_done_prompt(
                    &validation,
                )));
                final_response_only_after_validation = Some(validation);
                requested_validation_pending_after_write = false;
            }
        }
        compact_retained_tool_results(
            &mut current_messages,
            active_tools,
            trace,
            turn,
            depth,
            transcript_policy,
            context_window_tokens,
        )?;
    }

    unreachable!("tool iteration loop always returns or errors before exhaustion")
}

fn repair_depth_decision(
    validation_repair_active: bool,
    ledger: &ContextAssemblyLedger,
) -> Option<RepairDepthDecision> {
    if !validation_repair_active {
        return None;
    }

    let reason = if ledger.llm_call_depth >= MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH {
        Some(RepairDepthReason::MaxLlmCallDepth)
    } else if ledger.llm_call_depth > 0 && ledger.pressure_band == "red" {
        Some(RepairDepthReason::RedContextAfterRepairAction)
    } else {
        None
    }?;
    let runtime_reason = match reason {
        RepairDepthReason::MaxLlmCallDepth => crate::runtime::RepairDepthReason::MaxLlmCallDepth,
        RepairDepthReason::RedContextAfterRepairAction => {
            crate::runtime::RepairDepthReason::RedContextAfterRepairAction
        }
    };
    let event = crate::runtime::RuntimeEvent::RepairDepthExceeded {
        turn: ledger.turn,
        reason: runtime_reason,
    };
    if !matches!(
        crate::runtime::RuntimePolicy.decide(&crate::runtime::RuntimeState::default(), &event),
        crate::runtime::RuntimeDecision::HardStopRepairDepth { .. }
    ) {
        return None;
    }

    Some(RepairDepthDecision {
        turn: ledger.turn,
        llm_call_depth: ledger.llm_call_depth,
        reason,
        limit: MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH,
        estimated_tokens: ledger.estimated_tokens,
        context_window_tokens: ledger.context_window_tokens,
        utilization: ledger.utilization,
        pressure_band: ledger.pressure_band,
        message_count: ledger.message_count,
        max_tool_iterations: ledger.max_tool_iterations,
    })
}

#[derive(Debug, Clone, Serialize)]
struct ContextAssemblyLedger {
    model: String,
    turn: usize,
    llm_call_depth: usize,
    message_count: usize,
    components: Vec<ContextComponentLedger>,
    role_counts: BTreeMap<String, usize>,
    role_chars: BTreeMap<String, usize>,
    message_chars: usize,
    tool_descriptor_chars: usize,
    total_chars: usize,
    estimated_tokens: usize,
    context_window_tokens: Option<usize>,
    utilization: Option<f64>,
    pressure_band: &'static str,
    previous_call_total_chars: Option<usize>,
    delta_chars_from_previous_call: Option<isize>,
    completion_temperature: f32,
    max_tool_iterations: usize,
    assembly_policy: &'static str,
    transcript_policy: TranscriptPolicy,
}

impl ContextAssemblyLedger {
    fn total_chars(&self) -> Option<usize> {
        Some(self.total_chars)
    }
}

#[derive(Debug, Clone, Serialize)]
struct ContextComponentLedger {
    index: usize,
    role: String,
    inclusion_reason: &'static str,
    content_chars: usize,
    tool_call_chars: usize,
    total_chars: usize,
    estimated_tokens: usize,
    tool_call_count: usize,
    tool_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ModelProgressProjection {
    conservative_tokens_per_second: f64,
    sample_count: usize,
    expected_max_seconds: f64,
    allowed_seconds: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelProgressState {
    WaitingForFirstToken,
    Generating,
    GeneratingToolCall,
    ProgressUnknown,
    PossiblyStalled,
    Stalled,
}

impl ModelProgressState {
    fn as_str(self) -> &'static str {
        match self {
            Self::WaitingForFirstToken => "WaitingForFirstToken",
            Self::Generating => "Generating",
            Self::GeneratingToolCall => "GeneratingToolCall",
            Self::ProgressUnknown => "ProgressUnknown",
            Self::PossiblyStalled => "PossiblyStalled",
            Self::Stalled => "Stalled",
        }
    }

    fn has_progress_evidence(self) -> bool {
        matches!(
            self,
            Self::Generating | Self::GeneratingToolCall | Self::ProgressUnknown
        )
    }
}

#[derive(Debug, Clone, Serialize)]
struct RunnerActivitySample {
    source: String,
    process_active: Option<bool>,
    model_loaded: Option<bool>,
    accelerator_resident: Option<bool>,
    accelerator_label: Option<String>,
    gpu_utilization_percent: Option<f64>,
    raw_summary: Option<String>,
    error: Option<String>,
}

impl RunnerActivitySample {
    fn has_activity_evidence(&self) -> bool {
        self.process_active == Some(true)
            || self.model_loaded == Some(true)
            || self.accelerator_resident == Some(true)
            || self
                .gpu_utilization_percent
                .is_some_and(|utilization| utilization > 0.0)
    }
}

struct ModelProgressStatusInput<'a> {
    trace: &'a TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
    model: &'a str,
    progress_state: ModelProgressState,
    elapsed: Duration,
    seconds_since_observable_progress: f64,
    projection: &'a ModelProgressProjection,
    runner_activity: Option<&'a RunnerActivitySample>,
    stalled_candidate_checks: usize,
    automatic_interrupt: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ThroughputSample {
    timestamp: chrono::DateTime<chrono::Utc>,
    model: String,
    provider: String,
    host_signature: String,
    context_band: String,
    packet_type: String,
    expected_output_tokens: usize,
    turn: usize,
    llm_call_depth: usize,
    prompt_eval_count: Option<u64>,
    prompt_eval_duration_ns: Option<u64>,
    eval_count: u64,
    eval_duration_ns: u64,
    tokens_per_second: f64,
}

#[derive(Debug)]
struct ToolCallRunResult {
    ok: bool,
    content: String,
    duration_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
struct InspectionLoopDecision {
    signature: String,
    repeated_count: usize,
    limit: usize,
    turn: usize,
    llm_call_depth: usize,
}

#[derive(Debug, Default)]
struct InspectionLoopTracker {
    runtime: crate::runtime::RuntimeState,
}

impl InspectionLoopTracker {
    fn observe(
        &mut self,
        turn: usize,
        llm_call_depth: usize,
        call: &LlmToolCall,
        result: &ToolCallRunResult,
    ) -> Option<InspectionLoopDecision> {
        if self.runtime.meaningful_action_seen {
            return None;
        }
        if is_meaningful_source_edit(call, result) || is_validation_probe_result(result) {
            self.runtime.meaningful_action_seen = true;
            self.runtime.repeated_inspections.clear();
            return None;
        }

        let signature = inspection_signature(call)?;
        let event = crate::runtime::RuntimeEvent::Inspection {
            signature: signature.clone(),
        };
        let decision = crate::runtime::RuntimePolicy.decide(&self.runtime, &event);
        self.runtime.reduce(&event);
        match decision {
            crate::runtime::RuntimeDecision::StopRepeatedInspection { count, .. } => {
                Some(InspectionLoopDecision {
                    signature,
                    repeated_count: count,
                    limit: crate::runtime::MAX_PRE_VALIDATION_REPEATED_INSPECTIONS,
                    turn,
                    llm_call_depth,
                })
            }
            _ => None,
        }
    }
}

struct ContextAssemblyInput<'a> {
    model: &'a str,
    turn: usize,
    llm_call_depth: usize,
    messages: &'a [LlmMessage],
    tools: &'a [Box<dyn LlmTool>],
    completion_config: &'a CompletionConfig,
    context_window_tokens: Option<usize>,
    previous_call_total_chars: Option<usize>,
    transcript_policy: TranscriptPolicy,
}

fn context_assembly_ledger(input: ContextAssemblyInput<'_>) -> ContextAssemblyLedger {
    let components = input
        .messages
        .iter()
        .enumerate()
        .map(|(index, message)| component_ledger(index, message))
        .collect::<Vec<_>>();
    let mut role_counts = BTreeMap::new();
    let mut role_chars = BTreeMap::new();
    for component in &components {
        *role_counts.entry(component.role.clone()).or_insert(0) += 1;
        *role_chars.entry(component.role.clone()).or_insert(0) += component.total_chars;
    }
    let message_chars = components
        .iter()
        .map(|component| component.total_chars)
        .sum::<usize>();
    let tool_descriptor_chars = input
        .tools
        .iter()
        .map(|tool| {
            serde_json::to_string(&tool.descriptor())
                .unwrap_or_default()
                .len()
        })
        .sum::<usize>();
    let total_chars = message_chars + tool_descriptor_chars;
    let estimated_tokens = estimate_tokens(total_chars);
    let utilization = input
        .context_window_tokens
        .filter(|tokens| *tokens > 0)
        .map(|tokens| estimated_tokens as f64 / tokens as f64);
    let delta_chars_from_previous_call = input
        .previous_call_total_chars
        .map(|previous| total_chars as isize - previous as isize);

    ContextAssemblyLedger {
        model: input.model.to_string(),
        turn: input.turn,
        llm_call_depth: input.llm_call_depth,
        message_count: input.messages.len(),
        components,
        role_counts,
        role_chars,
        message_chars,
        tool_descriptor_chars,
        total_chars,
        estimated_tokens,
        context_window_tokens: input.context_window_tokens,
        utilization,
        pressure_band: pressure_band(utilization),
        previous_call_total_chars: input.previous_call_total_chars,
        delta_chars_from_previous_call,
        completion_temperature: input.completion_config.temperature,
        max_tool_iterations: input.completion_config.max_tool_iterations,
        assembly_policy: input.transcript_policy.as_str(),
        transcript_policy: input.transcript_policy,
    }
}

fn model_progress_projection(
    model: &str,
    expected_output_tokens: usize,
    context_band: &str,
    throughput_registry_path: &Path,
) -> ModelProgressProjection {
    let samples = load_matching_throughput_samples(throughput_registry_path, model, context_band);
    let conservative_tokens_per_second = percentile(
        samples
            .iter()
            .map(|sample| sample.tokens_per_second)
            .collect::<Vec<_>>(),
        0.05,
    )
    .unwrap_or(DEFAULT_THROUGHPUT_TPS);
    let expected_max_seconds = MODEL_PROGRESS_WARMUP_SECONDS
        + (expected_output_tokens as f64 / conservative_tokens_per_second)
        + MODEL_PROGRESS_TOOL_JSON_SLACK_SECONDS;
    let allowed_seconds = expected_max_seconds * MODEL_PROGRESS_VARIANCE_MULTIPLIER;
    ModelProgressProjection {
        conservative_tokens_per_second,
        sample_count: samples.len(),
        expected_max_seconds,
        allowed_seconds,
    }
}

fn trace_model_progress_status(input: ModelProgressStatusInput<'_>) -> Result<()> {
    let runner_activity_evidence = input
        .runner_activity
        .is_some_and(RunnerActivitySample::has_activity_evidence);
    input.trace.event(
        "llm.progress.status",
        serde_json::json!({
            "turn": input.turn,
            "llm_call_depth": input.llm_call_depth,
            "model": input.model,
            "progress_state": input.progress_state.as_str(),
            "elapsed_seconds": input.elapsed.as_secs_f64(),
            "seconds_since_observable_progress": input.seconds_since_observable_progress,
            "allowed_seconds": input.projection.allowed_seconds,
            "projected_allowance_exceeded": input.elapsed.as_secs_f64() > input.projection.allowed_seconds,
            "runner_activity_evidence": runner_activity_evidence,
            "runner_activity": input.runner_activity,
            "stalled_candidate_checks": input.stalled_candidate_checks,
            "automatic_interrupt": input.automatic_interrupt,
        }),
    )
}

fn compact_retained_tool_results(
    messages: &mut [LlmMessage],
    tools: &[Box<dyn LlmTool>],
    trace: &TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
    transcript_policy: TranscriptPolicy,
    context_window_tokens: Option<usize>,
) -> Result<()> {
    let retained_tool_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::Tool).then_some(index))
        .collect::<Vec<_>>();
    let latest_failed_validation_index =
        latest_failed_validation_tool_index(messages, &retained_tool_indices);
    let effective = effective_tool_result_compaction(
        transcript_policy,
        messages,
        tools,
        context_window_tokens,
        latest_failed_validation_index,
    );
    let compaction = effective.compaction;
    if !compaction.enabled {
        return Ok(());
    }
    if effective.repair_handoff_active {
        trace.event(
            "llm.context_assembly.validation_repair_handoff",
            serde_json::json!({
                "turn": turn,
                "llm_call_depth": llm_call_depth,
                "transcript_policy": transcript_policy,
                "effective_assembly_policy": "append_validation_repair_packet",
                "estimated_tokens_before_compaction": effective.estimated_tokens_before_compaction,
                "utilization_before_compaction": effective.utilization_before_compaction,
                "pressure_band_before_compaction": effective.pressure_band_before_compaction,
                "latest_failed_validation_index": latest_failed_validation_index,
                "raw_recent_tool_results_retained": compaction.raw_recent_count,
                "max_raw_tool_result_chars": compaction.max_raw_tool_result_chars,
            }),
        )?;
    }
    let latest_failed_validation_index = compaction
        .preserve_latest_failed_validation
        .then_some(latest_failed_validation_index)
        .flatten();
    let retain_raw_from = retained_tool_indices
        .len()
        .saturating_sub(compaction.raw_recent_count);

    for (ordinal, message_index) in retained_tool_indices.into_iter().enumerate() {
        if ordinal >= retain_raw_from {
            continue;
        }
        if Some(message_index) == latest_failed_validation_index {
            continue;
        }
        let message = &mut messages[message_index];
        let Some(content) = message.content.as_ref() else {
            continue;
        };
        if content.len() <= compaction.max_raw_tool_result_chars
            || content.starts_with(TOOL_RESULT_SUMMARY_PREFIX)
        {
            continue;
        }

        let original_chars = content.len();
        let original_estimated_tokens = estimate_tokens(original_chars);
        let tool_name = message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .map(|call| call.name.clone())
            .unwrap_or_else(|| "unknown".to_string());
        let preview = limit_preview(content, 1_200);
        let summary = format!(
            "{TOOL_RESULT_SUMMARY_PREFIX}\n\
             Tool result summary retained in prompt to reduce context pressure.\n\
             Tool: {tool_name}\n\
             Original chars: {original_chars}\n\
             Original estimated tokens: {original_estimated_tokens}\n\
             Raw result remains available in earlier trace tool events.\n\
             Preview:\n{preview}"
        );
        let retained_chars = summary.len();
        message.content = Some(summary);
        trace.event(
            "llm.context_assembly.tool_result_compacted",
            serde_json::json!({
                "turn": turn,
                "llm_call_depth": llm_call_depth,
                "message_index": message_index,
                "tool_name": tool_name,
                "original_chars": original_chars,
                "original_estimated_tokens": original_estimated_tokens,
                "retained_chars": retained_chars,
                "retained_estimated_tokens": estimate_tokens(retained_chars),
                "transcript_policy": transcript_policy,
                "effective_repair_handoff": effective.repair_handoff_active,
                "raw_recent_tool_results_retained": compaction.raw_recent_count,
                "max_raw_tool_result_chars": compaction.max_raw_tool_result_chars,
                "preserved_latest_failed_validation_index": latest_failed_validation_index,
            }),
        )?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct EffectiveToolResultCompaction {
    compaction: ToolResultCompaction,
    repair_handoff_active: bool,
    estimated_tokens_before_compaction: usize,
    utilization_before_compaction: Option<f64>,
    pressure_band_before_compaction: &'static str,
}

fn effective_tool_result_compaction(
    transcript_policy: TranscriptPolicy,
    messages: &[LlmMessage],
    tools: &[Box<dyn LlmTool>],
    context_window_tokens: Option<usize>,
    latest_failed_validation_index: Option<usize>,
) -> EffectiveToolResultCompaction {
    let estimated_tokens_before_compaction = estimate_prompt_tokens(messages, tools);
    let utilization_before_compaction = context_window_tokens
        .filter(|tokens| *tokens > 0)
        .map(|tokens| estimated_tokens_before_compaction as f64 / tokens as f64);
    let pressure_band_before_compaction = pressure_band(utilization_before_compaction);
    let repair_handoff_active = transcript_policy == TranscriptPolicy::SummarizedRepairHandoff
        && latest_failed_validation_index.is_some()
        && pressure_band_before_compaction == "red";
    let compaction = if repair_handoff_active {
        ToolResultCompaction {
            enabled: true,
            raw_recent_count: REPAIR_HANDOFF_RAW_TOOL_RESULT_RECENT_COUNT,
            max_raw_tool_result_chars: REPAIR_HANDOFF_RAW_TOOL_RESULT_MAX_CHARS,
            preserve_latest_failed_validation: true,
        }
    } else {
        transcript_policy.compaction()
    };

    EffectiveToolResultCompaction {
        compaction,
        repair_handoff_active,
        estimated_tokens_before_compaction,
        utilization_before_compaction,
        pressure_band_before_compaction,
    }
}

fn latest_failed_validation_tool_index(
    messages: &[LlmMessage],
    retained_tool_indices: &[usize],
) -> Option<usize> {
    retained_tool_indices.iter().rev().copied().find(|index| {
        messages[*index]
            .content
            .as_deref()
            .is_some_and(is_failed_validation_tool_result)
    })
}

fn is_failed_validation_tool_result(content: &str) -> bool {
    content.contains("\"validation_probe\":true")
        && content.contains("\"success\":false")
        && content.contains("\"repair_required\"")
}

fn classify_model_progress(
    latest_visible_state: ModelProgressState,
    elapsed: Duration,
    _since_observable_progress: Duration,
    allowed_seconds: f64,
    runner_activity: Option<&RunnerActivitySample>,
    stalled_candidate_checks: usize,
) -> ModelProgressState {
    if runner_activity.is_some_and(RunnerActivitySample::has_activity_evidence) {
        return match latest_visible_state {
            ModelProgressState::Generating | ModelProgressState::GeneratingToolCall => {
                latest_visible_state
            }
            _ => ModelProgressState::ProgressUnknown,
        };
    }

    if elapsed.as_secs_f64() <= allowed_seconds {
        return latest_visible_state;
    }

    if stalled_candidate_checks + 1 >= STALLED_CONFIRMATION_CHECKS {
        ModelProgressState::Stalled
    } else {
        ModelProgressState::PossiblyStalled
    }
}

fn progress_status_interval() -> Duration {
    std::env::var("HARNESS_PROGRESS_STATUS_INTERVAL_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_PROGRESS_STATUS_INTERVAL_SECONDS))
}

async fn sample_runner_activity(model: &str) -> RunnerActivitySample {
    let mut sample = sample_ollama_activity(model).await;
    sample.process_active = sample_process_active().await;
    if sample.gpu_utilization_percent.is_none() {
        if let Some(utilization) = sample_nvidia_gpu_utilization().await {
            sample.source.push_str("+nvidia-smi");
            sample.gpu_utilization_percent = Some(utilization);
        } else if let Some(utilization) = sample_rocm_gpu_utilization().await {
            sample.source.push_str("+rocm-smi");
            sample.gpu_utilization_percent = Some(utilization);
        } else if let Some(utilization) = sample_macmon_gpu_utilization().await {
            sample.source.push_str("+macmon");
            sample.gpu_utilization_percent = Some(utilization);
        } else if let Some(utilization) = sample_powermetrics_gpu_utilization().await {
            sample.source.push_str("+powermetrics");
            sample.gpu_utilization_percent = Some(utilization);
        }
    }
    sample
}

async fn sample_ollama_activity(model: &str) -> RunnerActivitySample {
    match command_output("ollama", &["ps"]).await {
        Ok(output) => {
            let parsed = parse_ollama_ps(model, &output);
            RunnerActivitySample {
                source: "ollama ps".to_string(),
                process_active: None,
                model_loaded: Some(parsed.model_loaded),
                accelerator_resident: parsed.accelerator_resident,
                accelerator_label: parsed.accelerator_label,
                gpu_utilization_percent: None,
                raw_summary: parsed.raw_summary,
                error: None,
            }
        }
        Err(error) => RunnerActivitySample {
            source: "ollama ps".to_string(),
            process_active: None,
            model_loaded: None,
            accelerator_resident: None,
            accelerator_label: None,
            gpu_utilization_percent: None,
            raw_summary: None,
            error: Some(error),
        },
    }
}

async fn sample_process_active() -> Option<bool> {
    command_output("pgrep", &["-fl", "ollama"])
        .await
        .ok()
        .map(|output| !output.trim().is_empty())
}

async fn sample_nvidia_gpu_utilization() -> Option<f64> {
    command_output(
        "nvidia-smi",
        &[
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
        ],
    )
    .await
    .ok()
    .and_then(|output| parse_max_number(&output))
}

async fn sample_rocm_gpu_utilization() -> Option<f64> {
    command_output("rocm-smi", &["--showuse"])
        .await
        .ok()
        .and_then(|output| parse_first_percent_for_keyword(&output, "GPU"))
}

async fn sample_macmon_gpu_utilization() -> Option<f64> {
    if std::env::consts::OS != "macos" {
        return None;
    }
    command_output_with_timeout(
        "macmon",
        &["pipe", "-s", "1"],
        MACMON_SAMPLE_TIMEOUT_SECONDS,
    )
    .await
    .ok()
    .and_then(|output| parse_macmon_gpu_utilization(&output))
}

async fn sample_powermetrics_gpu_utilization() -> Option<f64> {
    if std::env::consts::OS != "macos" || command_output("id", &["-u"]).await.ok()?.trim() != "0" {
        return None;
    }
    command_output(
        "powermetrics",
        &["--samplers", "gpu_power", "-n", "1", "-i", "1000"],
    )
    .await
    .ok()
    .and_then(|output| parse_first_percent_for_keyword(&output, "GPU"))
}

async fn command_output(program: &str, args: &[&str]) -> std::result::Result<String, String> {
    command_output_with_timeout(program, args, RUNNER_SAMPLE_TIMEOUT_SECONDS).await
}

async fn command_output_with_timeout(
    program: &str,
    args: &[&str],
    timeout_seconds: u64,
) -> std::result::Result<String, String> {
    let mut command = Command::new(program);
    command.args(args).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(timeout_seconds), command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }
        Ok(Ok(output)) => Err(String::from_utf8_lossy(&output.stderr).trim().to_string()),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!("{program} timed out after {timeout_seconds}s")),
    }
}

#[derive(Debug, Clone)]
struct ParsedOllamaPs {
    model_loaded: bool,
    accelerator_resident: Option<bool>,
    accelerator_label: Option<String>,
    raw_summary: Option<String>,
}

fn parse_ollama_ps(model: &str, output: &str) -> ParsedOllamaPs {
    let model_line = output.lines().find(|line| {
        line.split_whitespace()
            .next()
            .is_some_and(|name| name == model || name.starts_with(model))
    });
    let Some(line) = model_line else {
        return ParsedOllamaPs {
            model_loaded: false,
            accelerator_resident: Some(false),
            accelerator_label: None,
            raw_summary: None,
        };
    };

    let uppercase = line.to_ascii_uppercase();
    let accelerator_resident = Some(uppercase.contains("GPU") || uppercase.contains("NPU"));
    ParsedOllamaPs {
        model_loaded: true,
        accelerator_resident,
        accelerator_label: processor_label_from_ollama_line(line),
        raw_summary: Some(line.to_string()),
    }
}

fn processor_label_from_ollama_line(line: &str) -> Option<String> {
    let uppercase = line.to_ascii_uppercase();
    if uppercase.contains("GPU") || uppercase.contains("NPU") {
        return line
            .split_whitespace()
            .collect::<Vec<_>>()
            .windows(2)
            .find(|window| {
                window[0].ends_with('%')
                    && matches!(window[1].to_ascii_uppercase().as_str(), "GPU" | "NPU")
            })
            .map(|window| format!("{} {}", window[0], window[1]));
    }
    if uppercase.contains("CPU") {
        return Some("CPU".to_string());
    }
    None
}

fn parse_first_percent_for_keyword(output: &str, keyword: &str) -> Option<f64> {
    output
        .lines()
        .filter(|line| {
            line.to_ascii_uppercase()
                .contains(&keyword.to_ascii_uppercase())
        })
        .find_map(parse_percent_from_line)
}

fn parse_percent_from_line(line: &str) -> Option<f64> {
    line.split(|character: char| {
        character.is_whitespace() || matches!(character, ':' | ',' | '(' | ')')
    })
    .find_map(|token| token.strip_suffix('%').unwrap_or(token).parse::<f64>().ok())
}

fn parse_max_number(output: &str) -> Option<f64> {
    output
        .split(|character: char| {
            character.is_whitespace() || matches!(character, '%' | ',' | ':' | '(' | ')')
        })
        .filter_map(|token| token.parse::<f64>().ok())
        .max_by(f64::total_cmp)
}

fn parse_macmon_gpu_utilization(output: &str) -> Option<f64> {
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|sample| {
            let raw = sample
                .get("gpu_usage")
                .and_then(|value| value.as_array())
                .and_then(|values| values.get(1))
                .and_then(|value| value.as_f64())
                .or_else(|| sample.get("gpu_usage_pct").and_then(|value| value.as_f64()))?;
            normalize_utilization_percent(raw)
        })
}

fn normalize_utilization_percent(value: f64) -> Option<f64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    if value <= 1.0 {
        Some(value * 100.0)
    } else {
        Some(value.min(100.0))
    }
}

fn throughput_sample(
    model: &str,
    packet_type: &str,
    expected_output_tokens: usize,
    context_band: &str,
    turn: usize,
    llm_call_depth: usize,
    metrics: &StreamMetrics,
) -> Option<ThroughputSample> {
    Some(ThroughputSample {
        timestamp: chrono::Utc::now(),
        model: model.to_string(),
        provider: metrics.provider.clone(),
        host_signature: host_signature(),
        context_band: context_band.to_string(),
        packet_type: packet_type.to_string(),
        expected_output_tokens,
        turn,
        llm_call_depth,
        prompt_eval_count: metrics.prompt_eval_count,
        prompt_eval_duration_ns: metrics.prompt_eval_duration_ns,
        eval_count: metrics.eval_count?,
        eval_duration_ns: metrics.eval_duration_ns?,
        tokens_per_second: metrics.tokens_per_second?,
    })
}

fn append_throughput_sample(path: &Path, sample: &ThroughputSample) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening throughput registry {}", path.display()))?;
    serde_json::to_writer(&mut file, sample)?;
    use std::io::Write;
    file.write_all(b"\n")?;
    Ok(())
}

fn load_matching_throughput_samples(
    path: &Path,
    model: &str,
    context_band: &str,
) -> Vec<ThroughputSample> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let host_signature = host_signature();
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<ThroughputSample>(line).ok())
        .filter(|sample| {
            sample.model == model
                && sample.provider == "ollama"
                && sample.host_signature == host_signature
                && sample.context_band == context_band
        })
        .collect()
}

fn percentile(mut values: Vec<f64>, quantile: f64) -> Option<f64> {
    values.retain(|value| value.is_finite() && *value > 0.0);
    if values.is_empty() {
        return None;
    }
    values.sort_by(|left, right| left.total_cmp(right));
    let index = ((values.len() - 1) as f64 * quantile.clamp(0.0, 1.0)).floor() as usize;
    values.get(index).copied()
}

fn host_signature() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn component_ledger(index: usize, message: &LlmMessage) -> ContextComponentLedger {
    let content_chars = message.content.as_deref().unwrap_or_default().len();
    let tool_call_chars = message
        .tool_calls
        .as_ref()
        .map(|calls| serde_json::to_string(calls).unwrap_or_default().len())
        .unwrap_or_default();
    let tool_names = message
        .tool_calls
        .as_ref()
        .map(|calls| calls.iter().map(|call| call.name.clone()).collect())
        .unwrap_or_default();
    let total_chars = content_chars + tool_call_chars;
    ContextComponentLedger {
        index,
        role: role_label(message.role).to_string(),
        inclusion_reason: inclusion_reason(index, message),
        content_chars,
        tool_call_chars,
        total_chars,
        estimated_tokens: estimate_tokens(total_chars),
        tool_call_count: message
            .tool_calls
            .as_ref()
            .map(Vec::len)
            .unwrap_or_default(),
        tool_names,
    }
}

fn inclusion_reason(index: usize, message: &LlmMessage) -> &'static str {
    match message.role {
        MessageRole::System => "base_system_prompt",
        MessageRole::User if index == 1 => "benchmark_task_and_run_instructions",
        MessageRole::User => "agent_loop_instruction_or_repair_prompt",
        MessageRole::Assistant
            if message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty()) =>
        {
            "retained_assistant_tool_request"
        }
        MessageRole::Assistant => "retained_assistant_text",
        MessageRole::Tool
            if message
                .content
                .as_deref()
                .is_some_and(|content| content.starts_with(TOOL_RESULT_SUMMARY_PREFIX)) =>
        {
            "retained_summarized_tool_result"
        }
        MessageRole::Tool => "retained_raw_tool_result",
    }
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn pressure_band(utilization: Option<f64>) -> &'static str {
    match utilization {
        None => "unknown",
        Some(value) if value < 0.15 => "green",
        Some(value) if value < 0.25 => "yellow",
        Some(value) if value < 0.40 => "orange",
        Some(_) => "red",
    }
}

fn message_chars(message: &LlmMessage) -> usize {
    message.content.as_deref().unwrap_or_default().len()
        + message
            .tool_calls
            .as_ref()
            .map(|calls| serde_json::to_string(calls).unwrap_or_default().len())
            .unwrap_or_default()
}

fn estimate_prompt_tokens(messages: &[LlmMessage], tools: &[Box<dyn LlmTool>]) -> usize {
    let message_chars = messages.iter().map(message_chars).sum::<usize>();
    let tool_descriptor_chars = tools
        .iter()
        .map(|tool| {
            serde_json::to_string(&tool.descriptor())
                .unwrap_or_default()
                .len()
        })
        .sum::<usize>();
    estimate_tokens(message_chars + tool_descriptor_chars)
}

async fn run_tool_call(
    call: &LlmToolCall,
    tools: &[Box<dyn LlmTool>],
    correlation_id: &str,
) -> ToolCallRunResult {
    let started = Instant::now();
    let ctx = ToolRunCtx {
        correlation_id: Some(correlation_id.to_string()),
        source: Some("adaptive-agent-harness".to_string()),
        ..Default::default()
    };
    let Some(tool) = tools.iter().find(|tool| tool.matches(&call.name)) else {
        return ToolCallRunResult {
            ok: false,
            content: serde_json::json!({
                "error": format!("Tool {:?} not found", call.name)
            })
            .to_string(),
            duration_ms: started.elapsed().as_millis(),
        };
    };

    match tool.run(&call.arguments, &ctx).await {
        Ok(value) => ToolCallRunResult {
            ok: true,
            content: serde_json::to_string(&value).unwrap_or_else(|error| {
                serde_json::json!({ "error": error.to_string() }).to_string()
            }),
            duration_ms: started.elapsed().as_millis(),
        },
        Err(error) => ToolCallRunResult {
            ok: false,
            content: serde_json::json!({ "error": error.to_string() }).to_string(),
            duration_ms: started.elapsed().as_millis(),
        },
    }
}

fn inspection_loop_failure_summary(decision: &InspectionLoopDecision) -> String {
    format!(
        "pre-validation inspection loop detected: {} repeated {} times before a source edit or validation probe",
        decision.signature, decision.repeated_count
    )
}

fn action_intent_signal(thinking: &str) -> bool {
    let text = thinking.to_ascii_lowercase();
    let intent = [
        "let me",
        "i will",
        "i'll",
        "i'm going to",
        "i am going to",
        "i should",
        "i need to",
        "now i",
        "next i",
    ]
    .iter()
    .any(|phrase| text.contains(phrase));
    let action = [
        "write",
        "patch",
        "edit",
        "implement",
        "create",
        "add ",
        "call write_file",
        "use write_file",
        "call edit_file",
        "use edit_file",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
        || crate::profile::select_profile()
            .action_intent_phrases()
            .iter()
            .any(|phrase| text.contains(phrase));
    intent && action
}

fn push_bounded_buffer(buffer: &mut String, chunk: &str, max_chars: usize) {
    if max_chars == 0 {
        buffer.clear();
        return;
    }
    buffer.push_str(chunk);
    if buffer.len() <= max_chars {
        return;
    }
    let mut start = buffer.len() - max_chars;
    while !buffer.is_char_boundary(start) {
        start += 1;
    }
    buffer.drain(..start);
}

fn inspection_signature(call: &LlmToolCall) -> Option<String> {
    match call.name.as_str() {
        "read_file" => {
            let path = call.arguments.get("path")?.as_str()?.trim();
            let line_start = call
                .arguments
                .get("line_start")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "*".to_string());
            let line_end = call
                .arguments
                .get("line_end")
                .and_then(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "*".to_string());
            Some(format!("read_file:{path}:{line_start}-{line_end}"))
        }
        "shell_command" => {
            let command = call.arguments.get("command")?.as_str()?;
            crate::profile::select_profile()
                .is_inspection_shell_command(command)
                .then(|| format!("shell_command:{}", normalize_shell_command(command)))
        }
        _ => None,
    }
}

fn is_meaningful_source_edit(call: &LlmToolCall, result: &ToolCallRunResult) -> bool {
    if !result.ok {
        return false;
    }
    match call.name.as_str() {
        "edit_file" => call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(crate::profile::coding::path_requires_validation_after_write),
        "patch_file" => true,
        "write_file" => call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(crate::profile::coding::path_requires_validation_after_write),
        _ => false,
    }
}

fn is_validation_probe_result(result: &ToolCallRunResult) -> bool {
    serde_json::from_str::<serde_json::Value>(&result.content)
        .ok()
        .and_then(|value| {
            value
                .get("validation_probe")
                .and_then(serde_json::Value::as_bool)
        })
        == Some(true)
}

fn successful_validation_from_tool_result(
    result: &ToolCallRunResult,
    requested_validation_commands: &[String],
    requested_validation_pending_after_write: bool,
) -> Option<SuccessfulValidationSnapshot> {
    if !result.ok {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(&result.content).ok()?;
    if value
        .get("validation_probe")
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        return None;
    }
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let clears_pending_source_writes = value
        .get("validation_probe_clears_pending_source_writes")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if requested_validation_commands.is_empty() && !clears_pending_source_writes {
        return None;
    }
    if !requested_validation_commands.is_empty()
        && !clears_pending_source_writes
        && !requested_validation_pending_after_write
    {
        return None;
    }
    if value
        .get("shell_mutation_requires_validation")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return None;
    }
    let command = value
        .get("command")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("validation command")
        .to_string();
    if !validation_matches_requested_command(&command, requested_validation_commands) {
        return None;
    }
    Some(SuccessfulValidationSnapshot {
        command,
        command_family: value
            .get("command_family")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("validation")
            .to_string(),
        status: value
            .get("status")
            .and_then(serde_json::Value::as_i64)
            .and_then(|status| i32::try_from(status).ok()),
        total_shell_probes: 0,
        total_write_operations: 0,
    })
}

fn normalize_shell_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn limit_preview(content: &str, max_chars: usize) -> String {
    content.chars().take(max_chars).collect()
}

fn context_snapshot(
    messages: &[LlmMessage],
    tools: &[Box<dyn LlmTool>],
    policy: &ToolPolicySnapshot,
    context_window_tokens: Option<usize>,
    transcript_policy: TranscriptPolicy,
) -> serde_json::Value {
    let message_chars: usize = messages
        .iter()
        .map(|message| {
            message.content.as_deref().unwrap_or_default().len()
                + message
                    .tool_calls
                    .as_ref()
                    .map(|calls| serde_json::to_string(calls).unwrap_or_default().len())
                    .unwrap_or_default()
        })
        .sum();
    let tool_descriptor_chars: usize = tools
        .iter()
        .map(|tool| {
            serde_json::to_string(&tool.descriptor())
                .unwrap_or_default()
                .len()
        })
        .sum();
    let outer_chars = message_chars + tool_descriptor_chars;
    let estimated_tokens = estimate_tokens(outer_chars);
    let utilization = context_window_tokens
        .filter(|tokens| *tokens > 0)
        .map(|tokens| estimated_tokens as f64 / tokens as f64);
    let inferred_effective_chars = outer_chars;
    let inferred_effective_tokens = estimate_tokens(inferred_effective_chars);
    let inferred_effective_utilization = context_window_tokens
        .filter(|tokens| *tokens > 0)
        .map(|tokens| inferred_effective_tokens as f64 / tokens as f64);

    serde_json::json!({
        "message_count": messages.len(),
        "role_counts": role_counts(messages),
        "message_chars": message_chars,
        "tool_descriptor_chars": tool_descriptor_chars,
        "estimated_total_chars": outer_chars,
        "approx_chars_per_token": APPROX_CHARS_PER_TOKEN,
        "estimated_tokens": estimated_tokens,
        "context_window_tokens": context_window_tokens,
        "utilization": utilization,
        "cumulative_tool_result_chars": policy.total_tool_result_chars,
        "cumulative_tool_result_estimated_tokens": policy.total_tool_result_estimated_tokens,
        "max_tool_result_chars": policy.max_tool_result_chars,
        "max_tool_result_estimated_tokens": policy.max_tool_result_estimated_tokens,
        "max_tool_result_kind": policy.max_tool_result_kind,
        "tool_result_chars_by_kind": policy.tool_result_chars_by_kind,
        "transcript_policy": transcript_policy,
        "assembly_policy": transcript_policy.as_str(),
        "inferred_effective_chars": inferred_effective_chars,
        "inferred_effective_tokens": inferred_effective_tokens,
        "inferred_effective_utilization": inferred_effective_utilization,
        "note": "estimated_tokens and inferred_effective_tokens estimate retained prompt content. Cumulative tool-result fields track raw evidence observed in traces, not necessarily prompt-retained content after summarization."
    })
}

fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(APPROX_CHARS_PER_TOKEN)
}

fn role_counts(messages: &[LlmMessage]) -> serde_json::Value {
    let mut system = 0usize;
    let mut user = 0usize;
    let mut assistant = 0usize;
    let mut tool = 0usize;
    for message in messages {
        match message.role {
            MessageRole::System => system += 1,
            MessageRole::User => user += 1,
            MessageRole::Assistant => assistant += 1,
            MessageRole::Tool => tool += 1,
        }
    }
    serde_json::json!({
        "system": system,
        "user": user,
        "assistant": assistant,
        "tool": tool,
    })
}

fn empty_response_prompt(consecutive_empty_responses: usize) -> String {
    let pressure = if consecutive_empty_responses >= EMPTY_RESPONSE_ESCALATION_TURNS {
        "Empty-response escalation is active. Your next turn must take exactly one bounded step: \
         use tools for one concrete missing artifact or one deterministic validation/probe, \
         reply DONE only if validation has already passed, or reply FAIL with a concrete blocker."
    } else if consecutive_empty_responses >= 2 {
        "You have returned multiple empty responses. Narrow the next action to the smallest missing artifact or failing validation signal."
    } else {
        "Your previous turn ended without a DONE or FAIL response."
    };
    format!(
        "{pressure}\n\
         Continue from the current experiment state. Do not repeat state inspection already performed unless necessary. \
         If required files are missing, write the next missing file now. \
         Run deterministic validation before replying DONE. Reply FAIL only if blocked."
    )
}

fn hidden_only_no_action_prompt(
    consecutive_hidden_only_no_action_turns: usize,
    tool_calls_this_turn: usize,
) -> String {
    format!(
        "Hidden-only no-action turn detected. Your previous turn produced hidden reasoning \
         but no visible final text, no source mutation, and no validation probe. \
         Consecutive hidden-only no-action turns: {consecutive_hidden_only_no_action_turns}. \
         Tool calls in the previous turn: {tool_calls_this_turn}.\n\
         In the next turn, take exactly one concrete action: write or edit the next source change, \
         run a deterministic validation probe, or reply FAIL with a concrete blocker. \
         Do not repeat broad inspection or restate the plan."
    )
}

fn action_boundary_interrupt_prompt(decision: &ActionBoundaryNoActionDecision) -> String {
    action_boundary_interrupt_prompt_text(
        &decision.interrupt,
        decision.consecutive_no_action_turns,
        decision.escalation_required,
    )
}

fn action_boundary_interrupt_prompt_for_interrupt(interrupt: &ActionBoundaryInterrupt) -> String {
    action_boundary_interrupt_prompt_text(interrupt, 1, false)
}

fn action_boundary_interrupt_prompt_text(
    interrupt: &ActionBoundaryInterrupt,
    consecutive_no_action_turns: usize,
    escalation_required: bool,
) -> String {
    let pressure = if escalation_required {
        format!(
            "Action-boundary escalation is active after {consecutive_no_action_turns} consecutive interrupted action-intent turns without a source edit or validation probe."
        )
    } else {
        format!(
            "Action-boundary no-action count: {consecutive_no_action_turns} consecutive interrupted action-intent turn(s) without a source edit or validation probe."
        )
    };
    format!(
        "{pressure}\n\
         Action-boundary interrupt fired. Your hidden reasoning repeatedly stated intent to act, \
         but no assistant-visible content, completed tool call, source edit, validation probe, \
         or FAIL crossed the stream boundary.\n\
         Estimated hidden-thinking tokens in the interrupted call: {tokens}. \
         Action-intent hits: {hits} of required {hit_limit}. \
         Latest preview: {preview}\n\
         Your next turn must take exactly one concrete action: write or edit the next source change, \
         run one deterministic validation or diagnostic probe, or reply FAIL with a concrete blocker. \
         Do not restate the plan. Do not repeat broad inspection.",
        tokens = interrupt.call_thinking_estimated_tokens,
        hits = interrupt.action_intent_hits,
        hit_limit = interrupt.hit_limit,
        preview = interrupt.latest_preview,
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmptyResponseDecision {
    escalation_required: bool,
    prompt: String,
}

#[cfg(test)]
fn empty_response_decision(consecutive_empty_responses: usize) -> EmptyResponseDecision {
    EmptyResponseDecision {
        escalation_required: consecutive_empty_responses >= EMPTY_RESPONSE_ESCALATION_TURNS,
        prompt: empty_response_prompt(consecutive_empty_responses),
    }
}

fn validation_repair_prompt(repair: &ValidationRepairSnapshot) -> String {
    let failure_details = repair_detail_text(repair);
    format!(
        "Validation repair action contract is active.\n\
         Failing command: {command}\n\
         Failure text: {failure_text}\n\
         Failure details:\n{failure_details}\n\
         Command family failure count: {command_count}\n\
         Failure-summary repeat count: {summary_count}\n\
         Your next turn must take exactly one targeted repair action based on the listed failure details: \
         apply one focused source edit with edit_file, run one deterministic diagnostic probe that narrows those exact details, \
         or reply FAIL with a concrete blocker. \
         Do not emit a text-only repair plan. Do not repeat broad inspection. \
         {repair_ladder_suffix}",
        command = repair.command,
        failure_text = repair.failure_text,
        failure_details = failure_details,
        command_count = repair.repeated_command_family_count,
        summary_count = repair.repeated_failure_summary_count,
        repair_ladder_suffix = crate::profile::select_profile().repair_ladder_suffix(),
    )
}

fn validation_repair_no_action_prompt(decision: &RepairNoActionDecision) -> String {
    let repair = &decision.active_repair;
    let failure_details = repair_detail_text(repair);
    let read_targets = if decision.validation_repair_read_paths.is_empty() {
        "none recorded".to_string()
    } else {
        decision
            .validation_repair_read_paths
            .iter()
            .map(|(path, count)| format!("{path} ({count})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let pressure = if decision.escalation_required {
        format!(
            "Validation repair escalation is active. You have spent {} consecutive repair turns without an edit or validation probe.",
            decision.consecutive_no_action_turns
        )
    } else {
        "Validation repair action contract remains active. The last repair turn made no edit and ran no validation probe.".to_string()
    };
    format!(
        "{pressure}\n\
         Failing command: {command}\n\
         Failure text: {failure_text}\n\
         Failure details:\n{failure_details}\n\
         Repair read targets since the latest failed validation: {read_targets}\n\
         Your next turn must take exactly one targeted repair action: apply one focused structured edit with edit_file to the relevant source, \
         replace an existing source file with write_file only after reading the complete file and preserving unrelated content, \
         run one deterministic probe that narrows the listed failure details, or reply FAIL with a concrete blocker. \
         Do not emit a text-only repair plan or restate the plan without taking one of those actions.",
        command = repair.command,
        failure_text = repair.failure_text,
        failure_details = failure_details,
    )
}

fn repair_detail_text(repair: &ValidationRepairSnapshot) -> String {
    if repair.failure_details.is_empty() {
        format!("- {}", repair.failure_text)
    } else {
        repair
            .failure_details
            .iter()
            .map(|detail| format!("- {detail}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream;
    use mojentic::llm::models::LlmGatewayResponse;
    use mojentic::llm::tools::{FunctionDescriptor, ToolDescriptor};
    use serde_json::{Value, json};
    use std::collections::{HashMap, VecDeque};
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn estimates_tokens_with_ceil_division() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
    }

    #[test]
    fn default_max_thinking_only_tokens_uses_generation_budget_fraction() {
        assert_eq!(default_expected_output_tokens("multi-file-edit"), 4_096);
        assert_eq!(default_expected_output_tokens("multi-file-patch"), 4_096);
        assert_eq!(default_max_thinking_only_tokens(4_096, None), 4_096);
        assert_eq!(default_max_thinking_only_tokens(4_096, Some(32_768)), 8_192);
        assert_eq!(default_max_thinking_only_tokens(4_096, Some(8_000)), 4_096);
        assert_eq!(default_max_thinking_only_tokens(4_096, Some(0)), 4_096);
    }

    #[test]
    fn default_repair_exit_thinking_tokens_uses_bounded_retry_threshold() {
        assert_eq!(default_repair_exit_thinking_tokens(), 16_384);
    }

    #[test]
    fn action_intent_signal_requires_intent_and_action_language() {
        assert!(action_intent_signal(
            "Let me write the missing implementation now."
        ));
        assert!(action_intent_signal(
            "I will use edit_file to edit src/lib.rs."
        ));
        assert!(action_intent_signal(
            "I'm going to write the first source file."
        ));
        assert!(!action_intent_signal(
            "The code probably needs more structure."
        ));
        assert!(!action_intent_signal("Let me think about the design."));
        assert!(!action_intent_signal(
            "Write operations are tracked by the harness."
        ));
    }

    #[test]
    fn counts_message_roles() {
        let counts = role_counts(&[
            LlmMessage::system("system"),
            LlmMessage::user("user"),
            LlmMessage::assistant("assistant"),
        ]);
        assert_eq!(counts["system"], 1);
        assert_eq!(counts["user"], 1);
        assert_eq!(counts["assistant"], 1);
        assert_eq!(counts["tool"], 0);
    }

    #[test]
    fn context_snapshot_includes_inferred_tool_pressure() {
        let policy = ToolPolicySnapshot {
            total_tool_calls: 3,
            consecutive_writes_without_shell: 0,
            writes_since_shell_probe: 0,
            writes_since_shell_probe_paths: BTreeMap::new(),
            validation_required_after_write: false,
            total_write_operations: 0,
            total_shell_probes: 1,
            validation_repair: None,
            validation_repair_read_paths: BTreeMap::new(),
            latest_successful_validation_after_write: None,
            patch_fallbacks: vec![],
            total_tool_result_chars: 4_000,
            total_tool_result_estimated_tokens: 1_000,
            max_tool_result_chars: 2_000,
            max_tool_result_estimated_tokens: 500,
            max_tool_result_kind: Some("tool.read_file".to_string()),
            tool_result_chars_by_kind: std::collections::BTreeMap::from([(
                "tool.read_file".to_string(),
                4_000,
            )]),
        };
        let snapshot = context_snapshot(
            &[LlmMessage::system("system"), LlmMessage::user("task")],
            &[],
            &policy,
            Some(8_000),
            TranscriptPolicy::SummarizedTranscript,
        );

        assert_eq!(snapshot["cumulative_tool_result_chars"], 4_000);
        assert_eq!(snapshot["cumulative_tool_result_estimated_tokens"], 1_000);
        assert_eq!(snapshot["max_tool_result_kind"], "tool.read_file");
        assert_eq!(
            snapshot["inferred_effective_tokens"],
            snapshot["estimated_tokens"]
        );
        assert!(
            snapshot["note"]
                .as_str()
                .unwrap()
                .contains("raw evidence observed in traces")
        );
    }

    #[test]
    fn context_assembly_ledger_explains_components_and_deltas() {
        let messages = vec![
            LlmMessage::system("system"),
            LlmMessage::user("task"),
            LlmMessage {
                role: MessageRole::Assistant,
                content: Some("reading".to_string()),
                tool_calls: Some(vec![LlmToolCall {
                    id: Some("call-1".to_string()),
                    name: "read_file".to_string(),
                    arguments: std::collections::HashMap::new(),
                }]),
                image_paths: None,
            },
            LlmMessage {
                role: MessageRole::Tool,
                content: Some("{\"content\":\"file\"}".to_string()),
                tool_calls: None,
                image_paths: None,
            },
        ];
        let config = CompletionConfig {
            temperature: 0.2,
            max_tool_iterations: 50,
            ..Default::default()
        };

        let ledger = context_assembly_ledger(ContextAssemblyInput {
            model: "qwen-test",
            turn: 3,
            llm_call_depth: 1,
            messages: &messages,
            tools: &[],
            completion_config: &config,
            context_window_tokens: Some(100),
            previous_call_total_chars: Some(10),
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
        });

        assert_eq!(ledger.turn, 3);
        assert_eq!(ledger.llm_call_depth, 1);
        assert_eq!(ledger.components[0].inclusion_reason, "base_system_prompt");
        assert_eq!(
            ledger.components[1].inclusion_reason,
            "benchmark_task_and_run_instructions"
        );
        assert_eq!(
            ledger.components[2].inclusion_reason,
            "retained_assistant_tool_request"
        );
        assert_eq!(
            ledger.components[3].inclusion_reason,
            "retained_raw_tool_result"
        );
        assert!(ledger.delta_chars_from_previous_call.unwrap() > 0);
        assert_ne!(ledger.pressure_band, "unknown");
    }

    #[test]
    fn compact_retained_tool_results_summarizes_old_large_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let mut messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];
        for index in 0..=RETAIN_RAW_TOOL_RESULT_RECENT_COUNT {
            messages.push(LlmMessage {
                role: MessageRole::Tool,
                content: Some("x".repeat(RETAIN_RAW_TOOL_RESULT_MAX_CHARS + 100 + index)),
                tool_calls: Some(vec![LlmToolCall {
                    id: Some(format!("call-{index}")),
                    name: "read_file".to_string(),
                    arguments: HashMap::new(),
                }]),
                image_paths: None,
            });
        }

        compact_retained_tool_results(
            &mut messages,
            &[],
            &trace,
            1,
            2,
            TranscriptPolicy::SummarizedTranscript,
            Some(1_000),
        )
        .unwrap();

        let first_tool = messages
            .iter()
            .find(|message| message.role == MessageRole::Tool)
            .unwrap();
        assert!(
            first_tool
                .content
                .as_deref()
                .unwrap()
                .starts_with(TOOL_RESULT_SUMMARY_PREFIX)
        );
        let raw_tool_count = messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::Tool
                    && !message
                        .content
                        .as_deref()
                        .unwrap_or_default()
                        .starts_with(TOOL_RESULT_SUMMARY_PREFIX)
            })
            .count();
        assert_eq!(raw_tool_count, RETAIN_RAW_TOOL_RESULT_RECENT_COUNT);
        let ledger = context_assembly_ledger(ContextAssemblyInput {
            model: "test",
            turn: 1,
            llm_call_depth: 3,
            messages: &messages,
            tools: &[],
            completion_config: &CompletionConfig::default(),
            context_window_tokens: Some(1_000),
            previous_call_total_chars: None,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
        });
        assert!(
            ledger
                .components
                .iter()
                .any(|component| component.inclusion_reason == "retained_summarized_tool_result")
        );
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.context_assembly.tool_result_compacted\""));
        assert!(content.contains("\"tool_name\":\"read_file\""));
    }

    #[test]
    fn full_transcript_policy_keeps_large_tool_results_raw() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let mut messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some("x".repeat(RETAIN_RAW_TOOL_RESULT_MAX_CHARS + 100)),
            tool_calls: Some(vec![LlmToolCall {
                id: Some("call-1".to_string()),
                name: "read_file".to_string(),
                arguments: HashMap::new(),
            }]),
            image_paths: None,
        });

        compact_retained_tool_results(
            &mut messages,
            &[],
            &trace,
            1,
            2,
            TranscriptPolicy::FullTranscript,
            Some(1_000),
        )
        .unwrap();

        assert!(
            !messages[2]
                .content
                .as_deref()
                .unwrap()
                .starts_with(TOOL_RESULT_SUMMARY_PREFIX)
        );
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(!content.contains("llm.context_assembly.tool_result_compacted"));
    }

    #[test]
    fn validation_repair_packet_preserves_latest_failed_validation() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let mut messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some("old-read".repeat(1_000)),
            tool_calls: Some(vec![LlmToolCall {
                id: Some("call-1".to_string()),
                name: "read_file".to_string(),
                arguments: HashMap::new(),
            }]),
            image_paths: None,
        });
        let failed_validation = serde_json::json!({
            "validation_probe": true,
            "success": false,
            "repair_required": { "command": "cargo test" },
            "stdout": "failure".repeat(1_000)
        })
        .to_string();
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(failed_validation),
            tool_calls: Some(vec![LlmToolCall {
                id: Some("call-2".to_string()),
                name: "shell_command".to_string(),
                arguments: HashMap::new(),
            }]),
            image_paths: None,
        });
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some("latest-small".to_string()),
            tool_calls: Some(vec![LlmToolCall {
                id: Some("call-3".to_string()),
                name: "read_file".to_string(),
                arguments: HashMap::new(),
            }]),
            image_paths: None,
        });

        compact_retained_tool_results(
            &mut messages,
            &[],
            &trace,
            1,
            2,
            TranscriptPolicy::ValidationRepairPacket,
            Some(1_000),
        )
        .unwrap();

        assert!(
            messages[2]
                .content
                .as_deref()
                .unwrap()
                .starts_with(TOOL_RESULT_SUMMARY_PREFIX)
        );
        assert!(
            messages[3]
                .content
                .as_deref()
                .unwrap()
                .contains("\"validation_probe\":true")
        );
    }

    #[test]
    fn summarized_repair_handoff_uses_repair_packet_compaction_only_under_red_pressure() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let failed_validation = serde_json::json!({
            "validation_probe": true,
            "success": false,
            "repair_required": { "command": "cargo test" },
            "stdout": "failure".repeat(2_000)
        })
        .to_string();
        let mut messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];
        for index in 0..=RETAIN_RAW_TOOL_RESULT_RECENT_COUNT {
            messages.push(LlmMessage {
                role: MessageRole::Tool,
                content: Some(format!("old-read-{index}").repeat(1_200)),
                tool_calls: Some(vec![LlmToolCall {
                    id: Some(format!("call-{index}")),
                    name: "read_file".to_string(),
                    arguments: HashMap::new(),
                }]),
                image_paths: None,
            });
        }
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(failed_validation),
            tool_calls: Some(vec![LlmToolCall {
                id: Some("validation-call".to_string()),
                name: "shell_command".to_string(),
                arguments: HashMap::new(),
            }]),
            image_paths: None,
        });

        compact_retained_tool_results(
            &mut messages,
            &[],
            &trace,
            4,
            8,
            TranscriptPolicy::SummarizedRepairHandoff,
            Some(1_000),
        )
        .unwrap();

        let raw_tool_count = messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::Tool
                    && !message
                        .content
                        .as_deref()
                        .unwrap_or_default()
                        .starts_with(TOOL_RESULT_SUMMARY_PREFIX)
            })
            .count();
        assert_eq!(raw_tool_count, REPAIR_HANDOFF_RAW_TOOL_RESULT_RECENT_COUNT);
        assert!(
            messages
                .last()
                .and_then(|message| message.content.as_deref())
                .is_some_and(|content| content.contains("\"validation_probe\":true"))
        );
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.context_assembly.validation_repair_handoff\""));
        assert!(content.contains("\"effective_repair_handoff\":true"));
    }

    #[test]
    fn pressure_bands_are_trace_visible() {
        assert_eq!(pressure_band(None), "unknown");
        assert_eq!(pressure_band(Some(0.10)), "green");
        assert_eq!(pressure_band(Some(0.20)), "yellow");
        assert_eq!(pressure_band(Some(0.30)), "orange");
        assert_eq!(pressure_band(Some(0.50)), "red");
    }

    #[test]
    fn gpu_activity_keeps_quiet_runner_progress_unknown() {
        let activity = RunnerActivitySample {
            source: "test".to_string(),
            process_active: Some(true),
            model_loaded: Some(true),
            accelerator_resident: Some(true),
            accelerator_label: Some("100% GPU".to_string()),
            gpu_utilization_percent: None,
            raw_summary: Some("model 100% GPU".to_string()),
            error: None,
        };

        let state = classify_model_progress(
            ModelProgressState::WaitingForFirstToken,
            Duration::from_secs(10_000),
            Duration::from_secs(10_000),
            30.0,
            Some(&activity),
            STALLED_CONFIRMATION_CHECKS,
        );

        assert_eq!(state, ModelProgressState::ProgressUnknown);
    }

    #[test]
    fn quiet_runner_requires_repeated_checks_before_stalled() {
        let first_check = classify_model_progress(
            ModelProgressState::WaitingForFirstToken,
            Duration::from_secs(90),
            Duration::from_secs(90),
            30.0,
            None,
            0,
        );
        let second_check = classify_model_progress(
            first_check,
            Duration::from_secs(120),
            Duration::from_secs(120),
            30.0,
            None,
            1,
        );

        assert_eq!(first_check, ModelProgressState::PossiblyStalled);
        assert_eq!(second_check, ModelProgressState::Stalled);
    }

    #[test]
    fn progress_status_records_automatic_interrupt() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let projection = ModelProgressProjection {
            conservative_tokens_per_second: 1.0,
            sample_count: 0,
            expected_max_seconds: 30.0,
            allowed_seconds: 60.0,
        };

        trace_model_progress_status(ModelProgressStatusInput {
            trace: &trace,
            turn: 1,
            llm_call_depth: 2,
            model: "test-model",
            progress_state: ModelProgressState::Stalled,
            elapsed: Duration::from_secs(61),
            seconds_since_observable_progress: 61.0,
            projection: &projection,
            runner_activity: None,
            stalled_candidate_checks: STALLED_CONFIRMATION_CHECKS,
            automatic_interrupt: true,
        })
        .unwrap();

        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"progress_state\":\"Stalled\""));
        assert!(content.contains("\"automatic_interrupt\":true"));
        assert!(content.contains("\"projected_allowance_exceeded\":true"));
    }

    #[tokio::test]
    async fn stalled_stream_interrupts_after_confirmed_stalled() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let config = CompletionConfig {
            temperature: 0.2,
            max_tool_iterations: 1,
            ..Default::default()
        };
        let error = stream_response(StreamResponseRequest {
            gateway: &NeverYieldGateway,
            model: "fake-stalled-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: config,
            context_window_tokens: Some(1_000),
            packet_type: "diagnosis-only",
            expected_output_tokens: 1,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: Some(ModelProgressProjection {
                conservative_tokens_per_second: 1000.0,
                sample_count: 1,
                expected_max_seconds: 0.01,
                allowed_seconds: 0.01,
            }),
            progress_status_interval_override: Some(Duration::from_millis(50)),
            runner_activity_override: Some(RunnerActivitySample {
                source: "test".to_string(),
                process_active: Some(false),
                model_loaded: Some(false),
                accelerator_resident: Some(false),
                accelerator_label: None,
                gpu_utilization_percent: None,
                raw_summary: None,
                error: None,
            }),
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains("interrupted stalled model call"));
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.progress.interrupted\""));
        assert!(content.contains("\"progress_state\":\"Stalled\""));
        assert!(content.contains("\"automatic_interrupt\":true"));
    }

    #[tokio::test]
    async fn stream_response_hard_stops_repeated_pre_validation_reads() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(workspace.join("src/lib.rs"), "fn main() {}\n").unwrap();
        let trace = Arc::new(TraceRecorder::create(&temp.path().join("traces")).unwrap());
        let scope = ToolScope::new(workspace, Arc::clone(&trace)).unwrap();
        let tools = coding_tools(&scope);
        let read_args = HashMap::from([
            ("path".to_string(), json!("src/lib.rs")),
            ("line_start".to_string(), json!(1)),
            ("line_end".to_string(), json!(20)),
        ]);
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk("read_file", read_args.clone())],
            vec![tool_call_chunk("read_file", read_args.clone())],
            vec![tool_call_chunk("read_file", read_args.clone())],
            vec![tool_call_chunk("read_file", read_args)],
        ]);

        let error = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &tools,
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 10,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("pre-validation inspection loop detected")
        );
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.inspection_loop.hard_failed\""));
        assert!(content.contains("read_file:src/lib.rs:1-20"));
    }

    #[tokio::test]
    async fn stream_response_hard_stops_no_content_stream_runaway() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Metrics(stream_metrics(
            (NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER + 1) as u64,
        ))]]);

        let error = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 1,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 1,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains("no assistant content"));
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.no_content_stream.hard_failed\""));
    }

    #[tokio::test]
    async fn stream_response_hard_stops_thinking_only_stream_runaway() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Thinking(
            "I am planning concrete edits but not emitting a tool call.".to_string(),
        )]]);

        let error = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 1,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: 1,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains("thinking-only stream exceeded"));
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.stream.thinking\""));
        assert!(content.contains("\"kind\":\"llm.thinking_only_stream.hard_failed\""));
        assert!(content.contains("\"max_thinking_only_tokens\":1"));
        assert!(content.contains("\"content_chars\":0"));
        assert!(content.contains("\"tool_call_count\":0"));
    }

    #[tokio::test]
    async fn stream_response_interrupts_pre_validation_action_boundary() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ScriptedGateway::new(vec![vec![
            StreamChunk::Thinking("Let me write the first file. ".to_string()),
            StreamChunk::Thinking("x".repeat(ACTION_BOUNDARY_INTENT_HIT_GAP_TOKENS * 4)),
            StreamChunk::Thinking("I will use write_file for src/lib.rs. ".to_string()),
        ]]);

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 1,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 1,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        assert_eq!(result.response, "");
        assert!(result.action_boundary_interrupted.is_some());
        assert!(!result.repair_no_content_interrupted);
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.action_boundary.interrupted\""));
        assert!(content.contains("\"action_intent_hits\":2"));
        assert!(!content.contains("\"kind\":\"llm.thinking_only_stream.hard_failed\""));
    }

    #[tokio::test]
    async fn stream_response_detects_action_boundary_across_split_thinking_chunks() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ScriptedGateway::new(vec![vec![
            StreamChunk::Thinking("I".to_string()),
            StreamChunk::Thinking("'m".to_string()),
            StreamChunk::Thinking(" going".to_string()),
            StreamChunk::Thinking(" to".to_string()),
            StreamChunk::Thinking(" write".to_string()),
            StreamChunk::Thinking(" src/lib.rs".to_string()),
            StreamChunk::Thinking("x".repeat(ACTION_BOUNDARY_INTENT_HIT_GAP_TOKENS * 4)),
            StreamChunk::Thinking(" before calling write_file.".to_string()),
        ]]);

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 1,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 1,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        assert_eq!(result.response, "");
        assert!(result.action_boundary_interrupted.is_some());
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.action_boundary.interrupted\""));
        assert!(content.contains("\"action_intent_hits\":2"));
    }

    #[tokio::test]
    async fn stream_response_interrupts_validation_repair_thinking_exit() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Thinking(
            "I am still considering the same repair without choosing a patch or probe.".to_string(),
        )]]);

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 1,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 1,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: true,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 5,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        assert_eq!(result.response, "");
        assert!(result.repair_no_content_interrupted);
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.validation.repair_exit_interrupted\""));
        assert!(content.contains("\"repair_exit_thinking_tokens\":1"));
        assert!(!content.contains("\"kind\":\"llm.thinking_only_stream.hard_failed\""));
    }

    #[tokio::test]
    async fn stream_response_interrupts_validation_repair_no_content_progress() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let progress_frames = (0..REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT)
            .map(|index| StreamChunk::Progress(stream_progress(index, 0, 0, 0)))
            .collect::<Vec<_>>();
        let gateway = ScriptedGateway::new(vec![progress_frames]);

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &[],
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 1,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: true,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 5,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        assert_eq!(result.response, "");
        assert!(result.repair_no_content_interrupted);
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.validation.repair_no_content_interrupted\""));
        assert!(content.contains("\"validation_repair_active\":true"));
        assert!(content.contains("\"call_stream_progress_frame_count\":1024"));
        assert!(!content.contains("\"kind\":\"llm.no_content_stream.hard_failed\""));
    }

    #[tokio::test]
    async fn stream_response_hard_stops_validation_repair_at_depth_limit() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(EchoTool)];
        let gateway = ScriptedGateway::new(
            (0..MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH)
                .map(|index| {
                    vec![tool_call_chunk(
                        "echo",
                        HashMap::from([("value".to_string(), json!(format!("step-{index}")))]),
                    )]
                })
                .collect(),
        );

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &tools,
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH + 5,
                ..Default::default()
            },
            context_window_tokens: Some(131_072),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: true,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 5,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        let decision = result.repair_depth_hard_stop.unwrap();
        assert!(matches!(
            decision.reason,
            RepairDepthReason::MaxLlmCallDepth
        ));
        assert_eq!(
            decision.llm_call_depth,
            MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH
        );
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.validation.repair_depth_hard_failed\""));
        assert!(content.contains("\"reason\":\"max_llm_call_depth\""));
    }

    #[tokio::test]
    async fn stream_response_hard_stops_validation_repair_red_context_after_action() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(EchoTool)];
        let gateway = ScriptedGateway::new(vec![vec![tool_call_chunk(
            "echo",
            HashMap::from([("value".to_string(), json!("large enough"))]),
        )]]);

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &tools,
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 5,
                ..Default::default()
            },
            context_window_tokens: Some(1),
            packet_type: "multi-file-patch",
            expected_output_tokens: 4_096,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: true,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 6,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        let decision = result.repair_depth_hard_stop.unwrap();
        assert!(matches!(
            decision.reason,
            RepairDepthReason::RedContextAfterRepairAction
        ));
        assert_eq!(decision.llm_call_depth, 1);
        assert_eq!(decision.pressure_band, "red");
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"agent.validation.repair_depth_hard_failed\""));
        assert!(content.contains("\"reason\":\"red_context_after_repair_action\""));
    }

    #[tokio::test]
    async fn stream_response_hard_stops_later_no_content_segment_after_tool_work() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(EchoTool)];
        let gateway = ScriptedGateway::new(vec![
            vec![
                StreamChunk::Content("I will inspect first.".to_string()),
                tool_call_chunk("echo", HashMap::from([("value".to_string(), json!("ok"))])),
            ],
            vec![StreamChunk::Metrics(stream_metrics(
                (NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER + 1) as u64,
            ))],
        ]);

        let error = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &[LlmMessage::system("system"), LlmMessage::user("task")],
            tools: &tools,
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 5,
                ..Default::default()
            },
            context_window_tokens: Some(8_000),
            packet_type: "multi-file-patch",
            expected_output_tokens: 1,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("since the latest content or tool call")
        );
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.no_content_stream.hard_failed\""));
        assert!(content.contains("\"turn_content_chars\":21"));
        assert!(content.contains("\"call_content_chars\":0"));
    }

    #[test]
    fn parses_ollama_gpu_residency() {
        let parsed = parse_ollama_ps(
            "qwen3.6:27b-coding-mxfp8",
            "NAME ID SIZE PROCESSOR UNTIL\nqwen3.6:27b-coding-mxfp8 abc 27 GB 100% GPU 4 minutes from now\n",
        );

        assert!(parsed.model_loaded);
        assert_eq!(parsed.accelerator_resident, Some(true));
        assert_eq!(parsed.accelerator_label.as_deref(), Some("100% GPU"));
    }

    #[test]
    fn parses_macmon_gpu_usage_fraction_as_percent() {
        let output = r#"{"gpu_usage":[338,0.08696959912776947],"gpu_power":0.4}"#;

        let utilization = parse_macmon_gpu_utilization(output);

        assert!(matches!(utilization, Some(value) if (value - 8.696959912776947).abs() < 0.0001));
    }

    #[test]
    fn parses_macmon_gpu_usage_percent_without_scaling() {
        let output = r#"{"gpu_usage":[338,42.5],"gpu_power":0.4}"#;

        let utilization = parse_macmon_gpu_utilization(output);

        assert_eq!(utilization, Some(42.5));
    }

    #[tokio::test]
    async fn stream_response_emits_per_llm_call_context_ledger() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = FakeToolGateway;
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(EchoTool)];
        let messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];
        let config = CompletionConfig {
            temperature: 0.2,
            max_tool_iterations: 5,
            ..Default::default()
        };

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &messages,
            tools: &tools,
            completion_config: config,
            context_window_tokens: Some(1_000),
            packet_type: "narrow-patch",
            expected_output_tokens: 2_048,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();
        let content = std::fs::read_to_string(trace.path()).unwrap();

        assert_eq!(result.response, "DONE");
        assert!(
            result
                .messages
                .iter()
                .any(|message| message.role == MessageRole::Tool)
        );
        assert_eq!(
            content
                .matches("\"kind\":\"llm.context_assembly.ledger\"")
                .count(),
            2
        );
        assert!(content.contains("\"kind\":\"llm.context_assembly.appended\""));
        assert!(content.contains("\"component\":\"tool_result\""));
        assert!(content.contains("\"kind\":\"llm.context_assembly.response\""));
        assert!(content.contains("\"kind\":\"llm.progress.status\""));
        assert!(content.contains("\"progress_state\":\"WaitingForFirstToken\""));
        assert!(content.contains("\"assembly_policy\":\"append_summarized_tool_transcript\""));
    }

    #[tokio::test]
    async fn stream_response_returns_tool_transcript_for_tool_only_turn() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ToolOnlyGateway;
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(EchoTool)];
        let messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &messages,
            tools: &tools,
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 5,
                ..Default::default()
            },
            context_window_tokens: Some(1_000),
            packet_type: "narrow-patch",
            expected_output_tokens: 2_048,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        assert_eq!(result.response, "");
        assert_eq!(result.messages.len(), 4);
        assert_eq!(result.messages[2].role, MessageRole::Assistant);
        assert_eq!(result.messages[3].role, MessageRole::Tool);
        assert!(
            result.messages[3]
                .content
                .as_deref()
                .unwrap()
                .contains("hello")
        );
    }

    #[tokio::test]
    async fn stream_response_preserves_call_boundary_before_final_status() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ContentThenToolGateway;
        let tools: Vec<Box<dyn LlmTool>> = vec![Box::new(EchoTool)];
        let messages = vec![LlmMessage::system("system"), LlmMessage::user("task")];

        let result = stream_response(StreamResponseRequest {
            gateway: &gateway,
            model: "fake-model",
            messages: &messages,
            tools: &tools,
            completion_config: CompletionConfig {
                temperature: 0.2,
                max_tool_iterations: 5,
                ..Default::default()
            },
            context_window_tokens: Some(1_000),
            packet_type: "narrow-patch",
            expected_output_tokens: 2_048,
            max_thinking_only_tokens: usize::MAX,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            validation_repair_active: false,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
            requested_validation_commands: &[],
            requested_validation_pending_after_write: false,
            requested_validation_ledger: RequestedValidationLedger::new(Vec::new()),
        })
        .await
        .unwrap();

        assert_eq!(result.response, "Validation passed: ok\nDONE");
        assert!(is_terminal_response(&result.response));
    }

    #[test]
    fn terminal_response_detection_requires_status_token() {
        assert!(is_terminal_response("DONE"));
        assert!(is_terminal_response(" done \n"));
        assert!(is_terminal_response("All tests pass.\n\nDONE"));
        assert!(is_terminal_response(
            "FAIL compiler cannot resolve dependency"
        ));
        assert!(!is_terminal_response(
            "Several tests are failing; let me inspect them."
        ));
        assert!(!is_terminal_response(
            "I am done editing, now I will validate."
        ));
    }

    #[test]
    fn fail_response_detection_only_checks_first_status_token() {
        assert!(is_fail_response("FAIL compiler cannot resolve dependency"));
        assert!(is_fail_response(" fail \n"));
        assert!(!is_fail_response("Tests still fail; I will inspect."));
        assert!(!is_fail_response("DONE"));
    }

    #[test]
    fn validation_repair_prompt_is_skipped_for_terminal_status() {
        let repair = ValidationRepairSnapshot {
            active: true,
            command: "cargo clippy --all-targets".to_string(),
            command_family: "cargo clippy".to_string(),
            status: Some(101),
            failure_text: "warning: length comparison to zero".to_string(),
            failure_details: Vec::new(),
            repeated_command_family_count: 1,
            repeated_failure_summary_count: 1,
        };
        let policy = repair_policy_snapshot(3, 3, Some(repair), BTreeMap::new());

        assert!(!should_prompt_validation_repair(
            &policy,
            "All validation passed.\nDONE"
        ));
        assert!(!should_prompt_validation_repair(
            &policy,
            "FAIL dependency unavailable"
        ));
        assert!(should_prompt_validation_repair(
            &policy,
            "I will inspect the failing clippy warning."
        ));
    }

    #[test]
    fn successful_validation_terminalization_requires_new_passing_validation() {
        let before = repair_policy_snapshot(1, 0, None, BTreeMap::new());
        let mut after = repair_policy_snapshot(1, 1, None, BTreeMap::new());
        after.latest_successful_validation_after_write = Some(SuccessfulValidationSnapshot {
            command: "cargo test focused".to_string(),
            command_family: "cargo test".to_string(),
            status: Some(0),
            total_shell_probes: 1,
            total_write_operations: 1,
        });

        let decision = should_terminalize_after_successful_validation(&before, &after, &[]);

        assert_eq!(
            decision.map(|validation| validation.command),
            Some("cargo test focused".to_string())
        );

        let mut dirty_after = after.clone();
        dirty_after.validation_required_after_write = true;
        assert!(
            should_terminalize_after_successful_validation(&before, &dirty_after, &[]).is_none()
        );

        let mut stale_before = before;
        stale_before.total_shell_probes = 1;
        assert!(
            should_terminalize_after_successful_validation(&stale_before, &after, &[]).is_none()
        );
    }

    // `requested_validation_parser_extracts_shell_validation_commands` and
    // `requested_validation_parser_ignores_masked_success_commands` moved to
    // `contract.rs` alongside `requested_validation_commands` itself (see
    // GENERALIZATION_PLAN.md Slice 2).

    #[test]
    fn validation_matching_rejects_masked_success_commands() {
        let requested = vec!["cargo build".to_string()];

        assert!(!validation_matches_requested_command(
            "cargo build 2>&1 || true",
            &requested
        ));
        assert!(!validation_matches_requested_command(
            "cargo build || exit 0",
            &requested
        ));
        assert!(!validation_matches_requested_command(
            "cargo build || :",
            &requested
        ));
        assert!(!validation_matches_requested_command(
            "cargo build ; true",
            &requested
        ));
        assert!(validation_matches_requested_command(
            "cargo build 2>&1",
            &requested
        ));
    }

    #[test]
    fn successful_validation_terminalization_requires_requested_command_match() {
        let before = repair_policy_snapshot(1, 0, None, BTreeMap::new());
        let requested = vec![
            "cargo test test_deterministic_simulation_terminates_and_reports_summary".to_string(),
        ];
        let mut after = repair_policy_snapshot(1, 1, None, BTreeMap::new());
        after.latest_successful_validation_after_write = Some(SuccessfulValidationSnapshot {
            command: "cargo build 2>&1".to_string(),
            command_family: "cargo build".to_string(),
            status: Some(0),
            total_shell_probes: 1,
            total_write_operations: 1,
        });

        assert!(
            should_terminalize_after_successful_validation(&before, &after, &requested).is_none()
        );

        after.latest_successful_validation_after_write = Some(SuccessfulValidationSnapshot {
            command: "cargo test test_deterministic_simulation_terminates_and_reports_summary 2>&1"
                .to_string(),
            command_family: "cargo test".to_string(),
            status: Some(0),
            total_shell_probes: 1,
            total_write_operations: 1,
        });

        assert_eq!(
            should_terminalize_after_successful_validation(&before, &after, &requested)
                .map(|validation| validation.command),
            Some(
                "cargo test test_deterministic_simulation_terminates_and_reports_summary 2>&1"
                    .to_string()
            )
        );
    }

    #[test]
    fn successful_validation_terminalization_rejects_masked_success_commands() {
        let before = repair_policy_snapshot(1, 0, None, BTreeMap::new());
        let requested = vec!["cargo build".to_string()];
        let mut after = repair_policy_snapshot(1, 1, None, BTreeMap::new());
        after.latest_successful_validation_after_write = Some(SuccessfulValidationSnapshot {
            command: "cargo build 2>&1 || true".to_string(),
            command_family: "cargo build".to_string(),
            status: Some(0),
            total_shell_probes: 1,
            total_write_operations: 1,
        });

        assert!(
            should_terminalize_after_successful_validation(&before, &after, &requested).is_none()
        );
    }

    #[test]
    fn successful_validation_done_prompt_forbids_more_tools() {
        let prompt = successful_validation_done_prompt(&SuccessfulValidationSnapshot {
            command: "cargo test focused".to_string(),
            command_family: "cargo test".to_string(),
            status: Some(0),
            total_shell_probes: 1,
            total_write_operations: 1,
        });

        assert!(prompt.contains("has passed"));
        assert!(prompt.contains("Do not call any more tools"));
        assert!(prompt.contains("Reply exactly DONE"));
    }

    #[test]
    fn empty_response_decision_escalates_after_repeated_true_empty_turns() {
        let first = empty_response_decision(1);
        let third = empty_response_decision(EMPTY_RESPONSE_ESCALATION_TURNS);

        assert!(!first.escalation_required);
        assert!(first.prompt.contains("previous turn ended"));
        assert!(third.escalation_required);
        assert!(third.prompt.contains("Empty-response escalation is active"));
        assert!(third.prompt.contains("one bounded step"));
    }

    #[test]
    fn validation_repair_prompt_carries_failure_evidence() {
        let prompt = validation_repair_prompt(&ValidationRepairSnapshot {
            active: true,
            command: "cargo test".to_string(),
            command_family: "cargo test".to_string(),
            status: Some(101),
            failure_text: "error[E0425]: cannot find value".to_string(),
            failure_details: vec![
                "tests::invader_shot_removed_when_leaving_bottom_edge".to_string(),
                "thread 'tests::invader_shot_removed_when_leaving_bottom_edge' panicked at src/lib.rs:739:9:"
                    .to_string(),
            ],
            repeated_command_family_count: 2,
            repeated_failure_summary_count: 1,
        });

        assert!(prompt.contains("Failing command: cargo test"));
        assert!(prompt.contains("Failure text: error[E0425]: cannot find value"));
        assert!(prompt.contains("Failure details:"));
        assert!(prompt.contains("tests::invader_shot_removed_when_leaving_bottom_edge"));
        assert!(prompt.contains("src/lib.rs:739:9"));
        assert!(prompt.contains("Command family failure count: 2"));
        assert!(prompt.contains("Validation repair action contract is active"));
        assert!(prompt.contains("exactly one targeted repair action"));
        assert!(prompt.contains("one focused source edit"));
        assert!(!prompt.contains("bounded write"));
        assert!(!prompt.contains("patch/write_file"));
        assert!(prompt.contains("one deterministic diagnostic probe"));
        assert!(prompt.contains("reply FAIL with a concrete blocker"));
        assert!(prompt.contains("Do not emit a text-only repair plan"));
    }

    #[test]
    fn repair_no_action_escalation_prompt_preserves_failure_evidence() {
        let repair = ValidationRepairSnapshot {
            active: true,
            command: "cargo clippy --all-targets".to_string(),
            command_family: "cargo clippy".to_string(),
            status: Some(101),
            failure_text: "error[E0422]: cannot find struct `TextStyle`".to_string(),
            failure_details: vec!["src/main.rs:12:5".to_string()],
            repeated_command_family_count: 1,
            repeated_failure_summary_count: 1,
        };
        let second = RepairNoActionDecision {
            turn: 7,
            tool_calls_this_turn: 1,
            reason: RepairNoActionReason::NoRepairAction,
            consecutive_no_action_turns: 2,
            escalation_required: true,
            active_repair: repair,
            validation_repair_read_paths: BTreeMap::from([("src/main.rs".to_string(), 3)]),
            total_write_operations_before_turn: 3,
            total_write_operations_after_turn: 3,
            total_shell_probes_before_turn: 2,
            total_shell_probes_after_turn: 2,
        };

        assert_eq!(second.consecutive_no_action_turns, 2);
        assert!(second.escalation_required);
        assert!(matches!(
            second.reason,
            RepairNoActionReason::NoRepairAction
        ));
        assert_eq!(second.validation_repair_read_paths["src/main.rs"], 3);

        let prompt = validation_repair_no_action_prompt(&second);
        assert!(prompt.contains("Validation repair escalation is active"));
        assert!(prompt.contains("Failure details:"));
        assert!(prompt.contains("src/main.rs:12:5"));
        assert!(prompt.contains("src/main.rs (3)"));
        assert!(prompt.contains("exactly one targeted repair action"));
        assert!(prompt.contains("apply one focused structured edit with edit_file"));
        assert!(prompt.contains("write_file only after reading the complete file"));
        assert!(!prompt.contains("bounded write"));
        assert!(!prompt.contains("patch/write_file"));
        assert!(prompt.contains("Do not emit a text-only repair plan"));
    }

    #[test]
    fn repair_no_action_failure_summary_names_hard_stop_reason() {
        let summary = repair_no_action_failure_summary(8);

        assert_eq!(
            summary,
            "turn 8 made no validation-repair edit or probe after validation failure"
        );
    }

    fn repair_policy_snapshot(
        total_write_operations: usize,
        total_shell_probes: usize,
        validation_repair: Option<ValidationRepairSnapshot>,
        validation_repair_read_paths: BTreeMap<String, usize>,
    ) -> ToolPolicySnapshot {
        ToolPolicySnapshot {
            total_tool_calls: 0,
            consecutive_writes_without_shell: 0,
            writes_since_shell_probe: 0,
            writes_since_shell_probe_paths: BTreeMap::new(),
            validation_required_after_write: false,
            total_write_operations,
            total_shell_probes,
            validation_repair,
            validation_repair_read_paths,
            latest_successful_validation_after_write: None,
            patch_fallbacks: vec![],
            total_tool_result_chars: 0,
            total_tool_result_estimated_tokens: 0,
            max_tool_result_chars: 0,
            max_tool_result_estimated_tokens: 0,
            max_tool_result_kind: None,
            tool_result_chars_by_kind: BTreeMap::new(),
        }
    }

    #[test]
    fn run_failed_trace_records_transport_error_context() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let config = AgentRunConfig {
            experiment_dir: temp.path().join("experiment"),
            goal_file: PathBuf::from("task.md"),
            model: "qwen3.6:27b-coding-mxfp8".to_string(),
            max_iterations: 10,
            max_tool_iterations: 50,
            context_window_tokens: Some(131_072),
            packet_type: "multi-file-patch".to_string(),
            expected_output_tokens: 4_096,
            num_predict: None,
            max_thinking_only_tokens: 4_096,
            repair_exit_thinking_tokens: 16_384,
            action_boundary_interrupt_tokens: 0,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
        };
        let error = anyhow::anyhow!("HTTP error: unexpected EOF during chunk size line");

        trace_run_failed(
            &trace,
            "agent.stream",
            Some(1),
            &error,
            &config,
            &temp.path().join("workspace"),
            &temp.path().join("experiment/task.md"),
        )
        .unwrap();

        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"run.failed\""));
        assert!(content.contains("agent.stream"));
        assert!(content.contains("unexpected EOF during chunk size line"));
        assert!(content.contains("qwen3.6:27b-coding-mxfp8"));
    }

    #[tokio::test]
    async fn fixture_retains_tool_only_turn_context_across_outer_turns() {
        let fixture = AgentFixture::new("Map the workspace and finish.");
        std::fs::write(fixture.workspace.join("src.txt"), "hello").unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk("list_tree", HashMap::new())],
            vec![],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 3).await;

        assert_eq!(summary.final_summary, "DONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.turn.tool_only_response\""));
        let turn_two_ledgers = trace_payloads(&trace, "llm.context_assembly.ledger")
            .into_iter()
            .filter(|payload| payload["turn"] == 2 && payload["llm_call_depth"] == 0)
            .collect::<Vec<_>>();
        assert_eq!(turn_two_ledgers.len(), 1, "{turn_two_ledgers:?}");
        assert_eq!(turn_two_ledgers[0]["message_count"], 5);
        assert_eq!(turn_two_ledgers[0]["role_counts"]["tool"], 1);
    }

    #[tokio::test]
    async fn fixture_escalates_repeated_true_empty_responses() {
        let fixture = AgentFixture::new("Finish without tools.");
        let gateway = ScriptedGateway::new(vec![
            vec![],
            vec![],
            vec![],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 4).await;

        assert_eq!(
            summary.final_summary,
            "turn 3 produced 3 consecutive empty responses with no tool calls or final text"
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.turn.empty_response_escalated\""));
        assert!(trace.contains("\"kind\":\"agent.turn.empty_response_hard_failed\""));
        assert!(trace.contains("\"consecutive_empty_responses\":3"));
        // The hard-stop escalates at turn 3 of 4, so the scripted fourth
        // turn's `Content("DONE")` must never be reached: the harness
        // finishes on the escalation summary, not a clean "DONE". (The
        // trace does legitimately contain the literal string "DONE"
        // elsewhere now, as descriptive `terminal.done_token` metadata on
        // the resolved run contract traced before the turn loop starts —
        // see `agent.contract.resolved`. `run.finished` is emitted
        // unconditionally on every loop exit, including this hard-stop, so
        // it is not a useful discriminator here.)
        assert!(!trace.contains("\"final_summary\":\"DONE\""));
    }

    #[tokio::test]
    async fn fixture_traces_thinking_only_response_separately_from_empty_stream() {
        let fixture = AgentFixture::new("Finish after thinking.");
        let gateway = ScriptedGateway::new(vec![
            vec![StreamChunk::Thinking(
                "Considering whether to edit files or answer DONE.".to_string(),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"llm.stream.thinking\""));
        assert!(trace.contains("\"kind\":\"agent.turn.thinking_only_response\""));
        assert!(trace.contains("\"thinking_chars_this_turn\":49"));
        assert!(trace.contains("Considering whether to edit files or answer DONE."));
    }

    #[tokio::test]
    async fn fixture_escalates_repeated_hidden_only_no_action_turns() {
        let fixture = AgentFixture::new("Inspect, then implement.");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk("list_tree", HashMap::new())],
            vec![StreamChunk::Thinking(
                "I should write src/lib.rs now, but I am still planning.".to_string(),
            )],
            vec![StreamChunk::Thinking(
                "I need to stop going in circles and write the code now.".to_string(),
            )],
            vec![tool_call_chunk(
                "write_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "content".to_string(),
                        json!("pub fn answer() -> u32 { 42 }\n"),
                    ),
                ]),
            )],
        ]);

        let summary = fixture.run(&gateway, 4).await;

        assert_eq!(
            summary.final_summary,
            "turn 2 produced 2 consecutive hidden-only no-action responses without source mutation, validation probe, or final text"
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.turn.hidden_only_no_action\""));
        assert!(trace.contains("\"kind\":\"agent.turn.hidden_only_no_action_escalated\""));
        assert!(trace.contains("\"kind\":\"agent.turn.hidden_only_no_action_hard_failed\""));
        assert!(trace.contains("\"tool_calls_this_turn\":1"));
        assert!(trace.contains("\"tool_calls_this_turn\":0"));
        assert!(!trace.contains("pub fn answer"));
    }

    #[tokio::test]
    async fn fixture_hard_stops_repeated_action_boundary_no_action() {
        let fixture = AgentFixture::new("Inspect, then implement.");
        let boundary_thinking = vec![
            StreamChunk::Thinking("I will write the source change now. ".to_string()),
            StreamChunk::Thinking("x".repeat(ACTION_BOUNDARY_INTENT_HIT_GAP_TOKENS * 4)),
            StreamChunk::Thinking("I will use write_file for src/lib.rs. ".to_string()),
        ];
        let gateway =
            ScriptedGateway::new(vec![boundary_thinking.clone(), boundary_thinking.clone()]);

        let summary = fixture
            .run_with_action_boundary_interrupt(&gateway, 3)
            .await;

        assert_eq!(
            summary.final_summary,
            "turn 2 produced 2 consecutive action-boundary interrupts without source mutation or validation probe"
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.action_boundary.no_action\""));
        assert!(trace.contains("\"kind\":\"agent.action_boundary.prompted\""));
        assert!(trace.contains("\"kind\":\"agent.action_boundary.hard_failed\""));
        assert!(trace.contains("\"consecutive_no_action_turns\":2"));
        assert!(!trace.contains("pub fn answer"));
    }

    #[tokio::test]
    async fn fixture_accepts_done_after_docs_only_write_following_validation() {
        let fixture = AgentFixture::new("Validate, write a README, and finish.");
        fixture.write_fake_cargo(0, "ok");
        let gateway = ScriptedGateway::new(vec![
            vec![StreamChunk::ToolCalls(vec![
                tool_call(
                    "shell_command",
                    HashMap::from([("command".to_string(), json!("./cargo test"))]),
                ),
                tool_call(
                    "write_file",
                    HashMap::from([
                        ("path".to_string(), json!("README.md")),
                        ("content".to_string(), json!("# Done\n")),
                    ]),
                ),
            ])],
            vec![StreamChunk::Content("Validation passed.\nDONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 3).await;

        assert_eq!(summary.final_summary, "Validation passed.\nDONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"command\":\"./cargo test\""));
        assert!(trace.contains("\"validation_probe\":true"));
        assert!(!trace.contains("\"kind\":\"agent.validation.required_after_edit\""));
        assert!(!trace.contains("\"turn\":2,\"max_iterations\""));
    }

    #[tokio::test]
    async fn fixture_records_edit_file_as_source_mutation() {
        let fixture = AgentFixture::new("Edit existing source, validate, and finish.");
        fixture.write_fake_cargo(0, "ok");
        std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
        std::fs::write(
            fixture.workspace.join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    1\n}\n",
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "edit_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "edits".to_string(),
                        json!([
                            {
                                "kind": "replace_exact",
                                "old": "    1\n",
                                "new": "    42\n"
                            }
                        ]),
                    ),
                ]),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 3).await;

        assert_eq!(summary.final_summary, "DONE");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("src/lib.rs")).unwrap(),
            "pub fn answer() -> u32 {\n    42\n}\n"
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"tool.edit_file\""));
        assert!(trace.contains("\"kind\":\"agent.stage.first_source_mutation\""));
        assert!(trace.contains("\"action\":\"write_intent\""));
    }

    #[tokio::test]
    async fn fixture_terminalizes_after_successful_post_write_validation() {
        let fixture = AgentFixture::new("Edit existing source, validate, and finish.");
        fixture.write_fake_cargo(0, "ok");
        std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
        std::fs::write(
            fixture.workspace.join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    1\n}\n",
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "edit_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "edits".to_string(),
                        json!([
                            {
                                "kind": "replace_exact",
                                "old": "    1\n",
                                "new": "    42\n"
                            }
                        ]),
                    ),
                ]),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        let tool_counts = gateway.tool_counts();
        assert!(
            tool_counts.len() >= 3,
            "expected at least three model calls, got {tool_counts:?}"
        );
        assert!(tool_counts[0] > 0, "{tool_counts:?}");
        assert!(tool_counts[1] > 0, "{tool_counts:?}");
        assert_eq!(tool_counts[2], 0, "{tool_counts:?}");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.validation.success_terminal_prompted\""));
        assert!(trace.contains("\"scope\":\"in_turn\""));
    }

    #[tokio::test]
    async fn fixture_waits_for_requested_validation_before_terminalizing() {
        let fixture = AgentFixture::new(
            "Edit existing source.\n\nRun:\n\n```sh\n./cargo test focused_summary\n```\n",
        );
        fixture.write_fake_cargo(0, "ok");
        std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
        std::fs::write(
            fixture.workspace.join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    1\n}\n",
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "edit_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "edits".to_string(),
                        json!([
                            {
                                "kind": "replace_exact",
                                "old": "    1\n",
                                "new": "    42\n"
                            }
                        ]),
                    ),
                ]),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo build"))]),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test focused_summary"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        let tool_counts = gateway.tool_counts();
        assert!(
            tool_counts.len() >= 4,
            "expected at least four model calls, got {tool_counts:?}"
        );
        assert!(tool_counts[0] > 0, "{tool_counts:?}");
        assert!(tool_counts[1] > 0, "{tool_counts:?}");
        assert!(tool_counts[2] > 0, "{tool_counts:?}");
        assert_eq!(tool_counts[3], 0, "{tool_counts:?}");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        let prompts = trace_payloads(&trace, "agent.validation.success_terminal_prompted");
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(
            prompts[0]["validation"]["command"],
            "./cargo test focused_summary"
        );
        assert!(
            trace.contains("\"requested_validation_commands\":[\"./cargo test focused_summary\"]")
        );
    }

    #[tokio::test]
    async fn fixture_rejects_done_until_all_requested_validation_passes() {
        let fixture = AgentFixture::new(
            "Edit existing source.\n\nRun:\n\n```sh\n./cargo build\n./cargo test\n```\n",
        );
        fixture.write_fake_cargo(0, "ok");
        std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
        std::fs::write(
            fixture.workspace.join("src/lib.rs"),
            "pub fn answer() -> u32 {\n    1\n}\n",
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "edit_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "edits".to_string(),
                        json!([
                            {
                                "kind": "replace_exact",
                                "old": "    1\n",
                                "new": "    42\n"
                            }
                        ]),
                    ),
                ]),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo build"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 3).await;

        assert_eq!(summary.final_summary, "DONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.validation.done_rejected\""));
        assert!(trace.contains("./cargo test"));
        let prompts = trace_payloads(&trace, "agent.validation.success_terminal_prompted");
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(prompts[0]["validation"]["command"], "./cargo test");
        let tool_counts = gateway.tool_counts();
        assert!(
            tool_counts.len() >= 5,
            "expected at least five model calls, got {tool_counts:?}"
        );
        assert!(tool_counts[2] > 0, "{tool_counts:?}");
        assert!(tool_counts[3] > 0, "{tool_counts:?}");
        assert_eq!(tool_counts[4], 0, "{tool_counts:?}");
    }

    #[tokio::test]
    async fn fixture_hard_stops_repeated_validation_repair_no_action() {
        let fixture = AgentFixture::new("Run validation and repair failures.");
        fixture.write_fake_cargo(1, "unit failed");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
            vec![StreamChunk::Content(
                "I will repair the failing test.".to_string(),
            )],
            vec![],
            vec![],
        ]);

        let summary = fixture.run(&gateway, 5).await;

        assert_eq!(
            summary.final_summary,
            "turn 3 made no validation-repair edit or probe after validation failure"
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.validation.repair_required\""));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_no_action\""));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_escalated\""));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_hard_failed\""));
        assert!(!trace.contains("\"turn\":4,\"max_iterations\""));
    }

    #[tokio::test]
    async fn fixture_hard_stops_repeated_validation_repair_no_content_interrupts() {
        let fixture = AgentFixture::new("Run validation and repair no-content failures.");
        fixture.write_fake_cargo(1, "compile failed");
        let first_interrupt_frames = (0..REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT)
            .map(|index| StreamChunk::Progress(stream_progress(index, 0, 0, 0)))
            .collect::<Vec<_>>();
        let second_interrupt_frames = (0..REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT)
            .map(|index| StreamChunk::Progress(stream_progress(index, 0, 0, 0)))
            .collect::<Vec<_>>();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
            vec![StreamChunk::Content(
                "I will repair the failing validation.".to_string(),
            )],
            first_interrupt_frames,
            second_interrupt_frames,
        ]);

        let summary = fixture.run(&gateway, 5).await;

        assert_eq!(
            summary.final_summary,
            "turn 3 validation repair produced no content or tool call after repeated interrupts"
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.validation.repair_no_content_interrupted\""));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_no_action\""));
        assert!(trace.contains("\"reason\":\"no_content_interrupted\""));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_hard_failed\""));
        assert!(!trace.contains("\"turn\":4,\"max_iterations\""));
    }

    #[tokio::test]
    async fn fixture_traces_stage_milestones() {
        let fixture = AgentFixture::new("Run validation, repair source, and probe again.");
        fixture.write_fake_cargo(1, "test failed");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
            vec![tool_call_chunk(
                "write_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "content".to_string(),
                        json!("pub fn answer() -> u32 { 42 }\n"),
                    ),
                ]),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("./cargo test"))]),
            )],
        ]);

        let summary = fixture.run(&gateway, 3).await;

        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.stage.first_validation_probe\""));
        assert!(trace.contains("\"kind\":\"agent.stage.first_source_mutation\""));
        assert!(trace.contains("\"kind\":\"agent.stage.first_post_validation_repair_action\""));
        assert!(trace.contains("\"action\":\"write_intent\""));
    }

    #[tokio::test]
    async fn fixture_records_pending_validation_when_max_iterations_exhausted() {
        let fixture = AgentFixture::new("Edit source once and stop.");
        std::fs::create_dir_all(fixture.workspace.join("src")).unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "write_file",
                HashMap::from([
                    ("path".to_string(), json!("src/lib.rs")),
                    (
                        "content".to_string(),
                        json!("pub fn answer() -> u32 { 42 }\n"),
                    ),
                ]),
            )],
            vec![StreamChunk::Content("I changed the source.".to_string())],
        ]);

        let summary = fixture.run(&gateway, 1).await;

        assert_eq!(summary.final_summary, "I changed the source.");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.validation.required_after_edit\""));
        assert!(
            trace.contains("\"kind\":\"agent.validation.required_after_edit_at_max_iterations\"")
        );
        assert!(trace.contains("\"kind\":\"run.finished\""));
    }

    struct AgentFixture {
        _temp: tempfile::TempDir,
        experiment: PathBuf,
        workspace: PathBuf,
    }

    impl AgentFixture {
        fn new(task: &str) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let experiment = temp.path().join("experiment");
            let workspace = temp.path().join("workspace");
            std::fs::create_dir_all(&experiment).unwrap();
            std::fs::create_dir_all(&workspace).unwrap();
            std::fs::write(experiment.join("task.md"), task).unwrap();
            Self {
                _temp: temp,
                experiment,
                workspace,
            }
        }

        fn write_fake_cargo(&self, status: i32, output: &str) {
            let script = format!("#!/bin/sh\necho {output:?}\nexit {status}\n");
            let path = self.workspace.join("cargo");
            std::fs::write(&path, script).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&path, permissions).unwrap();
            }
        }

        async fn run(&self, gateway: &ScriptedGateway, max_iterations: usize) -> AgentRunSummary {
            run_coding_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    goal_file: PathBuf::from("task.md"),
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens: 16_384,
                    action_boundary_interrupt_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }

        async fn run_with_action_boundary_interrupt(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
        ) -> AgentRunSummary {
            run_coding_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    goal_file: PathBuf::from("task.md"),
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: usize::MAX,
                    repair_exit_thinking_tokens: 16_384,
                    action_boundary_interrupt_tokens: 1,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }
    }

    fn tool_call_chunk(name: &str, arguments: HashMap<String, Value>) -> StreamChunk {
        StreamChunk::ToolCalls(vec![tool_call(name, arguments)])
    }

    fn tool_call(name: &str, arguments: HashMap<String, Value>) -> LlmToolCall {
        LlmToolCall {
            id: Some(format!("call-{name}")),
            name: name.to_string(),
            arguments,
        }
    }

    fn stream_metrics(eval_count: u64) -> StreamMetrics {
        StreamMetrics {
            provider: "test".to_string(),
            total_duration_ns: None,
            load_duration_ns: None,
            prompt_eval_count: None,
            prompt_eval_duration_ns: None,
            eval_count: Some(eval_count),
            eval_duration_ns: Some(1_000_000_000),
            tokens_per_second: Some(eval_count as f64),
        }
    }

    fn stream_progress(
        frame_index: usize,
        content_chars: usize,
        tool_call_count: usize,
        accumulated_tool_call_count: usize,
    ) -> mojentic::llm::gateway::StreamProgress {
        mojentic::llm::gateway::StreamProgress {
            provider: "test".to_string(),
            frame_index,
            done: false,
            content_chars,
            thinking_chars: 0,
            tool_call_count,
            accumulated_tool_call_count,
        }
    }

    fn trace_payloads(content: &str, kind: &str) -> Vec<Value> {
        content
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| event["kind"] == kind)
            .filter_map(|event| event.get("payload").cloned())
            .collect()
    }

    struct ScriptedGateway {
        streams: StdMutex<VecDeque<Vec<StreamChunk>>>,
        tool_counts: StdMutex<Vec<usize>>,
    }

    impl ScriptedGateway {
        fn new(streams: Vec<Vec<StreamChunk>>) -> Self {
            Self {
                streams: StdMutex::new(VecDeque::from(streams)),
                tool_counts: StdMutex::new(Vec::new()),
            }
        }

        fn tool_counts(&self) -> Vec<usize> {
            self.tool_counts
                .lock()
                .expect("scripted gateway tool-count mutex poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl LlmGateway for ScriptedGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            unimplemented!("streaming path only")
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            unimplemented!("streaming path only")
        }

        async fn get_available_models(&self) -> mojentic::Result<Vec<String>> {
            Ok(vec!["fake-model".to_string()])
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> mojentic::Result<Vec<f32>> {
            Ok(vec![])
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [LlmMessage],
            tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            self.tool_counts
                .lock()
                .expect("scripted gateway tool-count mutex poisoned")
                .push(tools.map(|tools| tools.len()).unwrap_or(0));
            let chunks = self
                .streams
                .lock()
                .expect("scripted gateway mutex poisoned")
                .pop_front()
                .unwrap_or_default();
            Box::pin(stream::iter(chunks.into_iter().map(Ok)))
        }
    }

    struct FakeToolGateway;

    #[async_trait]
    impl LlmGateway for FakeToolGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            unimplemented!("streaming path only")
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            unimplemented!("streaming path only")
        }

        async fn get_available_models(&self) -> mojentic::Result<Vec<String>> {
            Ok(vec!["fake-model".to_string()])
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> mojentic::Result<Vec<f32>> {
            Ok(vec![])
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            messages: &'a [LlmMessage],
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            if messages
                .iter()
                .any(|message| message.role == MessageRole::Tool)
            {
                Box::pin(stream::iter(vec![Ok(StreamChunk::Content(
                    "DONE".to_string(),
                ))]))
            } else {
                Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCalls(vec![
                    LlmToolCall {
                        id: Some("call-1".to_string()),
                        name: "echo".to_string(),
                        arguments: HashMap::from([("value".to_string(), json!("hello"))]),
                    },
                ]))]))
            }
        }
    }

    struct ContentThenToolGateway;

    #[async_trait]
    impl LlmGateway for ContentThenToolGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            unimplemented!("streaming path only")
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            unimplemented!("streaming path only")
        }

        async fn get_available_models(&self) -> mojentic::Result<Vec<String>> {
            Ok(vec!["fake-model".to_string()])
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> mojentic::Result<Vec<f32>> {
            Ok(vec![])
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            messages: &'a [LlmMessage],
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            if messages
                .iter()
                .any(|message| message.role == MessageRole::Tool)
            {
                Box::pin(stream::iter(vec![Ok(StreamChunk::Content(
                    "DONE".to_string(),
                ))]))
            } else {
                Box::pin(stream::iter(vec![
                    Ok(StreamChunk::Content("Validation passed: ok".to_string())),
                    Ok(StreamChunk::ToolCalls(vec![LlmToolCall {
                        id: Some("call-1".to_string()),
                        name: "echo".to_string(),
                        arguments: HashMap::from([("value".to_string(), json!("hello"))]),
                    }])),
                ]))
            }
        }
    }

    struct ToolOnlyGateway;

    #[async_trait]
    impl LlmGateway for ToolOnlyGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            unimplemented!("streaming path only")
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            unimplemented!("streaming path only")
        }

        async fn get_available_models(&self) -> mojentic::Result<Vec<String>> {
            Ok(vec!["fake-model".to_string()])
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> mojentic::Result<Vec<f32>> {
            Ok(vec![])
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            messages: &'a [LlmMessage],
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            if messages
                .iter()
                .any(|message| message.role == MessageRole::Tool)
            {
                Box::pin(stream::empty())
            } else {
                Box::pin(stream::iter(vec![Ok(StreamChunk::ToolCalls(vec![
                    LlmToolCall {
                        id: Some("call-1".to_string()),
                        name: "echo".to_string(),
                        arguments: HashMap::from([("value".to_string(), json!("hello"))]),
                    },
                ]))]))
            }
        }
    }

    struct NeverYieldGateway;

    #[async_trait]
    impl LlmGateway for NeverYieldGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            unimplemented!("streaming path only")
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            unimplemented!("streaming path only")
        }

        async fn get_available_models(&self) -> mojentic::Result<Vec<String>> {
            Ok(vec!["fake-stalled-model".to_string()])
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> mojentic::Result<Vec<f32>> {
            Ok(vec![])
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [LlmMessage],
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            Box::pin(stream::pending())
        }
    }

    #[derive(Clone)]
    struct EchoTool;

    #[async_trait]
    impl LlmTool for EchoTool {
        async fn run(
            &self,
            args: &HashMap<String, Value>,
            _ctx: &ToolRunCtx,
        ) -> mojentic::Result<Value> {
            Ok(json!({ "echo": args.get("value").cloned().unwrap_or(Value::Null) }))
        }

        fn descriptor(&self) -> ToolDescriptor {
            ToolDescriptor {
                r#type: "function".to_string(),
                function: FunctionDescriptor {
                    name: "echo".to_string(),
                    description: "Echo a value.".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "value": { "type": "string" }
                        }
                    }),
                },
            }
        }

        fn clone_box(&self) -> Box<dyn LlmTool> {
            Box::new(self.clone())
        }
    }

    /// A gateway that panics if any of its methods are invoked. Used to
    /// prove contract resolution happens before any LLM/tool effect
    /// (GENERALIZATION_PLAN.md Slice 2): `resolve_contract`'s signature has
    /// no gateway parameter at all, so an invalid contract cannot reach one
    /// — this test exercises that ordering concretely by threading a
    /// `PanicGateway` through the same async call site resolution runs in
    /// front of, and asserting the panics never fire.
    struct PanicGateway;

    #[async_trait]
    impl LlmGateway for PanicGateway {
        async fn complete(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            _config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            panic!("PanicGateway::complete must never be called before contract resolution");
        }

        async fn complete_json(
            &self,
            _model: &str,
            _messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            panic!("PanicGateway::complete_json must never be called before contract resolution");
        }

        async fn get_available_models(&self) -> mojentic::Result<Vec<String>> {
            panic!(
                "PanicGateway::get_available_models must never be called before contract resolution"
            );
        }

        async fn calculate_embeddings(
            &self,
            _text: &str,
            _model: Option<&str>,
        ) -> mojentic::Result<Vec<f32>> {
            panic!(
                "PanicGateway::calculate_embeddings must never be called before contract resolution"
            );
        }

        fn complete_stream<'a>(
            &'a self,
            _model: &'a str,
            _messages: &'a [LlmMessage],
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            panic!("PanicGateway::complete_stream must never be called before contract resolution");
        }
    }

    /// Mirrors the resolution-before-effect ordering in
    /// `run_coding_agent_with_gateway`: contract resolution is the first
    /// fallible step, performed before `gateway` is touched at all.
    async fn resolve_contract_before_gateway_probe<G: LlmGateway + ?Sized>(
        source: crate::contract::ContractSource,
        budgets: crate::contract::Budgets,
        _gateway: &G,
    ) -> Result<crate::contract::ResolvedRunContract> {
        crate::contract::resolve_contract(source, budgets)
    }

    #[tokio::test]
    async fn invalid_explicit_contract_errors_before_any_gateway_effect() {
        let invalid_json =
            std::fs::read_to_string("fixtures/contracts/invalid/duplicate_probe_id.json")
                .expect("reading invalid contract fixture");
        let source = crate::contract::ContractSource::Explicit {
            source_path: Some("fixtures/contracts/invalid/duplicate_probe_id.json".to_string()),
            json_text: invalid_json,
        };

        let result = resolve_contract_before_gateway_probe(
            source,
            crate::contract::Budgets::default(),
            &PanicGateway,
        )
        .await;

        assert!(
            result.is_err(),
            "invalid explicit contract must fail resolution"
        );
    }
}
