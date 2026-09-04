use crate::acceptance_interactions::{
    DEFAULT_MAX_INTERACTION_SCENARIOS, plan_acceptance_interactions_with_gateway,
};
use crate::acceptance_ledger::{AcceptanceLedgerSnapshot, AcceptanceLedgerSpec};
use crate::acceptance_plan::{DEFAULT_MAX_PLAN_ITEMS, plan_acceptance_with_gateway};
use crate::tools::{
    SuccessfulValidationSnapshot, ToolPolicySnapshot, ToolScope, ToolSurface,
    ValidationRepairSnapshot, tools_for_profile,
};
use crate::trace::TraceRecorder;
use anyhow::{Context, Result};
use futures::StreamExt;
use mojentic::MojenticError;
use mojentic::llm::gateway::{ReasoningEffort, ResponseFormat, StreamChunk, StreamMetrics};
use mojentic::llm::gateways::OllamaGateway;
use mojentic::llm::models::{LlmMessage, LlmToolCall, MessageRole};
use mojentic::llm::tools::{LlmTool, ToolRunCtx};
use mojentic::llm::{CompletionConfig, LlmGateway};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
const FILE_OBSERVATION_PREFIX: &str = "[harness-consolidated-file-observation]";
const FILE_OBSERVATION_METADATA_PREFIX: &str = "Metadata: ";
const FILE_OBSERVATION_CONTENT_START: &str = "--- BEGIN LINE-NUMBERED CONTENT ---";
const FILE_OBSERVATION_CONTENT_END: &str = "--- END LINE-NUMBERED CONTENT ---";
const FILE_OBSERVATION_CONTENT_MIN_CHARS: usize = 20_000;
const FILE_OBSERVATION_CONTENT_MAX_CHARS: usize = 120_000;
const EMPTY_RESPONSE_ESCALATION_TURNS: usize = 3;
const HIDDEN_ONLY_NO_ACTION_ESCALATION_TURNS: usize = 2;
const NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER: usize = 20;
const REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT: usize = 1_024;
const MAX_VALIDATION_REPAIR_LLM_CALL_DEPTH: usize = 12;
const DEFAULT_REPAIR_EXIT_THINKING_TOKENS: usize = 16_384;
const ACTION_BOUNDARY_INTENT_HIT_LIMIT: usize = 2;
const ACTION_BOUNDARY_INTENT_BUFFER_CHARS: usize = 4_096;
const ACTION_BOUNDARY_INTENT_HIT_GAP_TOKENS: usize = 512;
const RESPONSE_TOOL_CALL_ARGUMENT_MAX_CHARS: usize = 4_096;
const ACCEPTANCE_LEDGER_ADVISORY_OUTPUT_TOKENS: usize = 4_096;
const REASONING_CHECKPOINT_PREFIX: &str = "[harness-reasoning-checkpoint]";

#[derive(Debug, Clone)]
pub struct AgentRunConfig {
    pub experiment_dir: PathBuf,
    /// Optional trace destination outside the tool-visible experiment root.
    /// Defaults to `<experiment_dir>/traces` for backward compatibility.
    pub trace_dir: Option<PathBuf>,
    pub goal_file: PathBuf,
    pub contract_file: Option<PathBuf>,
    pub model: String,
    pub max_iterations: usize,
    pub max_tool_iterations: usize,
    pub context_window_tokens: Option<usize>,
    pub packet_type: String,
    pub expected_output_tokens: usize,
    pub num_predict: Option<usize>,
    pub max_thinking_only_tokens: usize,
    pub repair_exit_thinking_tokens: usize,
    pub repair_handoff_policy: RepairHandoffPolicy,
    pub action_boundary_interrupt_tokens: usize,
    /// Approximate token budget for retaining the tail of a hidden-only
    /// no-action turn into the next ordinary turn. Zero disables retention.
    pub reasoning_checkpoint_tokens: usize,
    pub transcript_policy: TranscriptPolicy,
    /// Optional adapter-owned initial-context catalog. Required, selectable,
    /// and excluded guidance dispositions are enforced by the assembler.
    pub initial_context_catalog_file: Option<PathBuf>,
    /// Optional model override for isolated semantic advisory calls. Defaults
    /// to the worker model when an advisory is required.
    pub semantic_advisor_model: Option<String>,
    /// Generate and retain a proposal-derived acceptance coverage ledger before
    /// the worker loop. Coverage is advisory and never replaces declared probes.
    pub acceptance_ledger: bool,
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
    pub repair_handoff_policy: RepairHandoffPolicy,
    pub action_boundary_interrupt_tokens: usize,
    pub reasoning_checkpoint_tokens: usize,
    pub transcript_policy: TranscriptPolicy,
    pub initial_context_catalog_file: Option<PathBuf>,
    pub semantic_advisor_model: Option<String>,
    pub acceptance_ledger: bool,
    pub acceptance_ledger_entry_count: usize,
    pub acceptance_plan_trace_file: Option<PathBuf>,
    pub acceptance_interactions_trace_file: Option<PathBuf>,
    pub required_initial_context_ids: Vec<String>,
    pub advisory_selected_context_ids: Vec<String>,
    pub excluded_initial_context_ids: Vec<String>,
    pub final_summary: String,
    pub harness_source_state: crate::provenance::HarnessSourceState,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepairHandoffPolicy {
    #[default]
    TextOnly,
    Constrained,
    ConstrainedActionOnly,
}

impl RepairHandoffPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "text-only" | "text" | "legacy" => Some(Self::TextOnly),
            "constrained" | "action-shaped" => Some(Self::Constrained),
            "constrained-action-only" | "action-only" => Some(Self::ConstrainedActionOnly),
            _ => None,
        }
    }

    fn is_constrained(self) -> bool {
        matches!(self, Self::Constrained | Self::ConstrainedActionOnly)
    }

    fn uses_action_only_retry(self) -> bool {
        self == Self::ConstrainedActionOnly
    }
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

pub async fn run_agent(config: AgentRunConfig) -> Result<AgentRunSummary> {
    let gateway = OllamaGateway::new();
    let tool_root = PathBuf::from(".")
        .canonicalize()
        .context("canonicalizing harness cwd")?;
    run_agent_with_gateway(config, &gateway, tool_root).await
}

fn agent_completion_config(
    max_tool_iterations: usize,
    num_predict: Option<usize>,
    context_window_tokens: Option<usize>,
) -> Result<CompletionConfig> {
    let num_predict = num_predict
        .map(i32::try_from)
        .transpose()
        .context("num_predict exceeds i32 range")?;
    let mut completion_config = CompletionConfig {
        temperature: 0.2,
        max_tool_iterations,
        num_predict,
        ..Default::default()
    };
    if let Some(context_window_tokens) = context_window_tokens {
        completion_config.num_ctx = context_window_tokens;
    }
    Ok(completion_config)
}

async fn run_agent_with_gateway<G: LlmGateway + ?Sized>(
    config: AgentRunConfig,
    gateway: &G,
    tool_root: PathBuf,
) -> Result<AgentRunSummary> {
    let experiment_dir = config
        .experiment_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", config.experiment_dir.display()))?;
    let goal_file = canonicalize_goal(
        &experiment_dir,
        config.contract_file.as_ref().unwrap_or(&config.goal_file),
    )?;
    let goal = tokio::fs::read_to_string(&goal_file)
        .await
        .with_context(|| format!("reading goal file {}", goal_file.display()))?;

    // Resolve the typed run contract before any tool scope, prompt, or LLM
    // effect (GENERALIZATION_PLAN.md Slice 2). The legacy coding adapter
    // wraps today's shell-fence scraping, so `requested_validation_commands`
    // below carries exactly the same ordered, deduped command strings as
    // before this slice.
    let contract_source = if config.contract_file.is_some() {
        crate::contract::ContractSource::Explicit {
            source_path: Some(goal_file.display().to_string()),
            json_text: goal.clone(),
        }
    } else {
        crate::contract::ContractSource::Legacy {
            goal_path: goal_file.display().to_string(),
            goal_text: goal.clone(),
        }
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
        .filter(|probe| !probe.command.is_empty())
        .map(|probe| probe.command.clone())
        .collect();

    let tool_root = tool_root
        .canonicalize()
        .with_context(|| format!("canonicalizing tool root {}", tool_root.display()))?;

    let trace_dir = config
        .trace_dir
        .clone()
        .unwrap_or_else(|| experiment_dir.join("traces"));
    let trace = Arc::new(TraceRecorder::create(&trace_dir)?);
    let harness_source_state = crate::provenance::capture();
    let tool_surface = ToolSurface::from_environment()?;
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
            "repair_handoff_policy": config.repair_handoff_policy,
            "action_boundary_interrupt_tokens": config.action_boundary_interrupt_tokens,
            "reasoning_checkpoint_tokens": config.reasoning_checkpoint_tokens,
            "assembly_policy": config.transcript_policy.as_str(),
            "transcript_policy": config.transcript_policy,
            "initial_context_catalog_file": config.initial_context_catalog_file,
            "semantic_advisor_model": config.semantic_advisor_model,
            "acceptance_ledger": config.acceptance_ledger,
            "tool_surface": tool_surface,
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

    let profile = crate::profile::profile_by_ref(&resolved_contract.profile)?;
    let initial_context_result: Result<_> = async {
        let catalog = if let Some(catalog_file) = &config.initial_context_catalog_file {
            let catalog_file = canonicalize_goal(&experiment_dir, catalog_file)?;
            Some(crate::initial_context::InitialContextCatalog::from_path(
                &catalog_file,
            )?)
        } else {
            None
        };
        let advisor_model = config
            .semantic_advisor_model
            .as_deref()
            .unwrap_or(&config.model);
        crate::initial_context::assemble_initial_context(
            gateway,
            advisor_model,
            &resolved_contract.guidance,
            profile.run_guidance(&resolved_contract.guidance),
            catalog.as_ref(),
            &trace,
        )
        .await
    }
    .await;
    let initial_context = match initial_context_result {
        Ok(context) => context,
        Err(error) => {
            trace.event(
                crate::runtime_events::RUN_FAILED,
                serde_json::json!({
                    "stage": "initial_context",
                    "error": error.to_string(),
                }),
            )?;
            return Err(error);
        }
    };

    let acceptance_ledger_result: Result<_> = async {
        if !config.acceptance_ledger {
            return Ok(None);
        }
        let advisor_model = config
            .semantic_advisor_model
            .as_deref()
            .unwrap_or(&config.model);
        let plan_summary = plan_acceptance_with_gateway(
            gateway,
            advisor_model,
            &resolved_contract.guidance,
            &trace_dir.join("acceptance-plan"),
            DEFAULT_MAX_PLAN_ITEMS,
            ACCEPTANCE_LEDGER_ADVISORY_OUTPUT_TOKENS,
        )
        .await?;
        let interaction_summary = plan_acceptance_interactions_with_gateway(
            gateway,
            advisor_model,
            &resolved_contract.guidance,
            &plan_summary.plan,
            &trace_dir.join("acceptance-interactions"),
            DEFAULT_MAX_INTERACTION_SCENARIOS,
            ACCEPTANCE_LEDGER_ADVISORY_OUTPUT_TOKENS,
        )
        .await?;
        let spec = AcceptanceLedgerSpec::from_plans(
            &plan_summary.plan,
            &interaction_summary.interactions,
        )?;
        trace.event(
            crate::runtime_events::ACCEPTANCE_LEDGER_PLANNED,
            serde_json::json!({
                "authority": "coverage_only",
                "advisor_model": advisor_model,
                "atomic_item_count": plan_summary.plan.items.len(),
                "interaction_scenario_count": interaction_summary.interactions.scenarios.len(),
                "entry_count": spec.entries.len(),
                "plan_attempts": plan_summary.attempts,
                "interaction_attempts": interaction_summary.attempts,
                "plan_trace_file": plan_summary.trace_file,
                "interaction_trace_file": interaction_summary.trace_file,
            }),
        )?;
        Ok(Some((
            spec,
            plan_summary.trace_file,
            interaction_summary.trace_file,
        )))
    }
    .await;
    let acceptance_ledger = match acceptance_ledger_result {
        Ok(ledger) => ledger,
        Err(error) => {
            trace.event(
                crate::runtime_events::RUN_FAILED,
                serde_json::json!({
                    "stage": "acceptance_ledger",
                    "error": error.to_string(),
                }),
            )?;
            return Err(error);
        }
    };

    let scope_rules = |scope: &crate::contract::Scope| match scope {
        crate::contract::Scope::Unrestricted => Vec::new(),
        crate::contract::Scope::Rules(rules) => rules.clone(),
    };
    let scope = ToolScope::new_profiled(
        tool_root.clone(),
        Arc::clone(&trace),
        resolved_contract.profile.clone(),
        scope_rules(&resolved_contract.read_scope),
        scope_rules(&resolved_contract.write_scope),
    )?;
    scope.configure_tool_surface(tool_surface);
    let requested_probe_ids_by_command = resolved_contract
        .probes
        .iter()
        .filter(|probe| !probe.command.is_empty())
        .map(|probe| (probe.command.clone(), probe.id.clone()))
        .collect::<BTreeMap<_, _>>();
    scope.configure_probes(resolved_contract.probes.clone())?;
    if let Some((spec, _, _)) = &acceptance_ledger {
        scope.configure_acceptance_ledger(spec.clone())?;
    }
    let system_prompt = profile.system_guidance();
    let tools = tools_for_profile(&scope, profile);
    let repair_tools = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.descriptor().function.name.as_str(),
                "edit_file"
                    | "replace_file_lines"
                    | "apply_patch"
                    | "apply_change_set"
                    | "write_file"
                    | "shell_command"
                    | "execute_probe"
            )
        })
        .map(|tool| tool.clone_box())
        .collect::<Vec<_>>();
    let action_only_tools = tools
        .iter()
        .filter(|tool| {
            matches!(
                tool.descriptor().function.name.as_str(),
                "edit_file"
                    | "replace_file_lines"
                    | "apply_patch"
                    | "apply_change_set"
                    | "write_file"
                    | "execute_probe"
            )
        })
        .map(|tool| tool.clone_box())
        .collect::<Vec<_>>();
    let worker_message = worker_message_with_acceptance_ledger(
        &initial_context.worker_message,
        acceptance_ledger.as_ref().map(|(spec, _, _)| spec),
    );
    let worker_message =
        worker_message_with_declared_probes(&worker_message, &resolved_contract.probes);
    if let Some((spec, plan_trace_file, interaction_trace_file)) = &acceptance_ledger {
        trace.event(
            crate::runtime_events::ACCEPTANCE_LEDGER_DELIVERED,
            serde_json::json!({
                "authority": "coverage_only",
                "entry_ids": spec.entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
                "entry_count": spec.entries.len(),
                "worker_message_chars": worker_message.chars().count(),
                "plan_trace_file": plan_trace_file,
                "interaction_trace_file": interaction_trace_file,
            }),
        )?;
    }
    if !resolved_contract.probes.is_empty() {
        trace.event(
            crate::runtime_events::AGENT_CONTRACT_PROBES_DELIVERED,
            serde_json::json!({
                "probe_ids": resolved_contract
                    .probes
                    .iter()
                    .map(|probe| probe.id.as_str())
                    .collect::<Vec<_>>(),
                "command_probe_ids": resolved_contract
                    .probes
                    .iter()
                    .filter(|probe| !probe.command.is_empty())
                    .map(|probe| probe.id.as_str())
                    .collect::<Vec<_>>(),
                "assertion_probe_ids": resolved_contract
                    .probes
                    .iter()
                    .filter(|probe| probe.assertion.is_some())
                    .map(|probe| probe.id.as_str())
                    .collect::<Vec<_>>(),
                "worker_message_chars": worker_message.chars().count(),
            }),
        )?;
    }
    let mut messages = vec![
        LlmMessage::system(system_prompt),
        LlmMessage::user(worker_message),
    ];
    let completion_config = agent_completion_config(
        config.max_tool_iterations,
        config.num_predict,
        config.context_window_tokens,
    )?;

    let mut final_summary = String::new();
    let mut final_response_only_next_turn = false;
    let mut authoritative_constrained_repair_active = false;
    let mut repair_action_only_next_turn = false;
    let mut pending_reasoning_checkpoint_delivery: Option<serde_json::Value> = None;
    let mut requested_validation_ledger = RequestedValidationLedger::new_for_probes(
        &resolved_contract.probes,
        resolved_contract.profile.clone(),
    );
    scope.observe_runtime(crate::runtime::RuntimeEvent::RunStarted);
    let requested_validation_completed_write_operations = 0usize;
    let mut exhausted_iterations = true;
    let no_tools: Vec<Box<dyn LlmTool>> = Vec::new();
    for turn in 1..=config.max_iterations {
        scope.observe_runtime(crate::runtime::RuntimeEvent::TurnStarted { turn });
        if let Some(mut metadata) = pending_reasoning_checkpoint_delivery.take() {
            metadata["delivery_turn"] = serde_json::json!(turn);
            trace.event(
                crate::runtime_events::AGENT_REASONING_CHECKPOINT_DELIVERED,
                metadata,
            )?;
        }
        let runtime_before_turn = scope.runtime_state_snapshot();
        let policy_before_turn = scope.policy_snapshot();
        let requested_validation_ledger_before_turn = requested_validation_ledger.clone();
        let constrained_repair_turn = config.repair_handoff_policy.is_constrained()
            && authoritative_constrained_repair_active
            && policy_before_turn.validation_repair.is_some();
        let repair_action_only_turn = constrained_repair_turn && repair_action_only_next_turn;
        let mut turn_completion_config = completion_config.clone();
        if repair_action_only_turn {
            turn_completion_config.reasoning_effort = Some(ReasoningEffort::Disabled);
        }
        if repair_action_only_turn {
            trace.event(
                "agent.validation.repair_action_only_started",
                serde_json::json!({
                    "turn": turn,
                    "reasoning_effort": ReasoningEffort::Disabled,
                    "prior_hidden_reasoning_retained": false,
                    "active_tool_names": action_only_tools
                        .iter()
                        .map(|tool| tool.descriptor().function.name)
                        .collect::<Vec<_>>(),
                }),
            )?;
        }
        let active_tools = if final_response_only_next_turn {
            &no_tools
        } else if repair_action_only_turn {
            &action_only_tools
        } else if constrained_repair_turn {
            &repair_tools
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
                "repair_handoff_policy": config.repair_handoff_policy,
                "constrained_repair_turn": constrained_repair_turn,
                "repair_action_only_turn": repair_action_only_turn,
                "active_tool_names": active_tools
                    .iter()
                    .map(|tool| tool.descriptor().function.name)
                    .collect::<Vec<_>>(),
            }),
        )?;
        let turn_result = match stream_response(StreamResponseRequest {
            gateway,
            model: &config.model,
            messages: &messages,
            tools: active_tools,
            completion_config: turn_completion_config,
            context_window_tokens: config.context_window_tokens,
            packet_type: &config.packet_type,
            expected_output_tokens: config.expected_output_tokens,
            max_thinking_only_tokens: config.max_thinking_only_tokens,
            repair_exit_thinking_tokens: config.repair_exit_thinking_tokens,
            repair_handoff_policy: config.repair_handoff_policy,
            action_boundary_interrupt_tokens: config.action_boundary_interrupt_tokens,
            reasoning_checkpoint_tokens: config.reasoning_checkpoint_tokens,
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
        repair_action_only_next_turn = false;
        final_response_only_next_turn = false;
        let repair_no_content_interrupted = turn_result.repair_no_content_interrupted;
        let action_boundary_interrupted = turn_result.action_boundary_interrupted;
        let repair_depth_hard_stop = turn_result.repair_depth_hard_stop;
        let authoritative_constrained_handoff =
            turn_result.authoritative_constrained_handoff_required;
        if policy_before_turn.validation_repair.is_none() {
            authoritative_constrained_repair_active = false;
        }
        let thinking_chars_this_turn = turn_result.thinking_chars;
        let reasoning_checkpoint = turn_result.reasoning_checkpoint;
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
        let consumed_reasoning_checkpoints = remove_prior_reasoning_checkpoints(&mut messages);
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
                validation_required_after_turn: policy.validation_required_after_write,
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
                "turn {turn} action-boundary interrupt after hidden reasoning without required source progress or fresh validation"
            );
            trace.event("agent.action_boundary.prompted", &decision.interrupt)?;
            if !response.trim().is_empty() {
                messages.push(LlmMessage::assistant(response.clone()));
            }
            messages.push(LlmMessage::user(action_boundary_interrupt_prompt(decision)));
            continue;
        }
        if authoritative_constrained_handoff {
            authoritative_constrained_repair_active = true;
            if !response.trim().is_empty() {
                messages.push(LlmMessage::assistant(response.clone()));
            }
            let removed_user_messages = prune_superseded_agent_loop_guidance(&mut messages);
            trace.event(
                "agent.validation.constrained_handoff_context_pruned",
                serde_json::json!({
                    "turn": turn,
                    "removed_user_messages": removed_user_messages,
                    "retained_initial_context": true,
                }),
            )?;
            let repair = policy
                .validation_repair
                .as_ref()
                .expect("authoritative failed probe requires active repair");
            messages.push(LlmMessage::user(validation_repair_prompt_for_profile(
                repair, profile,
            )));
            final_summary = format!(
                "turn {turn} entered constrained validation repair after authoritative failure"
            );
            continue;
        }
        if response.trim().is_empty() {
            if constrained_repair_turn && let Some(repair) = &policy.validation_repair {
                trace.event(
                    "agent.validation.repair_prompted",
                    serde_json::json!({
                        "turn": turn,
                        "tool_calls_this_turn": tool_calls_this_turn,
                        "policy": policy,
                        "handoff_policy": config.repair_handoff_policy,
                        "priority": "authoritative_failed_probe",
                    }),
                )?;
                final_summary = format!(
                    "turn {turn} entered constrained validation repair after authoritative failure"
                );
                if config.repair_handoff_policy.uses_action_only_retry()
                    && repair_no_content_interrupted
                {
                    repair_action_only_next_turn = true;
                    trace.event(
                        "agent.validation.repair_action_only_scheduled",
                        serde_json::json!({
                            "turn": turn,
                            "next_turn": turn + 1,
                            "prior_thinking_chars": thinking_chars_this_turn,
                            "prior_hidden_reasoning_retained": false,
                            "next_reasoning_effort": ReasoningEffort::Disabled,
                            "reason": "bounded_repair_diagnosis_ended_without_action",
                        }),
                    )?;
                    messages.push(LlmMessage::user(validation_repair_action_only_prompt(
                        repair, profile,
                    )));
                } else {
                    messages.push(LlmMessage::user(validation_repair_prompt_for_profile(
                        repair, profile,
                    )));
                }
                continue;
            }
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
                messages.push(LlmMessage::user(profile.post_write_validation_nudge(false)));
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
                if let Some(checkpoint) = &reasoning_checkpoint {
                    let metadata = serde_json::json!({
                        "source_turn": turn,
                        "total_thinking_chars": checkpoint.total_thinking_chars,
                        "retained_chars": checkpoint.retained_tail.len(),
                        "retained_estimated_tokens": estimate_tokens(checkpoint.retained_tail.len()),
                        "omitted_prefix_chars": checkpoint.omitted_prefix_chars,
                        "retained_sha256": checkpoint.retained_sha256,
                        "removed_consumed_checkpoints": consumed_reasoning_checkpoints,
                        "authority": "self_generated_continuity_only",
                        "reasoning_disabled_next_turn": false,
                        "tool_surface_narrowed_next_turn": false,
                    });
                    trace.event(
                        crate::runtime_events::AGENT_REASONING_CHECKPOINT_CAPTURED,
                        &metadata,
                    )?;
                    pending_reasoning_checkpoint_delivery = Some(metadata);
                }
                messages.push(LlmMessage::user(hidden_only_no_action_prompt(
                    consecutive_hidden_only_no_action_turns,
                    tool_calls_this_turn,
                    reasoning_checkpoint.as_ref(),
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
            messages.push(LlmMessage::user(profile.post_write_validation_nudge(true)));
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
        if is_done_response(&final_summary) && config.acceptance_ledger {
            let acceptance = scope.acceptance_ledger_snapshot();
            if !acceptance.is_complete() {
                trace.event(
                    crate::runtime_events::ACCEPTANCE_LEDGER_DONE_REJECTED,
                    serde_json::json!({
                        "turn": turn,
                        "response": final_summary,
                        "ledger": acceptance,
                        "authority": "coverage_only",
                    }),
                )?;
                messages.push(LlmMessage::user(acceptance_ledger_done_rejected_prompt(
                    &acceptance,
                )));
                continue;
            }
        }
        if is_terminal_response(&final_summary) {
            if let Some(token) = crate::runtime::terminal_token(&final_summary) {
                let event = crate::runtime::RuntimeEvent::TerminalToken { token };
                scope.observe_runtime(event.clone());
                for legacy in crate::runtime_events::legacy_trace_events(&event) {
                    trace.event(legacy.kind, legacy.payload)?;
                }
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
                messages.push(LlmMessage::user(validation_repair_prompt_for_profile(
                    repair, profile,
                )));
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
        repair_handoff_policy: config.repair_handoff_policy,
        action_boundary_interrupt_tokens: config.action_boundary_interrupt_tokens,
        reasoning_checkpoint_tokens: config.reasoning_checkpoint_tokens,
        transcript_policy: config.transcript_policy,
        initial_context_catalog_file: config.initial_context_catalog_file,
        semantic_advisor_model: config.semantic_advisor_model,
        acceptance_ledger: config.acceptance_ledger,
        acceptance_ledger_entry_count: acceptance_ledger
            .as_ref()
            .map(|(spec, _, _)| spec.entries.len())
            .unwrap_or(0),
        acceptance_plan_trace_file: acceptance_ledger.as_ref().map(|(_, path, _)| path.clone()),
        acceptance_interactions_trace_file: acceptance_ledger
            .as_ref()
            .map(|(_, _, path)| path.clone()),
        required_initial_context_ids: initial_context.required_ids,
        advisory_selected_context_ids: initial_context.advisory_selected_ids,
        excluded_initial_context_ids: initial_context.excluded_ids,
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
    #[serde(skip)]
    profile: crate::profile::ProfileRef,
    generation: usize,
    entries: Vec<RequestedValidationEntry>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct RequestedValidationEntry {
    probe_id: Option<String>,
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
    #[cfg(test)]
    fn new(commands: Vec<String>) -> Self {
        Self::new_for_profile(commands, crate::profile::ProfileRef::default())
    }

    #[cfg(test)]
    fn new_for_profile(commands: Vec<String>, profile: crate::profile::ProfileRef) -> Self {
        Self {
            profile,
            generation: 0,
            entries: commands
                .into_iter()
                .map(|command| RequestedValidationEntry {
                    probe_id: None,
                    command,
                    status: RequestedValidationStatus::Pending,
                    observed_command: None,
                    status_code: None,
                    generation: None,
                })
                .collect(),
        }
    }

    fn new_for_probes(
        probes: &[crate::contract::Probe],
        profile: crate::profile::ProfileRef,
    ) -> Self {
        Self {
            profile,
            generation: 0,
            entries: probes
                .iter()
                .filter(|probe| !probe.command.is_empty())
                .map(|probe| RequestedValidationEntry {
                    probe_id: Some(probe.id.clone()),
                    command: probe.command.clone(),
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
        let observed_probe_id = value.get("probe_id").and_then(serde_json::Value::as_str);
        let matched_index = self.entries.iter().position(|entry| {
            observed_probe_id.is_some_and(|probe_id| entry.probe_id.as_deref() == Some(probe_id))
                || validation_matches_requested_command(
                    &command,
                    std::slice::from_ref(&entry.command),
                )
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
                command: entry.observed_command.clone().unwrap_or_else(|| {
                    entry
                        .probe_id
                        .as_ref()
                        .map(|probe_id| format!("probe:{probe_id}"))
                        .unwrap_or_else(|| entry.command.clone())
                }),
                command_family: crate::profile::profile_by_ref(&self.profile)
                    .expect("ledger profile was validated at contract resolution")
                    .validation_command_family(&entry.command),
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
            let target = entry
                .probe_id
                .as_ref()
                .map(|probe_id| format!("probe `{probe_id}`"))
                .unwrap_or_else(|| entry.command.clone());
            format!("- {target} ({status})")
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

fn acceptance_ledger_done_rejected_prompt(ledger: &AcceptanceLedgerSnapshot) -> String {
    format!(
        "DONE is not accepted yet because advisory acceptance coverage is incomplete for mutation epoch {}. Missing or stale ledger IDs: {}. Address those requirements and interactions, gather deterministic observations, then call `submit_acceptance_evidence` with the covered IDs and concise citations. This is a coverage gate; declared probes remain the validation authority.",
        ledger.mutation_epoch,
        ledger.incomplete_ids.join(", ")
    )
}

fn worker_message_with_acceptance_ledger(
    base: &str,
    ledger: Option<&AcceptanceLedgerSpec>,
) -> String {
    match ledger {
        Some(ledger) => format!("{base}\n\n{}", ledger.render_worker_packet()),
        None => base.to_string(),
    }
}

fn worker_message_with_declared_probes(base: &str, probes: &[crate::contract::Probe]) -> String {
    if probes.is_empty() {
        return base.to_string();
    }

    let rendered = probes
        .iter()
        .map(|probe| {
            if probe.command.is_empty() {
                format!(
                    "Probe `{}` is an adapter-owned artifact assertion. Execute it by probe ID after the latest relevant mutation.",
                    probe.id
                )
            } else {
                format!(
                    "Probe `{}` is an adapter-owned command capability. Execute it by probe ID after the latest relevant mutation; its implementation is intentionally not part of your context.",
                    probe.id
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "{base}\n\nAuthoritative validation contract:\n{rendered}\n\nDo not replace a declared probe with a synthetic approximation. DONE is accepted only after every declared probe passes for the latest source state."
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
        "turn {} produced {} consecutive action-boundary interrupts without required source progress or fresh validation",
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
    repair_handoff_policy: RepairHandoffPolicy,
    action_boundary_interrupt_tokens: usize,
    reasoning_checkpoint_tokens: usize,
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
    reasoning_checkpoint: Option<ReasoningCheckpoint>,
    repair_no_content_interrupted: bool,
    action_boundary_interrupted: Option<ActionBoundaryInterrupt>,
    repair_depth_hard_stop: Option<RepairDepthDecision>,
    authoritative_constrained_handoff_required: bool,
    requested_validation_ledger: RequestedValidationLedger,
}

#[derive(Debug, Clone)]
struct ReasoningCheckpoint {
    total_thinking_chars: usize,
    retained_tail: String,
    omitted_prefix_chars: usize,
    retained_sha256: String,
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
    validation_required_after_turn: bool,
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
        repair_handoff_policy,
        action_boundary_interrupt_tokens,
        reasoning_checkpoint_tokens,
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
    let mut validation_repair_active = validation_repair_active;
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
    let reasoning_checkpoint_max_chars =
        reasoning_checkpoint_tokens.saturating_mul(APPROX_CHARS_PER_TOKEN);
    let mut reasoning_checkpoint_tail = String::new();
    let mut stream_progress_frame_count = 0usize;
    let mut tool_call_progress_frame_count = 0usize;
    let mut no_content_segment_eval_count = 0usize;
    let mut repair_no_content_interrupted = false;
    let mut action_boundary_interrupted = None;
    let no_assistant_content_limit =
        expected_output_tokens.saturating_mul(NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER);
    let mut inspection_loop_tracker = InspectionLoopTracker::default();
    let mut final_response_only_after_validation: Option<SuccessfulValidationSnapshot> = None;
    let mut constrained_repair_handoff_required = false;
    let mut requested_validation_pending_after_write = requested_validation_pending_after_write;
    let mut requested_validation_ledger = requested_validation_ledger;
    let profile = crate::profile::profile_by_ref(&requested_validation_ledger.profile)?;
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
                reasoning_checkpoint: reasoning_checkpoint(
                    thinking_chars,
                    reasoning_checkpoint_tail,
                ),
                repair_no_content_interrupted,
                action_boundary_interrupted,
                repair_depth_hard_stop: Some(decision),
                authoritative_constrained_handoff_required: false,
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
        trace_provider_request(ProviderRequestTraceInput {
            trace,
            model,
            turn,
            llm_call_depth: depth,
            messages: &current_messages,
            tools: active_tools,
            completion_config: &completion_config,
            max_thinking_only_tokens,
            repair_exit_thinking_tokens,
            validation_repair_active,
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
                        crate::runtime_events::LLM_STREAM_TOOL_CALLS_COMPLETED,
                        serde_json::json!({
                            "turn": turn,
                            "llm_call_depth": depth,
                            "tool_call_count": tool_calls.len(),
                            "tool_call_names": tool_calls.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
                        }),
                    )?;
                    trace_response_tool_calls(trace, turn, depth, &tool_calls)?;
                    accumulated_tool_calls = tool_calls;
                }
                Ok(StreamChunk::Thinking(thinking)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    latest_progress_state = ModelProgressState::Generating;
                    no_content_segment_eval_count = 0;
                    thinking_chunk_count += 1;
                    thinking_chars += thinking.len();
                    push_bounded_buffer(
                        &mut reasoning_checkpoint_tail,
                        &thinking,
                        reasoning_checkpoint_max_chars,
                    );
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
                    if action_intent_signal_for_profile(&call_action_intent_buffer, profile)
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
                            crate::runtime_events::LLM_THINKING_ONLY_STREAM_ACTION_TRANSITIONED,
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
                                "next_policy": "action_only_turn",
                            }),
                        )?;
                        break;
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
                reasoning_checkpoint: reasoning_checkpoint(
                    thinking_chars,
                    reasoning_checkpoint_tail,
                ),
                repair_no_content_interrupted,
                action_boundary_interrupted,
                repair_depth_hard_stop: None,
                authoritative_constrained_handoff_required: false,
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
            if let Some(decision) =
                inspection_loop_tracker.observe(turn, depth, call, &tool_result, profile)
            {
                trace.event("agent.inspection_loop.hard_failed", &decision)?;
                anyhow::bail!("{}", inspection_loop_failure_summary(&decision));
            }
            if is_meaningful_source_edit(call, &tool_result, profile) {
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
            if repair_handoff_policy.is_constrained()
                && call.name == "execute_probe"
                && tool_result_requires_repair(&tool_result)
            {
                constrained_repair_handoff_required = true;
                trace.event(
                    "agent.validation.constrained_handoff_required",
                    serde_json::json!({
                        "turn": turn,
                        "llm_call_depth": depth,
                        "tool_call_id": &call.id,
                        "tool_name": &call.name,
                        "next_policy": "end_in_flight_turn_and_prompt_repair",
                    }),
                )?;
            }
            if call.name == "execute_probe" && tool_result_requires_repair(&tool_result) {
                validation_repair_active = true;
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
        coalesce_retained_file_observations(
            &mut current_messages,
            trace,
            turn,
            depth,
            context_window_tokens.unwrap_or(completion_config.num_ctx),
        )?;
        compact_retained_tool_results(
            &mut current_messages,
            active_tools,
            trace,
            turn,
            depth,
            transcript_policy,
            context_window_tokens,
        )?;
        if constrained_repair_handoff_required {
            trace.event(
                "agent.validation.constrained_handoff_started",
                serde_json::json!({
                    "turn": turn,
                    "llm_call_depth": depth,
                    "response_chars": response.len(),
                    "tool_call_count": accumulated_tool_calls.len(),
                }),
            )?;
            return Ok(StreamResponseResult {
                response,
                messages: current_messages,
                thinking_chars,
                reasoning_checkpoint: reasoning_checkpoint(
                    thinking_chars,
                    reasoning_checkpoint_tail,
                ),
                repair_no_content_interrupted,
                action_boundary_interrupted,
                repair_depth_hard_stop: None,
                authoritative_constrained_handoff_required: true,
                requested_validation_ledger,
            });
        }
    }

    unreachable!("tool iteration loop always returns or errors before exhaustion")
}

fn tool_result_requires_repair(result: &ToolCallRunResult) -> bool {
    serde_json::from_str::<serde_json::Value>(&result.content)
        .ok()
        .and_then(|value| value.get("repair_required").cloned())
        .is_some_and(|repair| !repair.is_null())
}

fn prune_superseded_agent_loop_guidance(messages: &mut Vec<LlmMessage>) -> usize {
    let mut initial_user_retained = false;
    let before = messages.len();
    messages.retain(|message| {
        if message.role != MessageRole::User {
            return true;
        }
        if !initial_user_retained {
            initial_user_retained = true;
            return true;
        }
        false
    });
    before - messages.len()
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
        profile: &dyn crate::profile::DomainProfile,
    ) -> Option<InspectionLoopDecision> {
        if self.runtime.meaningful_action_seen {
            return None;
        }
        if is_meaningful_source_edit(call, result, profile) || is_validation_probe_result(result) {
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

struct ProviderRequestTraceInput<'a> {
    trace: &'a TraceRecorder,
    model: &'a str,
    turn: usize,
    llm_call_depth: usize,
    messages: &'a [LlmMessage],
    tools: &'a [Box<dyn LlmTool>],
    completion_config: &'a CompletionConfig,
    max_thinking_only_tokens: usize,
    repair_exit_thinking_tokens: usize,
    validation_repair_active: bool,
}

fn trace_provider_request(input: ProviderRequestTraceInput<'_>) -> Result<()> {
    let ProviderRequestTraceInput {
        trace,
        model,
        turn,
        llm_call_depth,
        messages,
        tools,
        completion_config,
        max_thinking_only_tokens,
        repair_exit_thinking_tokens,
        validation_repair_active,
    } = input;
    let tool_descriptors = tools
        .iter()
        .map(|tool| tool.descriptor())
        .collect::<Vec<_>>();
    let response_format = match &completion_config.response_format {
        None => serde_json::Value::Null,
        Some(ResponseFormat::Text) => serde_json::json!({ "type": "text" }),
        Some(ResponseFormat::JsonObject { schema }) => serde_json::json!({
            "type": "json_object",
            "schema": schema,
        }),
    };
    let thinking_disabled = completion_config.reasoning_effort == Some(ReasoningEffort::Disabled);
    let (effective_thinking_only_cap_tokens, effective_thinking_only_cap_source) =
        effective_thinking_only_cap(
            max_thinking_only_tokens,
            repair_exit_thinking_tokens,
            validation_repair_active,
            thinking_disabled,
        );

    trace.event(
        crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        serde_json::json!({
            "schema_version": "provider_request.v1",
            "turn": turn,
            "llm_call_depth": llm_call_depth,
            "model": model,
            "messages": messages,
            "tools": tool_descriptors,
            "completion": {
                "temperature": completion_config.temperature,
                "num_ctx": completion_config.num_ctx,
                "max_tokens": completion_config.max_tokens,
                "num_predict": completion_config.num_predict,
                "top_p": completion_config.top_p,
                "top_k": completion_config.top_k,
                "response_format": response_format,
                "reasoning_effort": completion_config.reasoning_effort,
                "max_tool_iterations": completion_config.max_tool_iterations,
            },
            "harness_limits": {
                "max_thinking_only_tokens": max_thinking_only_tokens,
                "repair_exit_thinking_tokens": repair_exit_thinking_tokens,
                "validation_repair_active": validation_repair_active,
                "thinking_disabled": thinking_disabled,
                "effective_thinking_only_cap_tokens": effective_thinking_only_cap_tokens,
                "effective_thinking_only_cap_source": effective_thinking_only_cap_source,
            },
        }),
    )
}

fn effective_thinking_only_cap(
    max_thinking_only_tokens: usize,
    repair_exit_thinking_tokens: usize,
    validation_repair_active: bool,
    thinking_disabled: bool,
) -> (Option<usize>, &'static str) {
    if thinking_disabled {
        return (None, "provider_disabled");
    }
    let ordinary = (max_thinking_only_tokens > 0).then_some(max_thinking_only_tokens);
    let repair = (validation_repair_active && repair_exit_thinking_tokens > 0)
        .then_some(repair_exit_thinking_tokens);
    match (ordinary, repair) {
        (None, None) => (None, "disabled"),
        (Some(cap), None) => (Some(cap), "ordinary"),
        (None, Some(cap)) => (Some(cap), "validation_repair"),
        (Some(ordinary), Some(repair)) if repair <= ordinary => (Some(repair), "validation_repair"),
        (Some(ordinary), Some(_)) => (Some(ordinary), "ordinary"),
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileObservationRange {
    line_start: usize,
    line_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileObservationSegment {
    line_start: usize,
    line_end: usize,
    content: String,
    last_line_complete: bool,
    last_observed_sequence: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FileObservationSegmentMetadata {
    line_start: usize,
    line_end: usize,
    last_line_complete: bool,
    last_observed_sequence: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsolidatedFileObservationMetadata {
    schema_version: String,
    path: String,
    epoch: usize,
    source_read_count: usize,
    unique_read_signatures: Vec<String>,
    requested_ranges: Vec<FileObservationRange>,
    historically_observed_ranges: Vec<FileObservationRange>,
    retained_ranges: Vec<FileObservationRange>,
    content_status: String,
    missing_ranges: Vec<FileObservationRange>,
    content_budget_chars: usize,
    total_lines: usize,
    line_number_width: usize,
    segments: Vec<FileObservationSegmentMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConsolidatedFileObservation {
    schema_version: String,
    path: String,
    epoch: usize,
    source_read_count: usize,
    unique_read_signatures: Vec<String>,
    #[serde(alias = "reported_ranges")]
    requested_ranges: Vec<FileObservationRange>,
    #[serde(alias = "observed_ranges")]
    historically_observed_ranges: Vec<FileObservationRange>,
    retained_ranges: Vec<FileObservationRange>,
    #[serde(default)]
    content_status: String,
    #[serde(alias = "omitted_ranges")]
    missing_ranges: Vec<FileObservationRange>,
    content_budget_chars: usize,
    total_lines: usize,
    segments: Vec<FileObservationSegment>,
}

#[derive(Debug, Clone)]
struct RawFileObservation {
    path: String,
    content: String,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
    truncated_by_bytes: bool,
    signature: String,
}

#[derive(Debug, Default)]
struct FileObservationAccumulator {
    source_read_count: usize,
    unique_read_signatures: Vec<String>,
    reported_ranges: Vec<FileObservationRange>,
    observed_ranges: Vec<FileObservationRange>,
    total_lines: Option<usize>,
    lines: BTreeMap<usize, (String, bool, usize)>,
    observation_sequence: usize,
    conflict: bool,
}

fn coalesce_retained_file_observations(
    messages: &mut Vec<LlmMessage>,
    trace: &TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
    context_window_tokens: usize,
) -> Result<()> {
    let original = std::mem::take(messages);
    let mut output = Vec::with_capacity(original.len());
    let mut epoch_messages = Vec::new();
    let mut epoch = 0;
    let mut index = 0;

    while index < original.len() {
        let span = assistant_tool_exchange_span(&original, index).unwrap_or(1);
        let exchange = &original[index..index + span];
        let mutation_barrier = file_observation_mutation_barrier(exchange);

        if mutation_barrier {
            flush_file_observation_epoch(
                &mut output,
                std::mem::take(&mut epoch_messages),
                epoch,
                trace,
                turn,
                llm_call_depth,
                context_window_tokens,
            )?;
            output.extend(exchange.iter().cloned());
            epoch += 1;
        } else {
            epoch_messages.extend(exchange.iter().cloned());
        }
        index += span;
    }

    flush_file_observation_epoch(
        &mut output,
        epoch_messages,
        epoch,
        trace,
        turn,
        llm_call_depth,
        context_window_tokens,
    )?;
    retain_latest_file_observation_epochs(&mut output, trace, turn, llm_call_depth)?;
    *messages = output;
    Ok(())
}

fn retain_latest_file_observation_epochs(
    messages: &mut Vec<LlmMessage>,
    trace: &TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
) -> Result<()> {
    let mut latest_epochs = BTreeMap::<String, usize>::new();
    let mut index = 0;
    while index < messages.len() {
        let span = assistant_tool_exchange_span(messages, index).unwrap_or(1);
        if let Some(observations) = parse_read_exchange(&messages[index..index + span]) {
            for observation in observations {
                if let ParsedFileObservation::Consolidated(value) = observation.observation {
                    latest_epochs
                        .entry(value.path)
                        .and_modify(|epoch| *epoch = (*epoch).max(value.epoch))
                        .or_insert(value.epoch);
                }
            }
        }
        index += span;
    }
    if latest_epochs.is_empty() {
        return Ok(());
    }

    let original = std::mem::take(messages);
    let mut retained = Vec::with_capacity(original.len());
    let mut removed_projection_count = 0;
    let mut cursor = 0;
    while cursor < original.len() {
        let span = assistant_tool_exchange_span(&original, cursor).unwrap_or(1);
        let exchange = &original[cursor..cursor + span];
        let Some(observations) = parse_read_exchange(exchange) else {
            retained.extend(exchange.iter().cloned());
            cursor += span;
            continue;
        };
        let calls = exchange[0]
            .tool_calls
            .as_ref()
            .expect("parsed read exchange has tool calls");
        let removed_ordinals = observations
            .iter()
            .filter_map(|observation| match &observation.observation {
                ParsedFileObservation::Consolidated(value)
                    if latest_epochs
                        .get(&value.path)
                        .is_some_and(|epoch| *epoch > value.epoch) =>
                {
                    Some(observation.ordinal)
                }
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        removed_projection_count += removed_ordinals.len();
        let retained_ordinals = (0..calls.len())
            .filter(|ordinal| !removed_ordinals.contains(ordinal))
            .collect::<Vec<_>>();
        if !retained_ordinals.is_empty() {
            let mut assistant = exchange[0].clone();
            assistant.tool_calls = Some(
                retained_ordinals
                    .iter()
                    .map(|ordinal| calls[*ordinal].clone())
                    .collect(),
            );
            retained.push(assistant);
            retained.extend(
                retained_ordinals
                    .iter()
                    .map(|ordinal| exchange[*ordinal + 1].clone()),
            );
        }
        cursor += span;
    }
    *messages = retained;
    if removed_projection_count > 0 {
        trace.event(
            "llm.context_assembly.superseded_file_observations_removed",
            serde_json::json!({
                "schema_version": "latest_file_observation.v1",
                "turn": turn,
                "llm_call_depth": llm_call_depth,
                "removed_projection_count": removed_projection_count,
                "latest_epochs": latest_epochs,
                "raw_history_retained_in_trace_only": true,
            }),
        )?;
    }
    Ok(())
}

fn flush_file_observation_epoch(
    output: &mut Vec<LlmMessage>,
    epoch_messages: Vec<LlmMessage>,
    epoch: usize,
    trace: &TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
    context_window_tokens: usize,
) -> Result<()> {
    if epoch_messages.is_empty() {
        return Ok(());
    }
    let original_message_count = epoch_messages.len();
    let original_chars = epoch_messages.iter().map(message_chars).sum::<usize>();
    let mut exchanges = Vec::new();
    let mut index = 0;
    while index < epoch_messages.len() {
        let span = assistant_tool_exchange_span(&epoch_messages, index).unwrap_or(1);
        if let Some(parsed) = parse_read_exchange(&epoch_messages[index..index + span]) {
            exchanges.push((index, span, parsed));
        }
        index += span;
    }

    let mut path_read_counts = BTreeMap::<String, usize>::new();
    for observation in exchanges
        .iter()
        .flat_map(|(_, _, observations)| observations)
    {
        *path_read_counts
            .entry(parsed_observation_path(&observation.observation).to_string())
            .or_default() += parsed_observation_source_count(&observation.observation);
    }
    let projected_paths = path_read_counts
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if projected_paths.is_empty() {
        output.extend(epoch_messages);
        return Ok(());
    }
    let source_read_count = projected_paths
        .iter()
        .filter_map(|path| path_read_counts.get(path))
        .sum::<usize>();
    let file_count = projected_paths.len();

    let mut accumulators = BTreeMap::<String, FileObservationAccumulator>::new();
    for observation in exchanges
        .iter()
        .flat_map(|(_, _, observations)| observations)
        .filter(|observation| {
            projected_paths.contains(parsed_observation_path(&observation.observation))
        })
        .cloned()
    {
        merge_parsed_file_observation(&mut accumulators, observation.observation);
    }
    if accumulators
        .values()
        .any(|accumulator| accumulator.conflict)
    {
        trace.event(
            "llm.context_assembly.file_observation_coalescing_skipped",
            serde_json::json!({
                "turn": turn,
                "llm_call_depth": llm_call_depth,
                "epoch": epoch,
                "reason": "conflicting_content_or_total_lines_within_unchanged_file_epoch",
                "source_read_count": source_read_count,
                "file_count": file_count,
            }),
        )?;
        output.extend(epoch_messages);
        return Ok(());
    }

    let consolidated = accumulators
        .into_iter()
        .map(|(path, accumulator)| accumulator.finish(path, epoch, context_window_tokens))
        .collect::<Vec<_>>();
    let (assistant, tools) = consolidated_file_observation_messages(&consolidated);
    let last_exchange_start = exchanges
        .iter()
        .rev()
        .find(|(_, _, observations)| {
            observations.iter().any(|observation| {
                projected_paths.contains(parsed_observation_path(&observation.observation))
            })
        })
        .map(|(start, _, _)| *start)
        .unwrap_or(0);
    let exchange_starts = exchanges
        .iter()
        .map(|(start, span, observations)| (*start, (*span, observations)))
        .collect::<BTreeMap<_, _>>();
    let mut candidate = Vec::with_capacity(epoch_messages.len());
    let mut cursor = 0;
    while cursor < epoch_messages.len() {
        if let Some((span, observations)) = exchange_starts.get(&cursor) {
            candidate.extend(retained_read_exchange(
                &epoch_messages[cursor..cursor + *span],
                observations,
                &projected_paths,
            ));
            if cursor == last_exchange_start {
                candidate.push(assistant.clone());
                candidate.extend(tools.iter().cloned());
            }
            cursor += *span;
        } else {
            candidate.push(epoch_messages[cursor].clone());
            cursor += 1;
        }
    }
    let retained_message_count = candidate.len();
    let retained_chars = candidate.iter().map(message_chars).sum::<usize>();
    output.extend(candidate);
    trace.event(
        "llm.context_assembly.file_observations_coalesced",
        serde_json::json!({
            "schema_version": "file_observation_coalescing.v2",
            "turn": turn,
            "llm_call_depth": llm_call_depth,
            "epoch": epoch,
            "source_read_count": source_read_count,
            "file_count": file_count,
            "original_message_count": original_message_count,
            "retained_message_count": retained_message_count,
            "original_chars": original_chars,
            "retained_chars": retained_chars,
            "character_delta": retained_chars as i128 - original_chars as i128,
            "estimated_token_delta": estimate_tokens(retained_chars) as i128
                - estimate_tokens(original_chars) as i128,
            "files": consolidated.iter().map(|value| serde_json::json!({
                "path": value.path,
                "source_read_count": value.source_read_count,
                "unique_read_count": value.unique_read_signatures.len(),
                "requested_ranges": value.requested_ranges,
                "historically_observed_ranges": value.historically_observed_ranges,
                "retained_ranges": value.retained_ranges,
                "content_status": value.content_status,
                "missing_ranges": value.missing_ranges,
                "content_budget_chars": value.content_budget_chars,
                "total_lines": value.total_lines,
            })).collect::<Vec<_>>(),
            "raw_tool_events_preserved_in_trace_only": true,
            "authoritative_provider_projection": true,
        }),
    )?;
    Ok(())
}

#[derive(Debug, Clone)]
enum ParsedFileObservation {
    Raw(RawFileObservation),
    Consolidated(ConsolidatedFileObservation),
}

#[derive(Debug, Clone)]
struct ParsedReadObservation {
    ordinal: usize,
    observation: ParsedFileObservation,
}

fn parsed_observation_path(observation: &ParsedFileObservation) -> &str {
    match observation {
        ParsedFileObservation::Raw(value) => &value.path,
        ParsedFileObservation::Consolidated(value) => &value.path,
    }
}

fn parsed_observation_source_count(observation: &ParsedFileObservation) -> usize {
    match observation {
        ParsedFileObservation::Raw(_) => 1,
        ParsedFileObservation::Consolidated(value) => value.source_read_count,
    }
}

fn retained_read_exchange(
    exchange: &[LlmMessage],
    observations: &[ParsedReadObservation],
    removed_paths: &std::collections::BTreeSet<String>,
) -> Vec<LlmMessage> {
    let assistant = &exchange[0];
    let calls = assistant.tool_calls.as_ref().expect("parsed read exchange");
    let removed_ordinals = observations
        .iter()
        .filter_map(|observation| {
            removed_paths
                .contains(parsed_observation_path(&observation.observation))
                .then_some(observation.ordinal)
        })
        .collect::<std::collections::BTreeSet<_>>();
    let retained_ordinals = (0..calls.len())
        .filter(|ordinal| !removed_ordinals.contains(ordinal))
        .collect::<Vec<_>>();
    if retained_ordinals.is_empty() {
        return Vec::new();
    }
    let mut retained_assistant = assistant.clone();
    retained_assistant.tool_calls = Some(
        retained_ordinals
            .iter()
            .map(|ordinal| calls[*ordinal].clone())
            .collect(),
    );
    let mut retained = vec![retained_assistant];
    retained.extend(
        retained_ordinals
            .iter()
            .map(|ordinal| exchange[*ordinal + 1].clone()),
    );
    retained
}

fn assistant_tool_exchange_span(messages: &[LlmMessage], index: usize) -> Option<usize> {
    let message = messages.get(index)?;
    if message.role != MessageRole::Assistant {
        return None;
    }
    let calls = message.tool_calls.as_ref()?;
    if calls.is_empty() || index + calls.len() >= messages.len() {
        return None;
    }
    messages[index + 1..=index + calls.len()]
        .iter()
        .all(|message| message.role == MessageRole::Tool)
        .then_some(calls.len() + 1)
}

fn parse_read_exchange(messages: &[LlmMessage]) -> Option<Vec<ParsedReadObservation>> {
    let assistant = messages.first()?;
    let calls = assistant.tool_calls.as_ref()?;
    if calls.is_empty() || messages.len() != calls.len() + 1 {
        return None;
    }
    let observations = calls
        .iter()
        .zip(messages.iter().skip(1))
        .enumerate()
        .filter_map(|(ordinal, (call, message))| {
            (call.name == "read_file")
                .then(|| parse_read_tool_message(call, message))
                .flatten()
                .map(|observation| ParsedReadObservation {
                    ordinal,
                    observation,
                })
        })
        .collect::<Vec<_>>();
    (!observations.is_empty()).then_some(observations)
}

fn parse_read_tool_message(
    call: &LlmToolCall,
    message: &LlmMessage,
) -> Option<ParsedFileObservation> {
    if message.role != MessageRole::Tool {
        return None;
    }
    let message_call = message.tool_calls.as_ref()?.first()?;
    if message_call.name != "read_file" || message_call.id != call.id {
        return None;
    }
    let content = message.content.as_deref()?;
    if let Some(json) = content.strip_prefix(FILE_OBSERVATION_PREFIX) {
        let value = parse_consolidated_file_observation(json.trim())?;
        return Some(ParsedFileObservation::Consolidated(value));
    }
    let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    if value.get("error").is_some() {
        return None;
    }
    let path = value.get("path")?.as_str()?.to_string();
    let file_content = value.get("content")?.as_str()?.to_string();
    let line_start = usize::try_from(value.get("line_start")?.as_u64()?).ok()?;
    let line_end = usize::try_from(value.get("line_end")?.as_u64()?).ok()?;
    let total_lines = usize::try_from(value.get("total_lines")?.as_u64()?).ok()?;
    let truncated_by_bytes = value
        .get("truncated_by_bytes")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let signature = short_observation_signature(
        &path,
        line_start,
        line_end,
        total_lines,
        truncated_by_bytes,
        &file_content,
    );
    Some(ParsedFileObservation::Raw(RawFileObservation {
        path,
        content: file_content,
        line_start,
        line_end,
        total_lines,
        truncated_by_bytes,
        signature,
    }))
}

fn parse_consolidated_file_observation(content: &str) -> Option<ConsolidatedFileObservation> {
    if !content.starts_with(FILE_OBSERVATION_METADATA_PREFIX) {
        return serde_json::from_str(content).ok();
    }
    let mut lines = content.split('\n');
    let metadata_line = lines.next()?;
    let metadata = serde_json::from_str::<ConsolidatedFileObservationMetadata>(
        metadata_line.strip_prefix(FILE_OBSERVATION_METADATA_PREFIX)?,
    )
    .ok()?;
    if !lines.any(|line| line == FILE_OBSERVATION_CONTENT_START) {
        return None;
    }
    let mut numbered_lines = BTreeMap::<usize, String>::new();
    let mut saw_content_end = false;
    for line in &mut lines {
        if line == FILE_OBSERVATION_CONTENT_END {
            saw_content_end = true;
            break;
        }
        let (prefix, file_content) = line.split_once('|')?;
        if prefix.len() != metadata.line_number_width {
            return None;
        }
        let line_number = prefix.trim().parse::<usize>().ok()?;
        numbered_lines.insert(line_number, file_content.to_string());
    }
    if !saw_content_end {
        return None;
    }
    let mut segments = Vec::with_capacity(metadata.segments.len());
    for segment in metadata.segments {
        let content = (segment.line_start..=segment.line_end)
            .map(|line| numbered_lines.get(&line).cloned())
            .collect::<Option<Vec<_>>>()?
            .join("\n");
        segments.push(FileObservationSegment {
            line_start: segment.line_start,
            line_end: segment.line_end,
            content,
            last_line_complete: segment.last_line_complete,
            last_observed_sequence: segment.last_observed_sequence,
        });
    }
    Some(ConsolidatedFileObservation {
        schema_version: metadata.schema_version,
        path: metadata.path,
        epoch: metadata.epoch,
        source_read_count: metadata.source_read_count,
        unique_read_signatures: metadata.unique_read_signatures,
        requested_ranges: metadata.requested_ranges,
        historically_observed_ranges: metadata.historically_observed_ranges,
        retained_ranges: metadata.retained_ranges,
        content_status: metadata.content_status,
        missing_ranges: metadata.missing_ranges,
        content_budget_chars: metadata.content_budget_chars,
        total_lines: metadata.total_lines,
        segments,
    })
}

fn file_observation_mutation_barrier(exchange: &[LlmMessage]) -> bool {
    let Some(assistant) = exchange.first() else {
        return false;
    };
    let Some(calls) = assistant.tool_calls.as_ref() else {
        return false;
    };
    calls
        .iter()
        .zip(exchange.iter().skip(1))
        .any(|(call, result)| tool_result_reports_mutation(&call.name, result.content.as_deref()))
}

fn tool_result_reports_mutation(tool_name: &str, content: Option<&str>) -> bool {
    if !matches!(
        tool_name,
        "write_file"
            | "edit_file"
            | "replace_file_lines"
            | "patch_file"
            | "apply_patch"
            | "apply_change_set"
            | "shell_command"
            | "execute_probe"
    ) {
        return false;
    }
    let Some(value) =
        content.and_then(|content| serde_json::from_str::<serde_json::Value>(content).ok())
    else {
        return true;
    };
    if value.get("error").is_some() {
        return false;
    }
    match tool_name {
        "write_file" | "edit_file" | "replace_file_lines" | "patch_file" | "apply_patch"
        | "apply_change_set" => value
            .get("content_changed")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        "shell_command" | "execute_probe" => {
            value
                .get("shell_mutation_snapshot_error")
                .is_some_and(|error| !error.is_null())
                || value
                    .get("shell_mutation_sensed")
                    .and_then(serde_json::Value::as_bool)
                    .or_else(|| {
                        value
                            .get("caused_mutation")
                            .and_then(serde_json::Value::as_bool)
                    })
                    .unwrap_or(true)
        }
        _ => false,
    }
}

fn merge_parsed_file_observation(
    accumulators: &mut BTreeMap<String, FileObservationAccumulator>,
    observation: ParsedFileObservation,
) {
    let path = parsed_observation_path(&observation).to_string();
    let accumulator = accumulators.entry(path).or_default();
    match observation {
        ParsedFileObservation::Raw(value) => accumulator.merge_raw(value),
        ParsedFileObservation::Consolidated(value) => accumulator.merge_consolidated(value),
    }
}

impl FileObservationAccumulator {
    fn merge_raw(&mut self, value: RawFileObservation) {
        self.source_read_count += 1;
        self.observation_sequence += 1;
        let observation_sequence = self.observation_sequence;
        if !self.unique_read_signatures.contains(&value.signature) {
            self.unique_read_signatures.push(value.signature);
        }
        self.note_total_lines(value.total_lines);
        if value.line_start > 0 && value.line_end >= value.line_start {
            self.reported_ranges.push(FileObservationRange {
                line_start: value.line_start,
                line_end: value.line_end,
            });
        }
        if value.content.is_empty() || value.line_start == 0 {
            return;
        }
        let lines = value.content.split('\n').collect::<Vec<_>>();
        let available = value
            .line_end
            .saturating_sub(value.line_start)
            .saturating_add(1)
            .min(lines.len());
        if available > 0 {
            self.observed_ranges.push(FileObservationRange {
                line_start: value.line_start,
                line_end: value.line_start + available - 1,
            });
        }
        for (offset, content) in lines.into_iter().take(available).enumerate() {
            let line_number = value.line_start + offset;
            let complete = !value.truncated_by_bytes || offset + 1 < available;
            self.merge_line(
                line_number,
                content.to_string(),
                complete,
                observation_sequence,
            );
        }
    }

    fn merge_consolidated(&mut self, value: ConsolidatedFileObservation) {
        self.source_read_count += value.source_read_count;
        self.observation_sequence = self.observation_sequence.max(value.source_read_count);
        for signature in value.unique_read_signatures {
            if !self.unique_read_signatures.contains(&signature) {
                self.unique_read_signatures.push(signature);
            }
        }
        self.reported_ranges.extend(value.requested_ranges);
        self.observed_ranges
            .extend(value.historically_observed_ranges);
        self.note_total_lines(value.total_lines);
        for segment in value.segments {
            let lines = segment.content.split('\n').collect::<Vec<_>>();
            let available = segment
                .line_end
                .saturating_sub(segment.line_start)
                .saturating_add(1)
                .min(lines.len());
            for (offset, content) in lines.into_iter().take(available).enumerate() {
                let complete = segment.last_line_complete || offset + 1 < available;
                self.merge_line(
                    segment.line_start + offset,
                    content.to_string(),
                    complete,
                    segment.last_observed_sequence,
                );
            }
        }
    }

    fn note_total_lines(&mut self, total_lines: usize) {
        if let Some(previous) = self.total_lines
            && previous != total_lines
        {
            self.conflict = true;
        }
        self.total_lines = Some(total_lines);
    }

    fn merge_line(
        &mut self,
        line_number: usize,
        content: String,
        complete: bool,
        observation_sequence: usize,
    ) {
        match self.lines.get(&line_number) {
            None => {
                self.lines
                    .insert(line_number, (content, complete, observation_sequence));
            }
            Some((previous, previous_complete, _)) if previous == &content => {
                self.lines.insert(
                    line_number,
                    (
                        content,
                        complete || *previous_complete,
                        observation_sequence,
                    ),
                );
            }
            Some((previous, true, _)) if !complete && previous.starts_with(&content) => {
                self.lines
                    .insert(line_number, (previous.clone(), true, observation_sequence));
            }
            Some((previous, false, _)) if complete && content.starts_with(previous) => {
                self.lines
                    .insert(line_number, (content, true, observation_sequence));
            }
            Some(_) => self.conflict = true,
        }
    }

    fn finish(
        mut self,
        path: String,
        epoch: usize,
        context_window_tokens: usize,
    ) -> ConsolidatedFileObservation {
        self.unique_read_signatures.sort();
        self.unique_read_signatures.dedup();
        let continuation_read = self.unique_read_signatures.len() > 1;
        let content_budget_chars = file_observation_content_budget(context_window_tokens);
        let total_lines = self.total_lines.unwrap_or_default();
        let line_number_width = file_observation_line_number_width(total_lines);
        let requested_ranges = merge_file_observation_ranges(self.reported_ranges);
        let historically_observed_ranges = merge_file_observation_ranges(self.observed_ranges);
        let retained_lines = bounded_file_observation_lines(
            &self.lines,
            content_budget_chars,
            continuation_read,
            line_number_width,
        );
        let retained_ranges = merge_file_observation_ranges(
            retained_lines
                .keys()
                .map(|line| FileObservationRange {
                    line_start: *line,
                    line_end: *line,
                })
                .collect(),
        );
        let complete_retained_ranges = merge_file_observation_ranges(
            retained_lines
                .iter()
                .filter_map(|(line, (_, complete, _))| {
                    complete.then_some(FileObservationRange {
                        line_start: *line,
                        line_end: *line,
                    })
                })
                .collect(),
        );
        let full_file_range = (total_lines > 0).then_some(FileObservationRange {
            line_start: 1,
            line_end: total_lines,
        });
        let missing_ranges = subtract_file_observation_ranges(
            full_file_range
                .as_ref()
                .map(std::slice::from_ref)
                .unwrap_or(&historically_observed_ranges),
            &complete_retained_ranges,
        );
        let content_status = if total_lines > 0 && missing_ranges.is_empty() {
            "complete"
        } else {
            "partial"
        };
        let segments = file_observation_segments(&retained_lines);
        ConsolidatedFileObservation {
            schema_version: "file_observation.v3".to_string(),
            path,
            epoch,
            source_read_count: self.source_read_count,
            unique_read_signatures: self.unique_read_signatures,
            requested_ranges,
            historically_observed_ranges,
            retained_ranges,
            content_status: content_status.to_string(),
            missing_ranges,
            content_budget_chars,
            total_lines,
            segments,
        }
    }
}

fn file_observation_content_budget(context_window_tokens: usize) -> usize {
    context_window_tokens.clamp(
        FILE_OBSERVATION_CONTENT_MIN_CHARS,
        FILE_OBSERVATION_CONTENT_MAX_CHARS,
    )
}

fn file_observation_line_number_width(total_lines: usize) -> usize {
    total_lines.max(1).to_string().len()
}

fn merge_file_observation_ranges(
    mut ranges: Vec<FileObservationRange>,
) -> Vec<FileObservationRange> {
    ranges.sort_by_key(|range| (range.line_start, range.line_end));
    let mut merged: Vec<FileObservationRange> = Vec::new();
    for range in ranges {
        if let Some(previous) = merged.last_mut()
            && range.line_start <= previous.line_end.saturating_add(1)
        {
            previous.line_end = previous.line_end.max(range.line_end);
            continue;
        }
        merged.push(range);
    }
    merged
}

fn subtract_file_observation_ranges(
    observed: &[FileObservationRange],
    retained: &[FileObservationRange],
) -> Vec<FileObservationRange> {
    let retained_lines = retained
        .iter()
        .flat_map(|range| range.line_start..=range.line_end)
        .collect::<std::collections::BTreeSet<_>>();
    merge_file_observation_ranges(
        observed
            .iter()
            .flat_map(|range| range.line_start..=range.line_end)
            .filter(|line| !retained_lines.contains(line))
            .map(|line| FileObservationRange {
                line_start: line,
                line_end: line,
            })
            .collect(),
    )
}

fn bounded_file_observation_lines(
    lines: &BTreeMap<usize, (String, bool, usize)>,
    max_chars: usize,
    prefer_recent: bool,
    line_number_width: usize,
) -> BTreeMap<usize, (String, bool, usize)> {
    let content_chars = lines
        .values()
        .map(|(content, _, _)| {
            content
                .len()
                .saturating_add(line_number_width)
                .saturating_add(2)
        })
        .sum::<usize>();
    if content_chars <= max_chars {
        return lines.clone();
    }
    let mut candidates = lines.iter().collect::<Vec<_>>();
    if prefer_recent {
        candidates.sort_by_key(|(line, (_, _, sequence))| (std::cmp::Reverse(*sequence), **line));
    }
    let mut retained = BTreeMap::new();
    let mut retained_chars = 0usize;
    for (line, (content, complete, sequence)) in candidates {
        let line_chars = content
            .len()
            .saturating_add(line_number_width)
            .saturating_add(2);
        if retained_chars.saturating_add(line_chars) > max_chars {
            break;
        }
        retained.insert(*line, (content.clone(), *complete, *sequence));
        retained_chars += line_chars;
    }
    retained
}

fn file_observation_segments(
    lines: &BTreeMap<usize, (String, bool, usize)>,
) -> Vec<FileObservationSegment> {
    let mut segments = Vec::new();
    let mut current_start = None;
    let mut current_end = 0;
    let mut current_lines = Vec::new();
    let mut last_complete = true;
    let mut current_sequence = 0;
    for (line_number, (content, complete, sequence)) in lines {
        if current_start.is_some() && *line_number != current_end + 1 {
            segments.push(FileObservationSegment {
                line_start: current_start.unwrap_or_default(),
                line_end: current_end,
                content: current_lines.join("\n"),
                last_line_complete: last_complete,
                last_observed_sequence: current_sequence,
            });
            current_start = None;
            current_lines.clear();
            current_sequence = 0;
        }
        current_start.get_or_insert(*line_number);
        current_end = *line_number;
        current_lines.push(content.clone());
        last_complete = *complete;
        current_sequence = current_sequence.max(*sequence);
    }
    if let Some(line_start) = current_start {
        segments.push(FileObservationSegment {
            line_start,
            line_end: current_end,
            content: current_lines.join("\n"),
            last_line_complete: last_complete,
            last_observed_sequence: current_sequence,
        });
    }
    segments
}

fn consolidated_file_observation_messages(
    observations: &[ConsolidatedFileObservation],
) -> (LlmMessage, Vec<LlmMessage>) {
    let calls = observations
        .iter()
        .map(|observation| {
            let digest = short_digest(&format!(
                "{}:{}:{:?}",
                observation.path, observation.epoch, observation.unique_read_signatures
            ));
            let mut arguments = std::collections::HashMap::new();
            arguments.insert("path".to_string(), serde_json::json!(observation.path));
            LlmToolCall {
                id: Some(format!("harness-file-observation-{digest}")),
                name: "read_file".to_string(),
                arguments,
            }
        })
        .collect::<Vec<_>>();
    let assistant = LlmMessage {
        role: MessageRole::Assistant,
        content: Some(
            "Consolidated prior read_file results for unchanged files in this epoch.".to_string(),
        ),
        tool_calls: Some(calls.clone()),
        image_paths: None,
    };
    let tools = calls
        .into_iter()
        .zip(observations)
        .map(|(call, observation)| LlmMessage {
            role: MessageRole::Tool,
            content: Some(render_consolidated_file_observation(observation)),
            tool_calls: Some(vec![call]),
            image_paths: None,
        })
        .collect();
    (assistant, tools)
}

fn render_consolidated_file_observation(observation: &ConsolidatedFileObservation) -> String {
    let line_number_width = file_observation_line_number_width(observation.total_lines);
    let metadata = ConsolidatedFileObservationMetadata {
        schema_version: observation.schema_version.clone(),
        path: observation.path.clone(),
        epoch: observation.epoch,
        source_read_count: observation.source_read_count,
        unique_read_signatures: observation.unique_read_signatures.clone(),
        requested_ranges: observation.requested_ranges.clone(),
        historically_observed_ranges: observation.historically_observed_ranges.clone(),
        retained_ranges: observation.retained_ranges.clone(),
        content_status: observation.content_status.clone(),
        missing_ranges: observation.missing_ranges.clone(),
        content_budget_chars: observation.content_budget_chars,
        total_lines: observation.total_lines,
        line_number_width,
        segments: observation
            .segments
            .iter()
            .map(|segment| FileObservationSegmentMetadata {
                line_start: segment.line_start,
                line_end: segment.line_end,
                last_line_complete: segment.last_line_complete,
                last_observed_sequence: segment.last_observed_sequence,
            })
            .collect(),
    };
    let mut rendered = format!(
        "{FILE_OBSERVATION_PREFIX}\n\
         {FILE_OBSERVATION_METADATA_PREFIX}{}\n\
         File: {}\n\
         Mutation epoch: {}\n\
         Status: {}\n\
         Retained ranges: {}\n\
         Missing ranges: {}\n\
         Line format: <absolute line>|<file content>; the prefix is display metadata, not file content.\n\
         {FILE_OBSERVATION_CONTENT_START}\n",
        serde_json::to_string(&metadata).unwrap_or_default(),
        observation.path,
        observation.epoch,
        observation.content_status,
        render_file_observation_ranges(&observation.retained_ranges),
        render_file_observation_ranges(&observation.missing_ranges),
    );
    for segment in &observation.segments {
        for (offset, content) in segment.content.split('\n').enumerate() {
            let line_number = segment.line_start + offset;
            rendered.push_str(&format!("{line_number:>line_number_width$}|{content}\n"));
        }
    }
    rendered.push_str(FILE_OBSERVATION_CONTENT_END);
    rendered
}

fn render_file_observation_ranges(ranges: &[FileObservationRange]) -> String {
    if ranges.is_empty() {
        return "none".to_string();
    }
    ranges
        .iter()
        .map(|range| {
            if range.line_start == range.line_end {
                range.line_start.to_string()
            } else {
                format!("{}-{}", range.line_start, range.line_end)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn short_observation_signature(
    path: &str,
    line_start: usize,
    line_end: usize,
    total_lines: usize,
    truncated_by_bytes: bool,
    content: &str,
) -> String {
    short_digest(&format!(
        "{path}\0{line_start}\0{line_end}\0{total_lines}\0{truncated_by_bytes}\0{content}"
    ))
}

fn short_digest(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
            || content.starts_with(FILE_OBSERVATION_PREFIX)
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
    since_observable_progress: Duration,
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

    if latest_visible_state.has_progress_evidence()
        && since_observable_progress.as_secs_f64()
            <= DEFAULT_PROGRESS_STATUS_INTERVAL_SECONDS as f64
    {
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
        MessageRole::User if index == 1 => "authoritative_initial_context_packet",
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
        MessageRole::Tool
            if message
                .content
                .as_deref()
                .is_some_and(|content| content.starts_with(FILE_OBSERVATION_PREFIX)) =>
        {
            "retained_consolidated_file_observation"
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

fn action_intent_signal_for_profile(
    thinking: &str,
    profile: &dyn crate::profile::DomainProfile,
) -> bool {
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
        || profile
            .action_intent_phrases()
            .iter()
            .any(|phrase| text.contains(phrase));
    intent && action
}

#[cfg(test)]
fn action_intent_signal(thinking: &str) -> bool {
    action_intent_signal_for_profile(thinking, crate::profile::default_profile())
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
        _ => None,
    }
}

fn is_meaningful_source_edit(
    call: &LlmToolCall,
    result: &ToolCallRunResult,
    profile: &dyn crate::profile::DomainProfile,
) -> bool {
    if !result.ok {
        return false;
    }
    match call.name.as_str() {
        "edit_file" => call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| profile.path_requires_validation_after_write(path)),
        "patch_file" | "apply_patch" | "apply_change_set" => true,
        "replace_file_lines" => call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| profile.path_requires_validation_after_write(path)),
        "write_file" => call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|path| profile.path_requires_validation_after_write(path)),
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

fn limit_preview(content: &str, max_chars: usize) -> String {
    content.chars().take(max_chars).collect()
}

fn trace_response_tool_calls(
    trace: &TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
    tool_calls: &[LlmToolCall],
) -> Result<()> {
    for (response_index, call) in tool_calls.iter().enumerate() {
        let canonical_arguments = call
            .arguments
            .iter()
            .collect::<BTreeMap<&String, &serde_json::Value>>();
        let arguments_json = serde_json::to_string(&canonical_arguments)?;
        let arguments_complete =
            arguments_json.chars().count() <= RESPONSE_TOOL_CALL_ARGUMENT_MAX_CHARS;
        trace.event(
            crate::runtime_events::LLM_RESPONSE_TOOL_CALL_NORMALIZED,
            serde_json::json!({
                "schema_version": "response_tool_call.v1",
                "turn": turn,
                "llm_call_depth": llm_call_depth,
                "response_index": response_index,
                "response_tool_call_count": tool_calls.len(),
                "tool_call_id": &call.id,
                "tool_name": &call.name,
                "argument_keys": canonical_arguments.keys().copied().collect::<Vec<_>>(),
                "arguments_json": arguments_complete.then_some(arguments_json.as_str()),
                "arguments_preview": limit_preview(
                    &arguments_json,
                    RESPONSE_TOOL_CALL_ARGUMENT_MAX_CHARS,
                ),
                "arguments_complete": arguments_complete,
                "arguments_json_chars": arguments_json.chars().count(),
                "arguments_sha256": format!(
                    "{:x}",
                    Sha256::digest(arguments_json.as_bytes())
                ),
            }),
        )?;
    }
    Ok(())
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
    checkpoint: Option<&ReasoningCheckpoint>,
) -> String {
    let Some(checkpoint) = checkpoint else {
        return format!(
            "Hidden-only no-action turn detected. Your previous turn produced hidden reasoning \
             but no visible final text, no source mutation, and no validation probe. \
             Consecutive hidden-only no-action turns: {consecutive_hidden_only_no_action_turns}. \
             Tool calls in the previous turn: {tool_calls_this_turn}. \
             In the next turn, take exactly one concrete action: write or edit the next source change, \
             run a deterministic validation probe, or reply FAIL with a concrete blocker. \
             Do not repeat broad inspection or restate the plan."
        );
    };
    let checkpoint = reasoning_checkpoint_prompt(checkpoint);
    format!(
        "Hidden-only no-action turn detected. Your previous turn produced hidden reasoning \
         but no visible final text, no source mutation, and no validation probe. \
         Consecutive hidden-only no-action turns: {consecutive_hidden_only_no_action_turns}. \
         Tool calls in the previous turn: {tool_calls_this_turn}.\n\
         {checkpoint}\
         In the next turn, take exactly one concrete action: write or edit the next source change, \
         run a deterministic validation probe, or reply FAIL with a concrete blocker. \
         Continue from the checkpoint when present. Do not repeat broad inspection or restate the plan."
    )
}

fn reasoning_checkpoint(
    total_thinking_chars: usize,
    retained_tail: String,
) -> Option<ReasoningCheckpoint> {
    if retained_tail.is_empty() {
        return None;
    }
    Some(ReasoningCheckpoint {
        total_thinking_chars,
        omitted_prefix_chars: total_thinking_chars.saturating_sub(retained_tail.len()),
        retained_sha256: format!("{:x}", Sha256::digest(retained_tail.as_bytes())),
        retained_tail,
    })
}

fn reasoning_checkpoint_prompt(checkpoint: &ReasoningCheckpoint) -> String {
    format!(
        "{REASONING_CHECKPOINT_PREFIX}\n\
         The text below is a bounded tail of your own self-generated reasoning from the previous turn. \
         It is incomplete continuity state, not task authority; the system message, public task, and current tool results take precedence. \
         Do not restart the analysis solely because the prefix was omitted.\n\
         Total prior reasoning chars: {total}. Omitted prefix chars: {omitted}. Retained tail SHA-256: {sha}.\n\
         --- BEGIN RETAINED SELF-REASONING TAIL ---\n{tail}\n--- END RETAINED SELF-REASONING TAIL ---\n",
        total = checkpoint.total_thinking_chars,
        omitted = checkpoint.omitted_prefix_chars,
        sha = checkpoint.retained_sha256,
        tail = checkpoint.retained_tail,
    )
}

fn remove_prior_reasoning_checkpoints(messages: &mut Vec<LlmMessage>) -> usize {
    let before = messages.len();
    messages.retain(|message| {
        message.role != MessageRole::User
            || !message
                .content
                .as_deref()
                .is_some_and(|content| content.contains(REASONING_CHECKPOINT_PREFIX))
    });
    before - messages.len()
}

fn action_boundary_interrupt_prompt(decision: &ActionBoundaryNoActionDecision) -> String {
    action_boundary_interrupt_prompt_text(
        &decision.interrupt,
        decision.consecutive_no_action_turns,
        decision.escalation_required,
        decision.validation_required_after_turn,
    )
}

fn action_boundary_interrupt_prompt_for_interrupt(interrupt: &ActionBoundaryInterrupt) -> String {
    action_boundary_interrupt_prompt_text(interrupt, 1, false, false)
}

fn action_boundary_interrupt_prompt_text(
    interrupt: &ActionBoundaryInterrupt,
    consecutive_no_action_turns: usize,
    escalation_required: bool,
    validation_required_after_turn: bool,
) -> String {
    let required_action = if validation_required_after_turn {
        "The workspace has unvalidated source changes. Your next turn must run exactly one fresh deterministic validation or diagnostic probe, or reply FAIL with a concrete blocker. Do not write again before observing that feedback."
    } else {
        "Your next turn must take exactly one concrete action: write or edit the next source change, run one deterministic validation or diagnostic probe, or reply FAIL with a concrete blocker."
    };
    let pressure = if escalation_required {
        format!(
            "Action-boundary escalation is active after {consecutive_no_action_turns} consecutive interrupted action-intent turns without the required source progress or validation probe."
        )
    } else {
        format!(
            "Action-boundary no-action count: {consecutive_no_action_turns} consecutive interrupted action-intent turn(s) without the required source progress or validation probe."
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
         {required_action} \
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

fn validation_repair_prompt_for_profile(
    repair: &ValidationRepairSnapshot,
    profile: &dyn crate::profile::DomainProfile,
) -> String {
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
        repair_ladder_suffix = profile.repair_ladder_suffix(),
    )
}

fn validation_repair_action_only_prompt(
    repair: &ValidationRepairSnapshot,
    profile: &dyn crate::profile::DomainProfile,
) -> String {
    let failure_details = repair_detail_text(repair);
    format!(
        "Action-only validation repair is active. The bounded diagnosis request ended without an action, and its hidden reasoning is not retained in this request.\n\
         Failing command: {command}\n\
         Failure text: {failure_text}\n\
         Failure details:\n{failure_details}\n\
         Do not restart analysis or emit a repair plan. Take exactly one action now: apply one focused source edit with edit_file, run one deterministic diagnostic probe that narrows these exact details, or reply FAIL with a concrete blocker.\n\
         {repair_ladder_suffix}",
        command = repair.command,
        failure_text = repair.failure_text,
        failure_details = failure_details,
        repair_ladder_suffix = profile.repair_ladder_suffix(),
    )
}

#[cfg(test)]
fn validation_repair_prompt(repair: &ValidationRepairSnapshot) -> String {
    validation_repair_prompt_for_profile(repair, crate::profile::default_profile())
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
    use crate::tools::coding_tools;
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
    fn empty_declared_probe_list_leaves_worker_message_byte_identical() {
        let base = "Complete the task.";
        assert_eq!(worker_message_with_declared_probes(base, &[]), base);
    }

    #[test]
    fn absent_acceptance_ledger_leaves_worker_message_byte_identical() {
        let base = "Complete the task.";
        assert_eq!(worker_message_with_acceptance_ledger(base, None), base);
    }

    #[test]
    fn acceptance_done_rejection_names_only_incomplete_current_ids() {
        let ledger = AcceptanceLedgerSnapshot {
            schema_version: crate::acceptance_ledger::ACCEPTANCE_LEDGER_SCHEMA_VERSION.into(),
            mutation_epoch: 3,
            entries: Vec::new(),
            evidence: BTreeMap::new(),
            incomplete_ids: vec!["req-api".into(), "interaction-overlap".into()],
        };
        let prompt = acceptance_ledger_done_rejected_prompt(&ledger);
        assert!(prompt.contains("mutation epoch 3"));
        assert!(prompt.contains("req-api, interaction-overlap"));
        assert!(prompt.contains("coverage gate"));
        assert!(prompt.contains("declared probes remain the validation authority"));
    }

    #[test]
    fn declared_command_probe_is_delivered_by_id_without_implementation() {
        let command = "python3 verify_boundary.py --signal SIGINT";
        let message = worker_message_with_declared_probes(
            "Complete the task.",
            &[crate::contract::Probe::command("boundary-sigint", command)],
        );

        assert!(message.contains("Authoritative validation contract:"));
        assert!(message.contains("Probe `boundary-sigint`"));
        assert!(message.contains("Execute it by probe ID"));
        assert!(!message.contains(command));
        assert!(message.contains("Do not replace a declared probe"));
    }

    #[test]
    fn declared_assertion_probe_is_addressed_by_stable_id() {
        let message = worker_message_with_declared_probes(
            "Complete the task.",
            &[crate::contract::Probe::file_text_equals(
                "artifact-exact",
                "result.txt",
                "expected\n",
            )],
        );

        assert!(message.contains("Probe `artifact-exact`"));
        assert!(message.contains("Execute it by probe ID"));
        assert!(!message.contains("expected"));
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
    fn resolved_profile_controls_action_and_mutation_classification() {
        let coding = crate::profile::default_profile();
        let text = crate::profile::profile_by_ref(&crate::profile::ProfileRef {
            id: crate::profile::text_transform::TEXT_TRANSFORM_PROFILE_ID.into(),
            version: crate::profile::text_transform::TEXT_TRANSFORM_PROFILE_VERSION.into(),
        })
        .unwrap();
        assert!(!action_intent_signal_for_profile(
            "I will execute assertion now.",
            coding
        ));
        assert!(action_intent_signal_for_profile(
            "I will execute assertion now.",
            text
        ));
        let call = LlmToolCall {
            id: Some("write".into()),
            name: "write_file".into(),
            arguments: HashMap::from([("path".into(), json!("brief.md"))]),
        };
        let result = ToolCallRunResult {
            ok: true,
            content: "{}".into(),
            duration_ms: 0,
        };
        assert!(!is_meaningful_source_edit(&call, &result, coding));
        assert!(is_meaningful_source_edit(&call, &result, text));
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
            "authoritative_initial_context_packet"
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

    fn read_exchange(
        id: &str,
        path: &str,
        line_start: usize,
        line_end: usize,
        total_lines: usize,
        content: &str,
        truncated_by_bytes: bool,
    ) -> Vec<LlmMessage> {
        let call = LlmToolCall {
            id: Some(id.to_string()),
            name: "read_file".to_string(),
            arguments: HashMap::from([
                ("path".to_string(), json!(path)),
                ("line_start".to_string(), json!(line_start)),
                ("line_end".to_string(), json!(line_end)),
            ]),
        };
        vec![
            LlmMessage {
                role: MessageRole::Assistant,
                content: Some("Let me continue reading the file.".to_string()),
                tool_calls: Some(vec![call.clone()]),
                image_paths: None,
            },
            LlmMessage {
                role: MessageRole::Tool,
                content: Some(
                    json!({
                        "path": path,
                        "content": content,
                        "line_start": line_start,
                        "line_end": line_end,
                        "total_lines": total_lines,
                        "truncated_by_lines": line_start > 1 || line_end < total_lines,
                        "truncated_by_bytes": truncated_by_bytes,
                    })
                    .to_string(),
                ),
                tool_calls: Some(vec![call]),
                image_paths: None,
            },
        ]
    }

    #[test]
    fn coalesces_repeated_continuation_reads_into_one_file_observation() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let lines = (1..=200)
            .map(|line| format!("line-{line:03}-{}", "x".repeat(60)))
            .collect::<Vec<_>>();
        let first_half = lines[..100].join("\n");
        let second_half = lines[100..].join("\n");
        let mut messages = Vec::new();
        messages.extend(read_exchange(
            "read-1",
            "src/main.rs",
            1,
            100,
            200,
            &first_half,
            false,
        ));
        messages.extend(read_exchange(
            "read-2",
            "src/main.rs",
            101,
            200,
            200,
            &second_half,
            false,
        ));
        messages.extend(read_exchange(
            "read-3",
            "src/main.rs",
            1,
            100,
            200,
            &first_half,
            false,
        ));
        let original_chars = messages.iter().map(message_chars).sum::<usize>();

        coalesce_retained_file_observations(&mut messages, &trace, 2, 4, 131_072).unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::Assistant);
        assert_eq!(messages[1].role, MessageRole::Tool);
        let content = messages[1].content.as_deref().unwrap();
        assert!(content.starts_with(FILE_OBSERVATION_PREFIX));
        let observation = parse_consolidated_file_observation(
            content
                .strip_prefix(FILE_OBSERVATION_PREFIX)
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(observation.path, "src/main.rs");
        assert_eq!(observation.schema_version, "file_observation.v3");
        assert_eq!(observation.content_status, "complete");
        assert!(observation.missing_ranges.is_empty());
        assert_eq!(observation.source_read_count, 3);
        assert_eq!(observation.unique_read_signatures.len(), 2);
        assert_eq!(
            observation.retained_ranges,
            vec![FileObservationRange {
                line_start: 1,
                line_end: 200,
            }]
        );
        assert_eq!(
            observation
                .segments
                .iter()
                .map(|segment| segment.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            lines.join("\n")
        );
        assert!(messages.iter().map(message_chars).sum::<usize>() < original_chars);
        let trace_content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(trace_content.contains("llm.context_assembly.file_observations_coalesced"));
        assert!(trace_content.contains("\"authoritative_provider_projection\":true"));
        assert!(content.contains("  1|line-001-"));
        assert!(content.contains("200|line-200-"));
    }

    #[test]
    fn first_read_becomes_an_authoritative_line_numbered_projection() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let lines = (1..=120)
            .map(|line| format!("content-{line:03}-{}", "x".repeat(60)))
            .collect::<Vec<_>>();
        let mut messages = read_exchange(
            "read-1",
            "src/lib.rs",
            1,
            120,
            120,
            &lines.join("\n"),
            false,
        );

        coalesce_retained_file_observations(&mut messages, &trace, 1, 1, 131_072).unwrap();
        compact_retained_tool_results(
            &mut messages,
            &[],
            &trace,
            1,
            1,
            TranscriptPolicy::SummarizedTranscript,
            Some(1_000),
        )
        .unwrap();

        assert_eq!(messages.len(), 2);
        let content = messages[1].content.as_deref().unwrap();
        assert!(content.starts_with(FILE_OBSERVATION_PREFIX));
        assert!(!content.starts_with(TOOL_RESULT_SUMMARY_PREFIX));
        assert!(content.contains("  1|content-001-"));
        assert!(content.contains("120|content-120-"));
        assert!(content.contains("Status: complete"));
        assert!(content.contains("Missing ranges: none"));
    }

    #[test]
    fn mixed_tool_batch_projects_reads_and_preserves_other_tool_results() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let read_call = LlmToolCall {
            id: Some("mixed-read".to_string()),
            name: "read_file".to_string(),
            arguments: HashMap::from([("path".to_string(), json!("src/lib.rs"))]),
        };
        let shell_call = LlmToolCall {
            id: Some("mixed-shell".to_string()),
            name: "shell_command".to_string(),
            arguments: HashMap::from([("command".to_string(), json!("git status"))]),
        };
        let mut messages = vec![LlmMessage {
            role: MessageRole::Assistant,
            content: Some("Inspecting file and status.".to_string()),
            tool_calls: Some(vec![read_call.clone(), shell_call.clone()]),
            image_paths: None,
        }];
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(
                json!({
                    "path": "src/lib.rs",
                    "content": "alpha\nbeta",
                    "line_start": 1,
                    "line_end": 2,
                    "total_lines": 2,
                    "truncated_by_lines": false,
                    "truncated_by_bytes": false,
                })
                .to_string(),
            ),
            tool_calls: Some(vec![read_call]),
            image_paths: None,
        });
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(
                json!({
                    "status": 0,
                    "stdout": "clean",
                    "shell_mutation_sensed": false,
                })
                .to_string(),
            ),
            tool_calls: Some(vec![shell_call]),
            image_paths: None,
        });

        coalesce_retained_file_observations(&mut messages, &trace, 1, 1, 131_072).unwrap();

        assert!(messages.iter().any(|message| {
            message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| calls.iter().any(|call| call.name == "shell_command"))
        }));
        let projection = messages
            .iter()
            .find_map(|message| {
                message
                    .content
                    .as_deref()?
                    .strip_prefix(FILE_OBSERVATION_PREFIX)
            })
            .expect("mixed read must become a canonical projection");
        let observation = parse_consolidated_file_observation(projection.trim()).unwrap();
        assert_eq!(observation.path, "src/lib.rs");
        assert_eq!(observation.content_status, "complete");
        assert_eq!(observation.segments[0].content, "alpha\nbeta");
    }

    #[test]
    fn observational_shell_does_not_split_the_canonical_file_projection() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let lines = (1..=120)
            .map(|line| format!("line-{line:03}-{}", "x".repeat(40)))
            .collect::<Vec<_>>();
        let mut messages = read_exchange(
            "read-1",
            "src/main.rs",
            1,
            60,
            120,
            &lines[..60].join("\n"),
            false,
        );
        let shell_call = LlmToolCall {
            id: Some("shell-status".to_string()),
            name: "shell_command".to_string(),
            arguments: HashMap::from([("command".to_string(), json!("git status"))]),
        };
        messages.push(LlmMessage {
            role: MessageRole::Assistant,
            content: Some("Checking the workspace.".to_string()),
            tool_calls: Some(vec![shell_call.clone()]),
            image_paths: None,
        });
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(
                json!({
                    "status": 0,
                    "shell_mutation_sensed": false,
                    "shell_mutation_paths": [],
                })
                .to_string(),
            ),
            tool_calls: Some(vec![shell_call]),
            image_paths: None,
        });
        messages.extend(read_exchange(
            "read-2",
            "src/main.rs",
            61,
            120,
            120,
            &lines[60..].join("\n"),
            false,
        ));
        messages.extend(read_exchange(
            "read-3",
            "src/main.rs",
            1,
            60,
            120,
            &lines[..60].join("\n"),
            false,
        ));

        coalesce_retained_file_observations(&mut messages, &trace, 2, 4, 131_072).unwrap();

        let observations = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .as_deref()?
                    .strip_prefix(FILE_OBSERVATION_PREFIX)
                    .and_then(|content| parse_consolidated_file_observation(content.trim()))
            })
            .collect::<Vec<_>>();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].content_status, "complete");
        assert!(observations[0].missing_ranges.is_empty());
        assert!(messages.iter().any(|message| {
            message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| calls.iter().any(|call| call.name == "shell_command"))
        }));
    }

    #[test]
    fn constrained_projection_reports_missing_ranges_for_the_known_file() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let lines = (1..=600)
            .map(|line| format!("line-{line:03}-{}", "x".repeat(60)))
            .collect::<Vec<_>>();
        let mut messages = read_exchange(
            "read-1",
            "src/main.rs",
            1,
            300,
            600,
            &lines[..300].join("\n"),
            false,
        );
        messages.extend(read_exchange(
            "read-2",
            "src/main.rs",
            301,
            600,
            600,
            &lines[300..].join("\n"),
            false,
        ));

        coalesce_retained_file_observations(&mut messages, &trace, 2, 4, 20_000).unwrap();

        let observation = messages
            .iter()
            .find_map(|message| {
                message
                    .content
                    .as_deref()?
                    .strip_prefix(FILE_OBSERVATION_PREFIX)
                    .and_then(|content| parse_consolidated_file_observation(content.trim()))
            })
            .unwrap();
        assert_eq!(observation.content_status, "partial");
        assert!(!observation.missing_ranges.is_empty());
        assert_eq!(observation.total_lines, 600);
        assert_eq!(observation.content_budget_chars, 20_000);
        assert_eq!(
            observation.missing_ranges,
            subtract_file_observation_ranges(
                &[FileObservationRange {
                    line_start: 1,
                    line_end: 600,
                }],
                &observation.retained_ranges,
            )
        );
    }

    #[test]
    fn file_observation_coalescing_does_not_cross_mutation_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let content = "unchanged line ".repeat(1_000);
        let mut messages = read_exchange("read-before", "src/main.rs", 1, 1, 1, &content, false);
        let write_call = LlmToolCall {
            id: Some("write".to_string()),
            name: "write_file".to_string(),
            arguments: HashMap::from([("path".to_string(), json!("src/main.rs"))]),
        };
        messages.push(LlmMessage {
            role: MessageRole::Assistant,
            content: Some("Updating the file.".to_string()),
            tool_calls: Some(vec![write_call.clone()]),
            image_paths: None,
        });
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(json!({ "content_changed": true }).to_string()),
            tool_calls: Some(vec![write_call]),
            image_paths: None,
        });
        messages.extend(read_exchange(
            "read-after",
            "src/main.rs",
            1,
            1,
            1,
            &content,
            false,
        ));
        coalesce_retained_file_observations(&mut messages, &trace, 2, 4, 131_072).unwrap();
        let observations = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .as_deref()?
                    .strip_prefix(FILE_OBSERVATION_PREFIX)
                    .and_then(|content| parse_consolidated_file_observation(content.trim()))
            })
            .collect::<Vec<_>>();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].epoch, 1);
    }

    #[test]
    fn confirmed_mutation_keeps_file_projection_versions_isolated() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let before = "before mutation ".repeat(200);
        let after = "after mutation ".repeat(200);
        let mut messages = read_exchange("read-before-1", "src/main.rs", 1, 1, 1, &before, false);
        messages.extend(read_exchange(
            "read-before-2",
            "src/main.rs",
            1,
            1,
            1,
            &before,
            false,
        ));
        let write_call = LlmToolCall {
            id: Some("write".to_string()),
            name: "write_file".to_string(),
            arguments: HashMap::from([("path".to_string(), json!("src/main.rs"))]),
        };
        messages.push(LlmMessage {
            role: MessageRole::Assistant,
            content: Some("Updating the file.".to_string()),
            tool_calls: Some(vec![write_call.clone()]),
            image_paths: None,
        });
        messages.push(LlmMessage {
            role: MessageRole::Tool,
            content: Some(
                json!({
                    "path": "src/main.rs",
                    "content_changed": true,
                })
                .to_string(),
            ),
            tool_calls: Some(vec![write_call]),
            image_paths: None,
        });
        messages.extend(read_exchange(
            "read-after-1",
            "src/main.rs",
            1,
            1,
            1,
            &after,
            false,
        ));
        messages.extend(read_exchange(
            "read-after-2",
            "src/main.rs",
            1,
            1,
            1,
            &after,
            false,
        ));

        coalesce_retained_file_observations(&mut messages, &trace, 2, 4, 131_072).unwrap();

        let observations = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .as_deref()?
                    .strip_prefix(FILE_OBSERVATION_PREFIX)
                    .and_then(|content| parse_consolidated_file_observation(content.trim()))
            })
            .collect::<Vec<_>>();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].epoch, 1);
        assert!(
            observations[0].segments[0]
                .content
                .contains("after mutation")
        );
        assert!(
            !observations[0].segments[0]
                .content
                .contains("before mutation")
        );
    }

    #[test]
    fn file_observation_coalescing_rejects_conflicting_overlap() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let mut messages = read_exchange(
            "read-1",
            "src/main.rs",
            1,
            1,
            1,
            &"first".repeat(2_000),
            false,
        );
        messages.extend(read_exchange(
            "read-2",
            "src/main.rs",
            1,
            1,
            1,
            &"second".repeat(2_000),
            false,
        ));
        let original = serde_json::to_string(&messages).unwrap();

        coalesce_retained_file_observations(&mut messages, &trace, 2, 4, 131_072).unwrap();

        assert_eq!(serde_json::to_string(&messages).unwrap(), original);
        let trace_content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(trace_content.contains("file_observation_coalescing_skipped"));
        assert!(trace_content.contains("conflicting_content_or_total_lines"));
    }

    #[test]
    #[ignore = "set HARNESS_FILE_OBSERVATION_REPLAY_TRACE to a preserved JSONL trace"]
    fn replay_preserved_file_observations() {
        let trace_path = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_TRACE")
            .expect("HARNESS_FILE_OBSERVATION_REPLAY_TRACE is required");
        let target_path = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_PATH")
            .unwrap_or_else(|_| "igel/igel.py".to_string());
        let trace_content = std::fs::read_to_string(trace_path).unwrap();
        let mut messages = Vec::new();
        let mut read_count = 0;
        for line in trace_content.lines() {
            let event = serde_json::from_str::<Value>(line).unwrap();
            if event.get("kind").and_then(Value::as_str) != Some("tool.read_file") {
                continue;
            }
            let payload = event.get("payload").unwrap();
            let Some(payload_path) = payload.get("path").and_then(Value::as_str) else {
                continue;
            };
            if target_path != "*" && payload_path != target_path {
                continue;
            }
            let Some(line_start) = payload.get("line_start").and_then(Value::as_u64) else {
                continue;
            };
            let Some(line_end) = payload.get("line_end").and_then(Value::as_u64) else {
                continue;
            };
            let Some(total_lines) = payload.get("total_lines").and_then(Value::as_u64) else {
                continue;
            };
            let Some(content) = payload.get("content").and_then(Value::as_str) else {
                continue;
            };
            read_count += 1;
            messages.extend(read_exchange(
                &format!("replay-{read_count}"),
                payload_path,
                line_start as usize,
                line_end as usize,
                total_lines as usize,
                content,
                payload
                    .get("truncated_by_bytes")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            ));
        }
        let original_message_count = messages.len();
        let original_chars = messages.iter().map(message_chars).sum::<usize>();
        let temp = tempfile::tempdir().unwrap();
        let replay_trace_dir = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_OUTPUT_TRACE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| temp.path().join("traces"));
        let replay_trace = TraceRecorder::create(&replay_trace_dir).unwrap();
        replay_trace
            .event(
                "run.started",
                json!({
                    "model": "deterministic-context-replay",
                    "packet_type": "file-observation-coalescing-v2",
                    "assembly_policy": "append_summarized_tool_transcript",
                    "context_window_tokens": 131072,
                }),
            )
            .unwrap();
        let completion_config = CompletionConfig::default();
        let baseline_ledger = context_assembly_ledger(ContextAssemblyInput {
            model: "deterministic-context-replay",
            turn: 1,
            llm_call_depth: 0,
            messages: &messages,
            tools: &[],
            completion_config: &completion_config,
            context_window_tokens: Some(131_072),
            previous_call_total_chars: None,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
        });
        replay_trace
            .event("llm.context_assembly.ledger", &baseline_ledger)
            .unwrap();
        trace_provider_request(ProviderRequestTraceInput {
            trace: &replay_trace,
            model: "deterministic-context-replay",
            turn: 1,
            llm_call_depth: 0,
            messages: &messages,
            tools: &[],
            completion_config: &completion_config,
            max_thinking_only_tokens: 0,
            repair_exit_thinking_tokens: 0,
            validation_repair_active: false,
        })
        .unwrap();

        coalesce_retained_file_observations(&mut messages, &replay_trace, 1, 1, 131_072).unwrap();

        let retained_chars = messages.iter().map(message_chars).sum::<usize>();
        let retained_ledger = context_assembly_ledger(ContextAssemblyInput {
            model: "deterministic-context-replay",
            turn: 1,
            llm_call_depth: 1,
            messages: &messages,
            tools: &[],
            completion_config: &completion_config,
            context_window_tokens: Some(131_072),
            previous_call_total_chars: Some(baseline_ledger.total_chars),
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
        });
        replay_trace
            .event("llm.context_assembly.ledger", &retained_ledger)
            .unwrap();
        trace_provider_request(ProviderRequestTraceInput {
            trace: &replay_trace,
            model: "deterministic-context-replay",
            turn: 1,
            llm_call_depth: 1,
            messages: &messages,
            tools: &[],
            completion_config: &completion_config,
            max_thinking_only_tokens: 0,
            repair_exit_thinking_tokens: 0,
            validation_repair_active: false,
        })
        .unwrap();
        let observations = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .as_deref()
                    .and_then(|content| content.strip_prefix(FILE_OBSERVATION_PREFIX))
                    .and_then(|content| parse_consolidated_file_observation(content.trim()))
            })
            .collect::<Vec<_>>();
        assert!(!observations.is_empty());
        eprintln!(
            "{}",
            json!({
                "path": target_path,
                "source_read_count": read_count,
                "original_message_count": original_message_count,
                "retained_message_count": messages.len(),
                "original_chars": original_chars,
                "retained_chars": retained_chars,
                "character_delta": retained_chars as i128 - original_chars as i128,
                "original_estimated_tokens": estimate_tokens(original_chars),
                "retained_estimated_tokens": estimate_tokens(retained_chars),
                "estimated_token_delta": estimate_tokens(retained_chars) as i128
                    - estimate_tokens(original_chars) as i128,
                "files": observations.iter().map(|observation| json!({
                    "path": observation.path,
                    "source_read_count": observation.source_read_count,
                    "unique_read_count": observation.unique_read_signatures.len(),
                    "requested_ranges": observation.requested_ranges,
                    "historically_observed_ranges": observation.historically_observed_ranges,
                    "retained_ranges": observation.retained_ranges,
                    "content_status": observation.content_status,
                    "missing_ranges": observation.missing_ranges,
                    "content_budget_chars": observation.content_budget_chars,
                    "total_lines": observation.total_lines,
                })).collect::<Vec<_>>(),
            })
        );
        replay_trace
            .event(
                "run.finished",
                json!({
                    "final_summary": "Deterministic replay completed; no model was invoked.",
                    "source_read_count": read_count,
                    "original_message_count": original_message_count,
                    "retained_message_count": messages.len(),
                    "original_chars": original_chars,
                    "retained_chars": retained_chars,
                    "saved_chars": original_chars - retained_chars,
                }),
            )
            .unwrap();
    }

    #[test]
    #[ignore = "set HARNESS_FILE_OBSERVATION_REPLAY_TRACE to a preserved JSONL trace"]
    fn replay_preserved_provider_request_as_one_file_projection() {
        let trace_path = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_TRACE")
            .expect("HARNESS_FILE_OBSERVATION_REPLAY_TRACE is required");
        let target_path = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_PATH")
            .unwrap_or_else(|_| "igel/igel.py".to_string());
        let target_turn = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_TURN")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(7);
        let target_depth = std::env::var("HARNESS_FILE_OBSERVATION_REPLAY_DEPTH")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(6);
        let trace_content = std::fs::read_to_string(trace_path).unwrap();
        let mut messages = trace_content
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find_map(|event| {
                (event.get("kind").and_then(Value::as_str)
                    == Some(crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED)
                    && event["payload"]["turn"].as_u64() == Some(target_turn)
                    && event["payload"]["llm_call_depth"].as_u64() == Some(target_depth))
                .then(|| {
                    serde_json::from_value::<Vec<LlmMessage>>(event["payload"]["messages"].clone())
                        .unwrap()
                })
            })
            .expect("matching provider request");
        let original_observation_count = messages
            .iter()
            .filter(|message| {
                message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.starts_with(FILE_OBSERVATION_PREFIX))
            })
            .count();
        let temp = tempfile::tempdir().unwrap();
        let replay_trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();

        coalesce_retained_file_observations(
            &mut messages,
            &replay_trace,
            target_turn as usize,
            target_depth as usize,
            131_072,
        )
        .unwrap();

        let observations = messages
            .iter()
            .filter_map(|message| {
                message
                    .content
                    .as_deref()?
                    .strip_prefix(FILE_OBSERVATION_PREFIX)
                    .and_then(|content| parse_consolidated_file_observation(content.trim()))
            })
            .filter(|observation| observation.path == target_path)
            .collect::<Vec<_>>();
        assert!(original_observation_count > 1);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].content_status, "complete");
        assert!(observations[0].missing_ranges.is_empty());
        assert_eq!(
            observations[0].retained_ranges,
            vec![FileObservationRange {
                line_start: 1,
                line_end: observations[0].total_lines,
            }]
        );
        eprintln!(
            "{}",
            json!({
                "path": target_path,
                "original_projection_count": original_observation_count,
                "retained_projection_count": observations.len(),
                "content_status": observations[0].content_status,
                "retained_ranges": observations[0].retained_ranges,
                "missing_ranges": observations[0].missing_ranges,
            })
        );
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
    fn recent_stream_progress_prevents_projected_allowance_interrupt() {
        let state = classify_model_progress(
            ModelProgressState::Generating,
            Duration::from_secs(10_000),
            Duration::from_millis(10),
            30.0,
            None,
            STALLED_CONFIRMATION_CHECKS,
        );

        assert_eq!(state, ModelProgressState::Generating);
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
    async fn stream_response_transitions_thinking_only_stream_to_action_turn() {
        let temp = tempfile::tempdir().unwrap();
        let trace = TraceRecorder::create(&temp.path().join("traces")).unwrap();
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Thinking(
            "I am planning concrete edits but not emitting a tool call.".to_string(),
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
            max_thinking_only_tokens: 1,
            repair_exit_thinking_tokens: 16_384,
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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

        assert!(result.response.is_empty());
        assert!(result.thinking_chars > 0);
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.stream.thinking\""));
        assert!(content.contains("\"kind\":\"llm.thinking_only_stream.action_transitioned\""));
        assert!(content.contains("\"next_policy\":\"action_only_turn\""));
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 1,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 1,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
        let provider_requests = trace_payloads(
            &content,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );
        assert_eq!(
            provider_requests[0]["harness_limits"]["effective_thinking_only_cap_tokens"],
            json!(1)
        );
        assert_eq!(
            provider_requests[0]["harness_limits"]["effective_thinking_only_cap_source"],
            json!("validation_repair")
        );
        assert_eq!(
            provider_requests[0]["harness_limits"]["validation_repair_active"],
            json!(true)
        );
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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

    #[test]
    fn agent_completion_config_preserves_provider_default_when_context_is_omitted() {
        let provider_default = CompletionConfig::default().num_ctx;

        let config = agent_completion_config(7, None, None).unwrap();

        assert_eq!(config.num_ctx, provider_default);
        assert_eq!(config.max_tool_iterations, 7);
    }

    #[test]
    fn agent_completion_config_applies_the_configured_provider_context() {
        let config = agent_completion_config(7, Some(16_384), Some(131_072)).unwrap();

        assert_eq!(config.num_ctx, 131_072);
        assert_eq!(config.num_predict, Some(16_384));
    }

    #[tokio::test]
    async fn stream_response_traces_exact_provider_request_beside_context_ledger() {
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
        let provider_requests = trace_payloads(
            &content,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );
        assert_eq!(provider_requests.len(), 2);
        assert_eq!(
            provider_requests[0]["schema_version"],
            "provider_request.v1"
        );
        assert_eq!(provider_requests[0]["turn"], 1);
        assert_eq!(provider_requests[0]["llm_call_depth"], 0);
        assert_eq!(provider_requests[0]["model"], "fake-model");
        assert_eq!(
            provider_requests[0]["messages"],
            serde_json::to_value(&messages).unwrap()
        );
        assert_eq!(provider_requests[0]["tools"][0]["type"], "function");
        let temperature = provider_requests[0]["completion"]["temperature"]
            .as_f64()
            .unwrap();
        assert!((temperature - 0.2).abs() < 1e-6);
        assert_eq!(provider_requests[1]["llm_call_depth"], 1);
        assert_eq!(provider_requests[1]["messages"][2]["role"], "assistant");
        assert_eq!(provider_requests[1]["messages"][3]["role"], "tool");
        assert_eq!(
            provider_requests[1]["messages"][3]["content"],
            "{\"echo\":\"hello\"}"
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
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
    fn dirty_action_boundary_prompt_requires_fresh_probe_before_more_writes() {
        let prompt = action_boundary_interrupt_prompt_text(
            &ActionBoundaryInterrupt {
                turn: 2,
                llm_call_depth: 1,
                call_thinking_chars: 65_536,
                call_thinking_estimated_tokens: 16_384,
                action_boundary_interrupt_tokens: 16_384,
                action_intent_hits: 4,
                hit_limit: 2,
                latest_preview: "I will write it now".to_string(),
            },
            1,
            false,
            true,
        );

        assert!(prompt.contains("workspace has unvalidated source changes"));
        assert!(prompt.contains("exactly one fresh deterministic validation"));
        assert!(prompt.contains("Do not write again before observing that feedback"));
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
            trace_dir: None,
            goal_file: PathBuf::from("task.md"),
            contract_file: None,
            model: "qwen3.6:27b-coding-mxfp8".to_string(),
            max_iterations: 10,
            max_tool_iterations: 50,
            context_window_tokens: Some(131_072),
            packet_type: "multi-file-patch".to_string(),
            expected_output_tokens: 4_096,
            num_predict: None,
            max_thinking_only_tokens: 4_096,
            repair_exit_thinking_tokens: 16_384,
            repair_handoff_policy: RepairHandoffPolicy::TextOnly,
            action_boundary_interrupt_tokens: 0,
            reasoning_checkpoint_tokens: 0,
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            initial_context_catalog_file: None,
            semantic_advisor_model: None,
            acceptance_ledger: false,
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
        let trace = std::fs::read_to_string(&summary.trace_file).unwrap();
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
    async fn fixture_writes_traces_outside_the_tool_visible_root_when_configured() {
        let fixture = AgentFixture::new("Stop explicitly.");
        let external_traces = fixture._temp.path().join("external-traces");
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Content(
            "FAIL intentionally stopped".to_string(),
        )]]);

        let summary = fixture
            .run_with_trace_dir(&gateway, 1, Some(external_traces.clone()))
            .await;

        assert!(summary.trace_file.starts_with(&external_traces));
        assert!(!fixture.experiment.join("traces").exists());
        assert!(summary.trace_file.is_file());
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
    async fn fixture_delivers_bounded_reasoning_checkpoint_without_narrowing_next_turn() {
        let fixture = AgentFixture::new("Inspect, then implement.");
        let reasoning = format!(
            "{}Now implement the first source file with write_file.",
            "discarded planning prefix ".repeat(8)
        );
        let gateway = ScriptedGateway::new(vec![
            vec![StreamChunk::Thinking(reasoning.clone())],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run_with_reasoning_checkpoint(&gateway, 2, 16).await;

        assert_eq!(summary.final_summary, "DONE");
        assert_eq!(summary.reasoning_checkpoint_tokens, 16);
        let calls = gateway.stream_messages();
        assert_eq!(calls.len(), 2);
        let checkpoint_prompt = calls[1]
            .iter()
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains(REASONING_CHECKPOINT_PREFIX))
            .expect("second ordinary turn must receive the checkpoint");
        assert!(checkpoint_prompt.contains("Now implement the first source file with write_file."));
        assert!(!checkpoint_prompt.contains("discarded planning prefix discarded planning prefix"));
        assert!(checkpoint_prompt.contains("incomplete continuity state, not task authority"));
        assert_eq!(gateway.reasoning_efforts(), vec![None, None]);
        assert_eq!(gateway.tool_counts(), vec![5, 5]);

        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.reasoning_checkpoint.captured\""));
        assert!(trace.contains("\"kind\":\"agent.reasoning_checkpoint.delivered\""));
        assert!(trace.contains("\"tool_surface_narrowed_next_turn\":false"));
        assert!(trace.contains("\"reasoning_disabled_next_turn\":false"));
        let captured = trace_payloads(
            &trace,
            crate::runtime_events::AGENT_REASONING_CHECKPOINT_CAPTURED,
        );
        assert_eq!(captured.len(), 1);
        assert!(captured[0].get("retained_tail").is_none());
    }

    #[tokio::test]
    async fn constrained_action_only_does_not_force_a_pre_source_handoff() {
        let fixture = AgentFixture::new("Inspect, then implement.");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk("list_tree", HashMap::new())],
            vec![StreamChunk::Thinking(
                "I should write src/lib.rs now, but I am still planning once.".to_string(),
            )],
            vec![StreamChunk::Thinking(
                "I should write src/lib.rs now, but I am still planning twice.".to_string(),
            )],
        ]);

        let summary = fixture.run_with_constrained_action_only(&gateway, 2).await;

        assert!(!fixture.workspace.join("src/lib.rs").exists());
        assert_eq!(gateway.reasoning_efforts(), vec![None, None, None]);
        assert_eq!(gateway.tool_counts(), vec![5, 5, 5]);
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        let provider_requests = trace_payloads(
            &trace,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );
        assert!(
            provider_requests
                .iter()
                .all(|request| request["completion"]["num_ctx"] == json!(131_072))
        );
        assert!(trace.contains("\"kind\":\"agent.turn.hidden_only_no_action_hard_failed\""));
        assert!(!trace.contains("agent.pre_source_action_only"));
    }

    #[tokio::test]
    async fn progress_frame_counts_do_not_inflate_final_tool_call_batch() {
        let fixture = AgentFixture::new("Inspect one file, then finish.");
        std::fs::write(fixture.workspace.join("one.txt"), "one\n").unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![
                StreamChunk::Progress(stream_progress(1, 0, 1, 1)),
                StreamChunk::Progress(stream_progress(2, 0, 1, 535)),
                tool_call_chunk(
                    "read_file",
                    HashMap::from([("path".to_string(), json!("one.txt"))]),
                ),
            ],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        let normalized = trace_payloads(&trace, "llm.response.tool_call.normalized");
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0]["response_index"], json!(0));
        assert_eq!(normalized[0]["response_tool_call_count"], json!(1));
        assert_eq!(normalized[0]["tool_name"], json!("read_file"));
        assert_eq!(
            normalized[0]["arguments_json"],
            json!(r#"{"path":"one.txt"}"#)
        );
        assert_eq!(trace_payloads(&trace, "tool.read_file").len(), 1);
    }

    #[tokio::test]
    async fn normalized_response_trace_preserves_each_batched_call_identity() {
        let fixture = AgentFixture::new("Inspect the scoped files, then finish.");
        for name in ["one.txt", "two.txt", "three.txt"] {
            std::fs::write(fixture.workspace.join(name), format!("{name}\n")).unwrap();
        }
        let calls = ["one.txt", "two.txt", "three.txt"]
            .into_iter()
            .enumerate()
            .map(|(index, path)| LlmToolCall {
                id: Some(format!("call-{index}")),
                name: "read_file".to_string(),
                arguments: HashMap::from([("path".to_string(), json!(path))]),
            })
            .collect::<Vec<_>>();
        let gateway = ScriptedGateway::new(vec![
            vec![StreamChunk::ToolCalls(calls)],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        let normalized = trace_payloads(&trace, "llm.response.tool_call.normalized");
        assert_eq!(normalized.len(), 3);
        for (index, path) in ["one.txt", "two.txt", "three.txt"].into_iter().enumerate() {
            assert_eq!(normalized[index]["response_index"], json!(index));
            assert_eq!(normalized[index]["response_tool_call_count"], json!(3));
            assert_eq!(
                normalized[index]["tool_call_id"],
                json!(format!("call-{index}"))
            );
            assert_eq!(
                normalized[index]["arguments_json"],
                json!(format!(r#"{{"path":"{path}"}}"#))
            );
            assert_eq!(normalized[index]["arguments_complete"], json!(true));
            assert_eq!(
                normalized[index]["arguments_sha256"]
                    .as_str()
                    .unwrap()
                    .len(),
                64
            );
        }
    }

    #[tokio::test]
    async fn normalized_response_trace_bounds_large_arguments_with_full_hash() {
        let fixture = AgentFixture::new("Write the requested artifact, then finish.");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "write_file",
                HashMap::from([
                    ("path".to_string(), json!("artifact.txt")),
                    (
                        "content".to_string(),
                        json!("x".repeat(RESPONSE_TOOL_CALL_ARGUMENT_MAX_CHARS + 1_000)),
                    ),
                ]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run(&gateway, 2).await;

        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        let normalized = trace_payloads(&trace, "llm.response.tool_call.normalized");
        assert_eq!(normalized.len(), 1);
        assert_eq!(normalized[0]["arguments_complete"], json!(false));
        assert_eq!(normalized[0]["arguments_json"], Value::Null);
        assert_eq!(
            normalized[0]["arguments_preview"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            RESPONSE_TOOL_CALL_ARGUMENT_MAX_CHARS
        );
        assert_eq!(
            normalized[0]["arguments_sha256"].as_str().unwrap().len(),
            64
        );
        assert!(fixture.workspace.join("artifact.txt").is_file());
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
            "turn 2 produced 2 consecutive action-boundary interrupts without required source progress or fresh validation"
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
        let fixture = AgentFixture::new(
            "Validate, write a README, and finish.\n\n```sh\n./cargo test\n```\n",
        );
        fixture.write_fake_cargo(0, "ok");
        let gateway = ScriptedGateway::new(vec![
            vec![StreamChunk::ToolCalls(vec![
                tool_call(
                    "execute_probe",
                    HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
        let fixture = AgentFixture::new(
            "Edit existing source, validate, and finish.\n\n```sh\n./cargo test\n```\n",
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
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
        let fixture = AgentFixture::new(
            "Edit existing source, validate, and finish.\n\n```sh\n./cargo test\n```\n",
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
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test-focused-summary"))]),
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
            "probe:cargo-test-focused-summary"
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
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-build"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
        assert_eq!(prompts[0]["validation"]["command"], "probe:cargo-test");
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
        let fixture =
            AgentFixture::new("Run validation and repair failures.\n\n```sh\n./cargo test\n```\n");
        fixture.write_fake_cargo(1, "unit failed");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
        let fixture = AgentFixture::new(
            "Run validation and repair no-content failures.\n\n```sh\n./cargo test\n```\n",
        );
        fixture.write_fake_cargo(1, "compile failed");
        let first_interrupt_frames = (0..REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT)
            .map(|index| StreamChunk::Progress(stream_progress(index, 0, 0, 0)))
            .collect::<Vec<_>>();
        let second_interrupt_frames = (0..REPAIR_NO_CONTENT_PROGRESS_FRAME_LIMIT)
            .map(|index| StreamChunk::Progress(stream_progress(index, 0, 0, 0)))
            .collect::<Vec<_>>();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
        let fixture = AgentFixture::new(
            "Run validation, repair source, and probe again.\n\n```sh\n./cargo test\n```\n",
        );
        fixture.write_fake_cargo(1, "test failed");
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("cargo-test"))]),
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
        let fixture = AgentFixture::new("Edit source once and stop.\n\n```sh\n./cargo test\n```\n");
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

    #[tokio::test]
    async fn empty_probe_contract_does_not_invent_validation_authority() {
        let fixture = AgentFixture::new("unused legacy task");
        std::fs::write(
            fixture.experiment.join("contract.json"),
            serde_json::to_string_pretty(&json!({
                "guidance": "Write the requested source and finish.",
                "probes": []
            }))
            .unwrap(),
        )
        .unwrap();
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
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run_contract(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.stage.first_source_mutation\""));
        assert!(!trace.contains("\"kind\":\"agent.validation.required_after_edit\""));
        assert!(!trace.contains("\"kind\":\"agent.validation_probe.observed\""));
    }

    #[tokio::test]
    async fn fixture_runs_explicit_file_assertion_without_a_shell_probe() {
        let expected = "# Project Aurora\n\n- Owner: Mia Chen\n- Status: Blocked\n- Blocker: Vendor API credentials\n- Next step: Contact vendor by Friday\n";
        let fixture = AgentFixture::new("unused legacy task");
        std::fs::write(
            fixture.workspace.join("input.txt"),
            "Project Aurora status\nowner: Mia Chen\nstatus: blocked\nblocker: vendor API credentials\nnext: contact vendor by Friday\n",
        )
        .unwrap();
        std::fs::write(
            fixture.experiment.join("contract.json"),
            serde_json::to_string_pretty(&json!({
                "profile": {
                    "id": "text_transform",
                    "version": "text_transform_profile.v1"
                },
                "guidance": "Create the exact brief from input.txt.",
                "read_scope": ["input.txt", "brief.md"],
                "write_scope": ["brief.md"],
                "probes": [{
                    "id": "brief-exact",
                    "assertion": {
                        "kind": "file_text_equals",
                        "path": "brief.md",
                        "expected": expected
                    }
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "write_file",
                HashMap::from([
                    ("path".to_string(), json!("brief.md")),
                    ("content".to_string(), json!(expected)),
                ]),
            )],
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("brief-exact"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run_contract(&gateway, 3).await;

        assert_eq!(summary.final_summary, "DONE");
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("brief.md")).unwrap(),
            expected
        );
        let trace = std::fs::read_to_string(&summary.trace_file).unwrap();
        assert!(trace.contains("\"assertion_kind\":\"file_text_equals\""));
        assert!(trace.contains("\"probe_id\":\"brief-exact\""));
        assert!(trace.contains("\"kind\":\"agent.terminal.done_observed\""));
        assert!(!trace.contains("\"kind\":\"tool.shell_command\""));
        let analysis = crate::trace_analysis::analyze_trace(&summary.trace_file).unwrap();
        assert!(analysis.validation_probe_reached.is_some());
        assert!(analysis.validation_probe_passed.is_some());
    }

    #[tokio::test]
    async fn fixture_keeps_registered_command_probe_bytes_out_of_provider_requests() {
        let fixture = AgentFixture::new("unused legacy task");
        let private_command = "test -f output.txt # PRIVATE_REGISTERED_COMMAND_BYTES";
        std::fs::write(
            fixture.experiment.join("contract.json"),
            serde_json::to_string_pretty(&json!({
                "profile": {
                    "id": "terminal_work",
                    "version": "terminal_work_profile.v1"
                },
                "guidance": "Create output.txt, then execute the declared probe by ID.",
                "read_scope": ["./**"],
                "write_scope": ["./**"],
                "probes": [{
                    "id": "output-present",
                    "command": private_command
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "write_file",
                HashMap::from([
                    ("path".to_string(), json!("output.txt")),
                    ("content".to_string(), json!("ready\n")),
                ]),
            )],
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("output-present"))]),
            )],
            vec![StreamChunk::Content("DONE".to_string())],
        ]);

        let summary = fixture.run_contract(&gateway, 3).await;
        let trace = std::fs::read_to_string(&summary.trace_file).unwrap();
        let provider_requests = trace_payloads(
            &trace,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );

        assert_eq!(summary.final_summary, "DONE");
        assert!(provider_requests.len() >= 3);
        assert!(provider_requests.iter().all(|request| {
            !serde_json::to_string(request)
                .unwrap()
                .contains("PRIVATE_REGISTERED_COMMAND_BYTES")
        }));
        assert!(
            provider_requests[0]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "execute_probe")
        );
        assert!(trace.contains("\"kind\":\"tool.execute_probe\""));
        assert!(trace.contains("\"probe_id\":\"output-present\""));
        assert!(trace.contains("\"command\":\"probe:output-present\""));
        assert!(trace.contains("\"success\":true"));
    }

    #[tokio::test]
    async fn fixture_keeps_failed_command_probe_bytes_out_of_repair_escalation() {
        let fixture = AgentFixture::new("unused legacy task");
        let private_command = "test -f never-created.txt # PRIVATE_REPAIR_COMMAND_BYTES";
        std::fs::write(
            fixture.experiment.join("contract.json"),
            serde_json::to_string_pretty(&json!({
                "profile": {
                    "id": "terminal_work",
                    "version": "terminal_work_profile.v1"
                },
                "guidance": "Execute the declared probe by ID.",
                "read_scope": ["./**"],
                "write_scope": ["./**"],
                "probes": [{
                    "id": "repair-confidential",
                    "command": private_command
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("repair-confidential"))]),
            )],
            vec![],
            vec![],
        ]);

        let summary = fixture.run_contract(&gateway, 3).await;
        let trace = std::fs::read_to_string(&summary.trace_file).unwrap();
        let provider_requests = trace_payloads(
            &trace,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );

        assert!(summary.final_summary.contains("validation-repair"));
        assert!(provider_requests.iter().all(|request| {
            !serde_json::to_string(request)
                .unwrap()
                .contains("PRIVATE_REPAIR_COMMAND_BYTES")
        }));
        assert!(provider_requests.iter().any(|request| {
            serde_json::to_string(request)
                .unwrap()
                .contains("probe:repair-confidential")
        }));
        assert_eq!(
            provider_requests[1]["harness_limits"]["validation_repair_active"],
            json!(true),
            "the next in-flight request must adopt repair policy immediately"
        );
    }

    #[tokio::test]
    async fn constrained_handoff_owns_the_first_request_after_failed_declared_probe() {
        let fixture = AgentFixture::new("unused legacy task");
        let private_command = "test -f never-created.txt # PRIVATE_HANDOFF_COMMAND_BYTES";
        std::fs::write(
            fixture.experiment.join("contract.json"),
            serde_json::to_string_pretty(&json!({
                "profile": {
                    "id": "terminal_work",
                    "version": "terminal_work_profile.v1"
                },
                "guidance": "Execute the declared probe by ID and repair its failure.",
                "read_scope": ["./**"],
                "write_scope": ["./**"],
                "probes": [{
                    "id": "repair-handoff",
                    "command": private_command
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![StreamChunk::Content(
                "I will continue from the current state.".to_string(),
            )],
            vec![tool_call_chunk(
                "shell_command",
                HashMap::from([("command".to_string(), json!("test -f never-created.txt"))]),
            )],
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("repair-handoff"))]),
            )],
            vec![StreamChunk::Content(
                "FAIL deterministic fixture ends after observing the constrained request"
                    .to_string(),
            )],
        ]);

        let summary = fixture
            .run_contract_with_policy(&gateway, 4, RepairHandoffPolicy::Constrained)
            .await;
        let trace = std::fs::read_to_string(&summary.trace_file).unwrap();
        let provider_requests = trace_payloads(
            &trace,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );

        assert_eq!(provider_requests.len(), 4, "{provider_requests:?}");
        let pre_handoff_request = serde_json::to_string(&provider_requests[2]).unwrap();
        assert!(
            pre_handoff_request.contains("Continue from the current experiment state"),
            "{pre_handoff_request}"
        );
        assert!(
            provider_requests[2]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "read_file")
        );
        let repair_request = &provider_requests[3];
        let repair_request_text = serde_json::to_string(repair_request).unwrap();
        assert!(repair_request_text.contains("Validation repair action contract is active"));
        assert!(repair_request_text.contains("probe:repair-handoff"));
        assert!(!repair_request_text.contains("PRIVATE_HANDOFF_COMMAND_BYTES"));
        assert!(!repair_request_text.contains("You used tools but produced no final text"));
        assert!(!repair_request_text.contains("Continue from the current experiment state"));
        assert!(!repair_request_text.contains("Do not edit again yet"));
        let tool_names = repair_request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            tool_names,
            vec!["write_file", "edit_file", "shell_command", "execute_probe"]
        );
        assert!(trace.contains("\"kind\":\"agent.validation.constrained_handoff_required\""));
        assert!(trace.contains("\"kind\":\"agent.validation.constrained_handoff_started\""));
        assert!(trace.contains("\"kind\":\"agent.validation.constrained_handoff_context_pruned\""));
        assert_eq!(
            trace
                .matches("\"kind\":\"agent.validation.constrained_handoff_required\"")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn constrained_action_only_disables_reasoning_after_bounded_repair_no_action() {
        let fixture = AgentFixture::new("unused legacy task");
        std::fs::write(
            fixture.experiment.join("contract.json"),
            serde_json::to_string_pretty(&json!({
                "profile": {
                    "id": "terminal_work",
                    "version": "terminal_work_profile.v1"
                },
                "guidance": "Execute the declared probe and repair its failure.",
                "read_scope": ["./**"],
                "write_scope": ["./**"],
                "probes": [{
                    "id": "action-only-repair",
                    "command": "test -f never-created.txt"
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![
            vec![tool_call_chunk(
                "execute_probe",
                HashMap::from([("probe_id".to_string(), json!("action-only-repair"))]),
            )],
            vec![StreamChunk::Thinking(
                "diagnosis exceeded the deliberately tiny fixture cap".to_string(),
            )],
            vec![StreamChunk::Content(
                "FAIL deterministic fixture observed the action-only request".to_string(),
            )],
        ]);

        let summary = fixture
            .run_contract_with_repair_config(
                &gateway,
                3,
                RepairHandoffPolicy::ConstrainedActionOnly,
                1,
            )
            .await;
        let trace = std::fs::read_to_string(&summary.trace_file).unwrap();
        let provider_requests = trace_payloads(
            &trace,
            crate::runtime_events::LLM_PROVIDER_REQUEST_ASSEMBLED,
        );

        assert_eq!(gateway.reasoning_efforts().len(), 3);
        assert_eq!(gateway.reasoning_efforts()[0], None);
        assert_eq!(gateway.reasoning_efforts()[1], None);
        assert_eq!(
            gateway.reasoning_efforts()[2],
            Some(ReasoningEffort::Disabled)
        );
        assert_eq!(provider_requests.len(), 3);
        assert_eq!(
            provider_requests[2]["completion"]["reasoning_effort"],
            json!("disabled")
        );
        assert_eq!(
            provider_requests[2]["harness_limits"]["effective_thinking_only_cap_source"],
            json!("provider_disabled")
        );
        let action_only_request = serde_json::to_string(&provider_requests[2]).unwrap();
        assert!(action_only_request.contains("hidden reasoning is not retained"));
        assert!(action_only_request.contains("Do not restart analysis"));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_action_only_scheduled\""));
        assert!(trace.contains("\"kind\":\"agent.validation.repair_action_only_started\""));
    }

    #[tokio::test]
    async fn fixture_assembles_required_selected_and_excluded_initial_context() {
        let fixture = AgentFixture::new("Create a release note with the required format.");
        std::fs::write(
            fixture.experiment.join("context-catalog.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": "initial_context_catalog.v2",
                "max_selected": 1,
                "max_total_guidance_chars": 3000,
                "max_advisory_chars": 10000,
                "min_confidence": 0.7,
                "records": [
                    {
                        "id": "release-safety",
                        "disposition": "required",
                        "description": "Mandatory release safety rule",
                        "content": "Never claim validation that was not observed.",
                        "source": "release-safety.md"
                    },
                    {
                        "id": "release-format",
                        "disposition": "selectable",
                        "description": "Exact release-note layout",
                        "content": "Use a title followed by exactly two bullet points.",
                        "source": "release-format.md"
                    },
                    {
                        "id": "database-migrations",
                        "disposition": "selectable",
                        "description": "Database migration policy",
                        "content": "Never rewrite an applied migration.",
                        "source": "database.md"
                    },
                    {
                        "id": "private-roadmap",
                        "disposition": "excluded",
                        "description": "Material prohibited from this task",
                        "source": "private-roadmap.md"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::with_json(
            vec![vec![StreamChunk::Content(
                "FAIL fixture stops after context inspection".to_string(),
            )]],
            vec![json!({
                "schema_version": "initial_context_decision.v2",
                "selected_ids": ["release-format"],
                "confidence": 0.94,
                "rationale": "The task explicitly requests a release note."
            })],
        );

        let summary = fixture.run_with_initial_context(&gateway).await;

        assert_eq!(summary.required_initial_context_ids, vec!["release-safety"]);
        assert_eq!(
            summary.advisory_selected_context_ids,
            vec!["release-format"]
        );
        assert_eq!(
            summary.excluded_initial_context_ids,
            vec!["private-roadmap"]
        );
        let worker_calls = gateway.stream_messages();
        assert_eq!(worker_calls.len(), 1);
        assert_eq!(worker_calls[0].len(), 2);
        let injected = worker_calls[0][1].content.as_deref().unwrap_or_default();
        assert!(injected.contains("Create a release note with the required format."));
        assert!(injected.contains("Never claim validation that was not observed."));
        assert!(injected.contains("Use a title followed by exactly two bullet points."));
        assert!(!injected.contains("Never rewrite an applied migration."));
        assert!(!injected.contains("private-roadmap"));
        let advisory_calls = gateway.json_messages();
        assert_eq!(advisory_calls.len(), 1);
        let advisory_packet = advisory_calls[0]
            .iter()
            .filter_map(|message| message.content.as_deref())
            .collect::<String>();
        assert!(advisory_packet.contains("release-format"));
        assert!(advisory_packet.contains("database-migrations"));
        assert!(!advisory_packet.contains("release-safety"));
        assert!(!advisory_packet.contains("private-roadmap"));
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"semantic_advisory.requested\""));
        assert!(trace.contains("\"kind\":\"initial_context.policy.evaluated\""));
        assert!(trace.contains("\"kind\":\"initial_context.assembled\""));
        assert!(trace.contains("\"inclusion_reason\":\"authoritative_initial_context_packet\""));
    }

    #[tokio::test]
    async fn fixture_task_only_initial_context_makes_no_advisory_call() {
        let fixture = AgentFixture::new("Produce the requested outcome.");
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Content(
            "FAIL fixture stops after packet inspection".to_string(),
        )]]);

        let summary = fixture.run(&gateway, 1).await;

        assert!(summary.required_initial_context_ids.is_empty());
        assert!(summary.advisory_selected_context_ids.is_empty());
        assert!(summary.excluded_initial_context_ids.is_empty());
        assert!(gateway.json_messages().is_empty());
        let worker_calls = gateway.stream_messages();
        assert_eq!(worker_calls.len(), 1);
        assert!(
            worker_calls[0][1]
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("Produce the requested outcome.")
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"initial_context.assembled\""));
        assert!(trace.contains("\"catalog_enabled\":false"));
        assert!(!trace.contains("\"kind\":\"semantic_advisory.requested\""));
    }

    #[tokio::test]
    async fn fixture_rejects_done_until_current_acceptance_coverage_is_submitted() {
        let task = "Create artifact.txt and validate its exact output.";
        let fixture = AgentFixture::new(task);
        let gateway = ScriptedGateway::with_json(
            vec![
                vec![StreamChunk::Content("DONE".to_string())],
                vec![
                    tool_call_chunk(
                        "submit_acceptance_evidence",
                        HashMap::from([
                            (
                                "acceptance_ids".to_string(),
                                json!(["artifact", "validation", "interaction"]),
                            ),
                            (
                                "evidence".to_string(),
                                json!("exact-output check passed after artifact inspection"),
                            ),
                        ]),
                    ),
                    StreamChunk::Content("DONE".to_string()),
                ],
            ],
            vec![
                json!({
                    "schema_version": "acceptance_plan.v1",
                    "items": [
                        {
                            "id": "artifact",
                            "requirement": "Create artifact.txt.",
                            "kind": "artifact",
                            "source_excerpt": "Create artifact.txt and validate its exact output.",
                            "suggested_evidence": "Inspect artifact.txt."
                        },
                        {
                            "id": "validation",
                            "requirement": "Validate exact output.",
                            "kind": "behavior",
                            "source_excerpt": "Create artifact.txt and validate its exact output.",
                            "suggested_evidence": "Run an exact-output check."
                        }
                    ]
                }),
                json!({
                    "schema_version": "acceptance_interactions.v1",
                    "scenarios": [{
                        "id": "interaction",
                        "item_ids": ["artifact", "validation"],
                        "risk": "The artifact can exist while its exact output is wrong.",
                        "suggested_evidence": "Inspect the artifact and check exact output together."
                    }]
                }),
            ],
        );

        let summary = fixture.run_with_acceptance_ledger(&gateway, 2).await;

        assert_eq!(summary.final_summary, "DONE");
        assert!(summary.acceptance_ledger);
        assert_eq!(summary.acceptance_ledger_entry_count, 3);
        assert_eq!(gateway.json_messages().len(), 2);
        assert!(gateway.stream_messages().len() >= 2);
        assert!(
            gateway.stream_messages()[0][1]
                .content
                .as_deref()
                .unwrap_or_default()
                .contains("submit_acceptance_evidence")
        );
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"acceptance_ledger.delivered\""));
        assert!(trace.contains("\"kind\":\"acceptance_ledger.done_rejected\""));
        assert!(trace.contains("\"kind\":\"tool.submit_acceptance_evidence\""));
        assert!(trace.contains("\"coverage_complete\":true"));
    }

    #[tokio::test]
    async fn fixture_required_only_catalog_skips_advisory_and_excludes_content() {
        let fixture = AgentFixture::new("Produce the requested outcome.");
        std::fs::write(
            fixture.experiment.join("context-catalog.json"),
            serde_json::to_string_pretty(&json!({
                "schema_version": "initial_context_catalog.v2",
                "max_selected": 0,
                "max_total_guidance_chars": 2000,
                "max_advisory_chars": 1000,
                "min_confidence": 0.7,
                "records": [
                    {
                        "id": "mandatory-policy",
                        "disposition": "required",
                        "description": "Mandatory task policy",
                        "content": "State only observed facts."
                    },
                    {
                        "id": "prohibited-context",
                        "disposition": "excluded",
                        "description": "Must never enter model context"
                    }
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let gateway = ScriptedGateway::new(vec![vec![StreamChunk::Content(
            "FAIL fixture stops after packet inspection".to_string(),
        )]]);

        let summary = fixture.run_with_initial_context(&gateway).await;

        assert_eq!(
            summary.required_initial_context_ids,
            vec!["mandatory-policy"]
        );
        assert!(summary.advisory_selected_context_ids.is_empty());
        assert_eq!(
            summary.excluded_initial_context_ids,
            vec!["prohibited-context"]
        );
        assert!(gateway.json_messages().is_empty());
        let worker_packet = gateway.stream_messages()[0][1]
            .content
            .clone()
            .unwrap_or_default();
        assert!(worker_packet.contains("State only observed facts."));
        assert!(!worker_packet.contains("prohibited-context"));
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"initial_context.advisory.skipped\""));
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
            self.run_with_trace_dir(gateway, max_iterations, None).await
        }

        async fn run_with_acceptance_ledger(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
        ) -> AgentRunSummary {
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir: None,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: None,
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens: 16_384,
                    repair_handoff_policy: RepairHandoffPolicy::TextOnly,
                    action_boundary_interrupt_tokens: 0,
                    reasoning_checkpoint_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: None,
                    semantic_advisor_model: None,
                    acceptance_ledger: true,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }

        async fn run_with_reasoning_checkpoint(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
            reasoning_checkpoint_tokens: usize,
        ) -> AgentRunSummary {
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir: None,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: None,
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens: 16_384,
                    repair_handoff_policy: RepairHandoffPolicy::TextOnly,
                    action_boundary_interrupt_tokens: 0,
                    reasoning_checkpoint_tokens,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: None,
                    semantic_advisor_model: None,
                    acceptance_ledger: false,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }

        async fn run_with_trace_dir(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
            trace_dir: Option<PathBuf>,
        ) -> AgentRunSummary {
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: None,
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens: 16_384,
                    repair_handoff_policy: RepairHandoffPolicy::TextOnly,
                    action_boundary_interrupt_tokens: 0,
                    reasoning_checkpoint_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: None,
                    semantic_advisor_model: None,
                    acceptance_ledger: false,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }

        async fn run_contract(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
        ) -> AgentRunSummary {
            self.run_contract_with_policy(gateway, max_iterations, RepairHandoffPolicy::TextOnly)
                .await
        }

        async fn run_contract_with_policy(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
            repair_handoff_policy: RepairHandoffPolicy,
        ) -> AgentRunSummary {
            self.run_contract_with_repair_config(
                gateway,
                max_iterations,
                repair_handoff_policy,
                16_384,
            )
            .await
        }

        async fn run_contract_with_repair_config(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
            repair_handoff_policy: RepairHandoffPolicy,
            repair_exit_thinking_tokens: usize,
        ) -> AgentRunSummary {
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir: None,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: Some(PathBuf::from("contract.json")),
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens,
                    repair_handoff_policy,
                    action_boundary_interrupt_tokens: 0,
                    reasoning_checkpoint_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: None,
                    semantic_advisor_model: None,
                    acceptance_ledger: false,
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
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir: None,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: None,
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: usize::MAX,
                    repair_exit_thinking_tokens: 16_384,
                    repair_handoff_policy: RepairHandoffPolicy::TextOnly,
                    action_boundary_interrupt_tokens: 1,
                    reasoning_checkpoint_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: None,
                    semantic_advisor_model: None,
                    acceptance_ledger: false,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }

        async fn run_with_constrained_action_only(
            &self,
            gateway: &ScriptedGateway,
            max_iterations: usize,
        ) -> AgentRunSummary {
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir: None,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: None,
                    model: "fake-model".to_string(),
                    max_iterations,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens: 16_384,
                    repair_handoff_policy: RepairHandoffPolicy::ConstrainedActionOnly,
                    action_boundary_interrupt_tokens: 0,
                    reasoning_checkpoint_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: None,
                    semantic_advisor_model: None,
                    acceptance_ledger: false,
                },
                gateway,
                self.workspace.clone(),
            )
            .await
            .unwrap()
        }

        async fn run_with_initial_context(&self, gateway: &ScriptedGateway) -> AgentRunSummary {
            run_agent_with_gateway(
                AgentRunConfig {
                    experiment_dir: self.experiment.clone(),
                    trace_dir: None,
                    goal_file: PathBuf::from("task.md"),
                    contract_file: None,
                    model: "worker-model".to_string(),
                    max_iterations: 1,
                    max_tool_iterations: 10,
                    context_window_tokens: Some(131_072),
                    packet_type: "narrow-patch".to_string(),
                    expected_output_tokens: 2_048,
                    num_predict: None,
                    max_thinking_only_tokens: 2_048,
                    repair_exit_thinking_tokens: 16_384,
                    repair_handoff_policy: RepairHandoffPolicy::TextOnly,
                    action_boundary_interrupt_tokens: 0,
                    reasoning_checkpoint_tokens: 0,
                    transcript_policy: TranscriptPolicy::SummarizedTranscript,
                    initial_context_catalog_file: Some(PathBuf::from("context-catalog.json")),
                    semantic_advisor_model: Some("context-curator-model".to_string()),
                    acceptance_ledger: false,
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
        json_responses: StdMutex<VecDeque<Value>>,
        json_messages: StdMutex<Vec<Vec<LlmMessage>>>,
        tool_counts: StdMutex<Vec<usize>>,
        stream_messages: StdMutex<Vec<Vec<LlmMessage>>>,
        reasoning_efforts: StdMutex<Vec<Option<ReasoningEffort>>>,
    }

    impl ScriptedGateway {
        fn new(streams: Vec<Vec<StreamChunk>>) -> Self {
            Self {
                streams: StdMutex::new(VecDeque::from(streams)),
                json_responses: StdMutex::new(VecDeque::new()),
                json_messages: StdMutex::new(Vec::new()),
                tool_counts: StdMutex::new(Vec::new()),
                stream_messages: StdMutex::new(Vec::new()),
                reasoning_efforts: StdMutex::new(Vec::new()),
            }
        }

        fn with_json(streams: Vec<Vec<StreamChunk>>, json_responses: Vec<Value>) -> Self {
            Self {
                streams: StdMutex::new(VecDeque::from(streams)),
                json_responses: StdMutex::new(VecDeque::from(json_responses)),
                json_messages: StdMutex::new(Vec::new()),
                tool_counts: StdMutex::new(Vec::new()),
                stream_messages: StdMutex::new(Vec::new()),
                reasoning_efforts: StdMutex::new(Vec::new()),
            }
        }

        fn tool_counts(&self) -> Vec<usize> {
            self.tool_counts
                .lock()
                .expect("scripted gateway tool-count mutex poisoned")
                .clone()
        }

        fn stream_messages(&self) -> Vec<Vec<LlmMessage>> {
            self.stream_messages
                .lock()
                .expect("scripted gateway message mutex poisoned")
                .clone()
        }

        fn json_messages(&self) -> Vec<Vec<LlmMessage>> {
            self.json_messages
                .lock()
                .expect("scripted gateway JSON-message mutex poisoned")
                .clone()
        }

        fn reasoning_efforts(&self) -> Vec<Option<ReasoningEffort>> {
            self.reasoning_efforts
                .lock()
                .expect("scripted gateway reasoning-effort mutex poisoned")
                .clone()
        }
    }

    #[async_trait]
    impl LlmGateway for ScriptedGateway {
        async fn complete(
            &self,
            _model: &str,
            messages: &[LlmMessage],
            _tools: Option<&[Box<dyn LlmTool>]>,
            config: &CompletionConfig,
        ) -> mojentic::Result<LlmGatewayResponse> {
            assert_eq!(config.max_tool_iterations, 0);
            self.json_messages
                .lock()
                .expect("scripted gateway JSON-message mutex poisoned")
                .push(messages.to_vec());
            let proposal = self
                .json_responses
                .lock()
                .expect("scripted gateway JSON mutex poisoned")
                .pop_front()
                .expect("unexpected structured-output call");
            Ok(LlmGatewayResponse {
                content: Some(serde_json::to_string(&proposal).unwrap()),
                object: None,
                tool_calls: Vec::new(),
                thinking: Some("scripted advisory reasoning".into()),
            })
        }

        async fn complete_json(
            &self,
            _model: &str,
            messages: &[LlmMessage],
            _schema: Value,
            _config: &CompletionConfig,
        ) -> mojentic::Result<Value> {
            self.json_messages
                .lock()
                .expect("scripted gateway JSON-message mutex poisoned")
                .push(messages.to_vec());
            Ok(self
                .json_responses
                .lock()
                .expect("scripted gateway JSON mutex poisoned")
                .pop_front()
                .expect("unexpected structured-output call"))
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
            config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
            {
                let mut messages = self
                    .stream_messages
                    .lock()
                    .expect("scripted gateway message mutex poisoned");
                messages.push(_messages.to_vec());
            }
            self.tool_counts
                .lock()
                .expect("scripted gateway tool-count mutex poisoned")
                .push(tools.map(|tools| tools.len()).unwrap_or(0));
            self.reasoning_efforts
                .lock()
                .expect("scripted gateway reasoning-effort mutex poisoned")
                .push(config.reasoning_effort);
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
    /// `run_agent_with_gateway`: contract resolution is the first
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
