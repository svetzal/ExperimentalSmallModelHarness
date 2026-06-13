use crate::tools::{ToolPolicySnapshot, ToolScope, ValidationRepairSnapshot, coding_tools};
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
const TOOL_RESULT_SUMMARY_PREFIX: &str = "[harness-retained-tool-result-summary]";
const MAX_REPAIR_NO_ACTION_TURNS: usize = 2;
const EMPTY_RESPONSE_ESCALATION_TURNS: usize = 3;
const MAX_PRE_VALIDATION_REPEATED_INSPECTIONS: usize = 4;
const NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER: usize = 20;

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
    pub transcript_policy: TranscriptPolicy,
    pub final_summary: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TranscriptPolicy {
    FullTranscript,
    #[default]
    SummarizedTranscript,
    ValidationRepairPacket,
}

impl TranscriptPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full-transcript" | "full" => Some(Self::FullTranscript),
            "summarized-transcript" | "summarized" | "summary" => Some(Self::SummarizedTranscript),
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
            Self::ValidationRepairPacket => ToolResultCompaction {
                enabled: true,
                raw_recent_count: 2,
                max_raw_tool_result_chars: 3_000,
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
        "multi-file-patch" => 4_096,
        "full-small-project" => 8_192,
        "validation-repair" => 2_048,
        _ => 4_096,
    }
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
    let tool_root = tool_root
        .canonicalize()
        .with_context(|| format!("canonicalizing tool root {}", tool_root.display()))?;

    let trace = Arc::new(TraceRecorder::create(&experiment_dir.join("traces"))?);
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
            "assembly_policy": config.transcript_policy.as_str(),
            "transcript_policy": config.transcript_policy,
            "context_instrumentation_version": CONTEXT_INSTRUMENTATION_VERSION,
            "harness_package_version": env!("CARGO_PKG_VERSION"),
            "harness_source_state": harness_source_state(),
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
    let scope = ToolScope::new(tool_root.clone(), Arc::clone(&trace))?;
    let system_prompt = system_prompt();
    let tools = coding_tools(&scope);
    let mut messages = vec![
        LlmMessage::system(system_prompt),
        LlmMessage::user(run_prompt(&goal)),
    ];
    let completion_config = CompletionConfig {
        temperature: 0.2,
        max_tool_iterations: config.max_tool_iterations,
        ..Default::default()
    };

    let mut final_summary = String::new();
    let mut consecutive_empty_responses = 0usize;
    let mut repair_no_action_tracker = RepairNoActionTracker::default();
    for turn in 1..=config.max_iterations {
        let policy_before_turn = scope.policy_snapshot();
        trace.event(
            "agent.context.estimated",
            context_snapshot(
                &messages,
                &tools,
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
            }),
        )?;
        let turn_result = match stream_response(StreamResponseRequest {
            gateway,
            model: &config.model,
            messages: &messages,
            tools: &tools,
            completion_config: completion_config.clone(),
            context_window_tokens: config.context_window_tokens,
            packet_type: &config.packet_type,
            expected_output_tokens: config.expected_output_tokens,
            transcript_policy: config.transcript_policy,
            throughput_registry_path: experiment_dir.join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn,
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
        let response = turn_result.response;
        messages = turn_result.messages;
        trace.event(
            "agent.turn.finished",
            serde_json::json!({
                "turn": turn,
                "response": response,
            }),
        )?;
        let policy = scope.policy_snapshot();
        let tool_calls_this_turn = policy.total_tool_calls - policy_before_turn.total_tool_calls;
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
            }),
        )?;
        let repair_no_action = repair_no_action_tracker.observe(
            turn,
            tool_calls_this_turn,
            &policy_before_turn,
            &policy,
        );
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
            final_summary = repair_no_action_failure_summary(turn);
            trace.event("agent.validation.repair_hard_failed", decision)?;
            break;
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
                    "You modified files after the most recent shell probe and did not provide final text. \
                     Do not edit again yet. Run the validation ladder now: cargo fmt --check, \
                     then cargo clippy, then focused tests, then broad tests. Use timeout_secs 1800 \
                     for cargo build or cargo test. Reply DONE only if validation passes.",
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
                messages.push(LlmMessage::user(
                    "You used tools but produced no final text. Continue from the current project state. \
                     If validation passed, reply exactly DONE. If validation failed, fix the cause and validate again.",
                ));
                continue;
            }
            consecutive_empty_responses += 1;
            let empty_response_decision = empty_response_decision(consecutive_empty_responses);
            trace.event(
                "agent.turn.empty_response",
                serde_json::json!({
                    "turn": turn,
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
            }
            messages.push(LlmMessage::user(empty_response_decision.prompt));
            continue;
        }
        consecutive_empty_responses = 0;
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
                "You modified files after the most recent shell probe. Do not edit again yet. \
                 Run the validation ladder now: cargo fmt --check, then cargo clippy, \
                 then focused tests, then broad tests. Use timeout_secs 1800 for cargo build \
                 or cargo test. Reply DONE only if validation passes.",
            ));
            continue;
        }
        if is_terminal_response(&final_summary) {
            break;
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
        transcript_policy: config.transcript_policy,
        final_summary,
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

fn harness_source_state() -> serde_json::Value {
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

    serde_json::json!({
        "manifest_dir": manifest_dir,
        "git_head": git_head,
        "git_dirty": git_dirty,
        "source_state_note": if git_head.is_some() {
            "git metadata captured"
        } else {
            "not a git checkout or git unavailable"
        },
    })
}

fn is_terminal_response(response: &str) -> bool {
    is_fail_response(response)
        || response
            .lines()
            .map(str::trim)
            .any(|line| line.eq_ignore_ascii_case("DONE"))
}

fn is_fail_response(response: &str) -> bool {
    response
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .is_some_and(|line| {
            line.split_once(char::is_whitespace)
                .map(|(head, _)| head.eq_ignore_ascii_case("FAIL"))
                .unwrap_or_else(|| line.eq_ignore_ascii_case("FAIL"))
        })
}

fn should_prompt_validation_repair(policy: &ToolPolicySnapshot, response: &str) -> bool {
    policy.validation_repair.is_some() && !is_terminal_response(response)
}

fn repair_no_action_failure_summary(turn: usize) -> String {
    format!("turn {turn} made no validation-repair edit or probe after validation failure")
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

fn system_prompt() -> String {
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
        "After a validation failure, repair narrowly: cite the failing command and failure text, inspect only relevant code, apply a focused patch or bounded write, then rerun validation.",
        "For Rust projects after edits, prefer this validation ladder: cargo fmt --check, cargo clippy --all-targets --all-features -- -D warnings, focused tests, then cargo test.",
        "If patch_file fails or times out for a file, retry with a smaller diff or use a bounded write_file after reading the current contents.",
        "Never end a turn with an empty response. Continue using tools, or reply DONE/FAIL as instructed.",
        "When you have completed the task and verified it, answer exactly DONE.",
        "If the task cannot be completed, answer exactly FAIL with one concise reason.",
    ]
    .join("\n")
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
    transcript_policy: TranscriptPolicy,
    throughput_registry_path: PathBuf,
    progress_projection_override: Option<ModelProgressProjection>,
    progress_status_interval_override: Option<Duration>,
    runner_activity_override: Option<RunnerActivitySample>,
    trace: &'a TraceRecorder,
    turn: usize,
}

#[derive(Debug)]
struct StreamResponseResult {
    response: String,
    messages: Vec<LlmMessage>,
}

#[derive(Debug, Default)]
struct RepairNoActionTracker {
    active_failure_key: Option<String>,
    consecutive_no_action_turns: usize,
}

#[derive(Debug, Clone, Serialize)]
struct RepairNoActionDecision {
    turn: usize,
    tool_calls_this_turn: usize,
    consecutive_no_action_turns: usize,
    escalation_required: bool,
    active_repair: ValidationRepairSnapshot,
    validation_repair_read_paths: BTreeMap<String, usize>,
    total_write_operations_before_turn: usize,
    total_write_operations_after_turn: usize,
    total_shell_probes_before_turn: usize,
    total_shell_probes_after_turn: usize,
}

impl RepairNoActionTracker {
    fn observe(
        &mut self,
        turn: usize,
        tool_calls_this_turn: usize,
        before: &ToolPolicySnapshot,
        after: &ToolPolicySnapshot,
    ) -> Option<RepairNoActionDecision> {
        let Some(active_repair) = after.validation_repair.clone() else {
            self.reset();
            return None;
        };
        let active_key = repair_failure_key(&active_repair);
        if self.active_failure_key.as_deref() != Some(active_key.as_str()) {
            self.active_failure_key = Some(active_key.clone());
            self.consecutive_no_action_turns = 0;
        }

        let repair_was_active_before = before
            .validation_repair
            .as_ref()
            .map(repair_failure_key)
            .is_some_and(|before_key| before_key == active_key);
        let wrote_this_turn = after.total_write_operations > before.total_write_operations;
        let probed_this_turn = after.total_shell_probes > before.total_shell_probes;

        if !repair_was_active_before || wrote_this_turn || probed_this_turn {
            self.consecutive_no_action_turns = 0;
            return None;
        }

        self.consecutive_no_action_turns += 1;
        Some(RepairNoActionDecision {
            turn,
            tool_calls_this_turn,
            consecutive_no_action_turns: self.consecutive_no_action_turns,
            escalation_required: self.consecutive_no_action_turns >= MAX_REPAIR_NO_ACTION_TURNS,
            active_repair,
            validation_repair_read_paths: after.validation_repair_read_paths.clone(),
            total_write_operations_before_turn: before.total_write_operations,
            total_write_operations_after_turn: after.total_write_operations,
            total_shell_probes_before_turn: before.total_shell_probes,
            total_shell_probes_after_turn: after.total_shell_probes,
        })
    }

    fn reset(&mut self) {
        self.active_failure_key = None;
        self.consecutive_no_action_turns = 0;
    }
}

fn repair_failure_key(repair: &ValidationRepairSnapshot) -> String {
    format!(
        "{}\n{}",
        repair.command_family.trim(),
        repair.failure_text.trim()
    )
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
        transcript_policy,
        throughput_registry_path,
        progress_projection_override,
        progress_status_interval_override,
        runner_activity_override,
        trace,
        turn,
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
    let mut stream_progress_frame_count = 0usize;
    let mut tool_call_progress_frame_count = 0usize;
    let mut no_content_segment_eval_count = 0usize;
    let no_assistant_content_limit =
        expected_output_tokens.saturating_mul(NO_ASSISTANT_CONTENT_OUTPUT_MULTIPLIER);
    let mut inspection_loop_tracker = InspectionLoopTracker::default();
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

        let ledger = context_assembly_ledger(ContextAssemblyInput {
            model,
            turn,
            llm_call_depth: depth,
            messages: &current_messages,
            tools,
            completion_config: &completion_config,
            context_window_tokens,
            previous_call_total_chars,
            transcript_policy,
        });
        previous_call_total_chars = ledger.total_chars();
        trace.event("llm.context_assembly.ledger", &ledger)?;
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
        let mut stream =
            gateway.complete_stream(model, &current_messages, Some(tools), &completion_config);
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
                Ok(StreamChunk::Progress(progress)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    stream_progress_frame_count += 1;
                    if progress.tool_call_count > 0 || progress.accumulated_tool_call_count > 0 {
                        tool_call_progress_frame_count += 1;
                        latest_progress_state = ModelProgressState::GeneratingToolCall;
                    } else if progress.content_chars > 0 {
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
                            "tool_call_count": progress.tool_call_count,
                            "accumulated_tool_call_count": progress.accumulated_tool_call_count,
                            "stream_progress_frame_count": stream_progress_frame_count,
                            "tool_call_progress_frame_count": tool_call_progress_frame_count,
                            "progress_state": if progress.done { "DoneFrame" } else { latest_progress_state.as_str() },
                        }),
                    )?;
                }
                Ok(StreamChunk::Metrics(metrics)) => {
                    last_observable_progress = Instant::now();
                    stalled_candidate_checks = 0;
                    if call_content.is_empty() && accumulated_tool_calls.is_empty() {
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
                "stream_progress_frame_count": stream_progress_frame_count,
                "tool_call_progress_frame_count": tool_call_progress_frame_count,
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
                    "llm_call_count": depth + 1,
                }),
            )?;
            return Ok(StreamResponseResult {
                response,
                messages: current_messages,
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
            let tool_result = run_tool_call(call, tools, &correlation_id).await;
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
        }
        compact_retained_tool_results(
            &mut current_messages,
            trace,
            turn,
            depth,
            transcript_policy,
        )?;
    }

    unreachable!("tool iteration loop always returns or errors before exhaustion")
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
    meaningful_action_seen: bool,
    signatures: BTreeMap<String, usize>,
}

impl InspectionLoopTracker {
    fn observe(
        &mut self,
        turn: usize,
        llm_call_depth: usize,
        call: &LlmToolCall,
        result: &ToolCallRunResult,
    ) -> Option<InspectionLoopDecision> {
        if self.meaningful_action_seen {
            return None;
        }
        if is_meaningful_source_edit(call, result) || is_validation_probe_result(result) {
            self.meaningful_action_seen = true;
            self.signatures.clear();
            return None;
        }

        let signature = inspection_signature(call)?;
        let count = self.signatures.entry(signature.clone()).or_insert(0);
        *count += 1;
        (*count >= MAX_PRE_VALIDATION_REPEATED_INSPECTIONS).then_some(InspectionLoopDecision {
            signature,
            repeated_count: *count,
            limit: MAX_PRE_VALIDATION_REPEATED_INSPECTIONS,
            turn,
            llm_call_depth,
        })
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
    trace: &TraceRecorder,
    turn: usize,
    llm_call_depth: usize,
    transcript_policy: TranscriptPolicy,
) -> Result<()> {
    let compaction = transcript_policy.compaction();
    if !compaction.enabled {
        return Ok(());
    }
    let retained_tool_indices = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::Tool).then_some(index))
        .collect::<Vec<_>>();
    let latest_failed_validation_index = compaction
        .preserve_latest_failed_validation
        .then(|| latest_failed_validation_tool_index(messages, &retained_tool_indices))
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
                "raw_recent_tool_results_retained": compaction.raw_recent_count,
                "max_raw_tool_result_chars": compaction.max_raw_tool_result_chars,
                "preserved_latest_failed_validation_index": latest_failed_validation_index,
            }),
        )?;
    }

    Ok(())
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
            is_inspection_shell_command(command)
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
        "patch_file" => true,
        "write_file" => call
            .arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .is_some_and(path_requires_validation_after_write),
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

fn is_inspection_shell_command(command: &str) -> bool {
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

fn normalize_shell_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn path_requires_validation_after_write(path: &str) -> bool {
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

fn run_prompt(goal: &str) -> String {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmptyResponseDecision {
    escalation_required: bool,
    prompt: String,
}

fn empty_response_decision(consecutive_empty_responses: usize) -> EmptyResponseDecision {
    EmptyResponseDecision {
        escalation_required: consecutive_empty_responses >= EMPTY_RESPONSE_ESCALATION_TURNS,
        prompt: empty_response_prompt(consecutive_empty_responses),
    }
}

fn validation_repair_prompt(repair: &ValidationRepairSnapshot) -> String {
    format!(
        "Validation repair mode is active.\n\
         Failing command: {command}\n\
         Failure text: {failure_text}\n\
         Command family failure count: {command_count}\n\
         Failure-summary repeat count: {summary_count}\n\
         Your next action must reference that exact failing command and failure text. \
         Prefer one focused diagnostic or a narrow patch before any broad rewrite. \
         If you discuss the same failure again without a probe or edit, run a deterministic probe next. \
         After editing, run the validation ladder: cargo fmt --check, cargo clippy, focused tests, then broad tests.",
        command = repair.command,
        failure_text = repair.failure_text,
        command_count = repair.repeated_command_family_count,
        summary_count = repair.repeated_failure_summary_count,
    )
}

fn validation_repair_no_action_prompt(decision: &RepairNoActionDecision) -> String {
    let repair = &decision.active_repair;
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
        "Validation repair mode remains active. The last repair turn made no edit and ran no validation probe.".to_string()
    };
    format!(
        "{pressure}\n\
         Failing command: {command}\n\
         Failure text: {failure_text}\n\
         Repair read targets since the latest failed validation: {read_targets}\n\
         Your next action must be exactly one of these: apply one focused patch/write_file to the relevant source, \
         run one deterministic probe that narrows the failure, or reply FAIL with a concrete blocker. \
         Do not restate the repair plan without taking one of those actions.",
        command = repair.command,
        failure_text = repair.failure_text,
    )
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
            &trace,
            1,
            2,
            TranscriptPolicy::SummarizedTranscript,
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
            &trace,
            1,
            2,
            TranscriptPolicy::FullTranscript,
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
            &trace,
            1,
            2,
            TranscriptPolicy::ValidationRepairPacket,
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
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
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
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
        })
        .await
        .unwrap_err();

        assert!(error.to_string().contains("no assistant content"));
        let content = std::fs::read_to_string(trace.path()).unwrap();
        assert!(content.contains("\"kind\":\"llm.no_content_stream.hard_failed\""));
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
            transcript_policy: TranscriptPolicy::FullTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
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
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
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
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
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
            transcript_policy: TranscriptPolicy::SummarizedTranscript,
            throughput_registry_path: temp.path().join("model-throughput.jsonl"),
            progress_projection_override: None,
            progress_status_interval_override: None,
            runner_activity_override: None,
            trace: &trace,
            turn: 1,
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
            repeated_command_family_count: 2,
            repeated_failure_summary_count: 1,
        });

        assert!(prompt.contains("Failing command: cargo test"));
        assert!(prompt.contains("Failure text: error[E0425]: cannot find value"));
        assert!(prompt.contains("Command family failure count: 2"));
    }

    #[test]
    fn repair_no_action_tracker_escalates_after_repeated_no_write_turns() {
        let repair = ValidationRepairSnapshot {
            active: true,
            command: "cargo clippy --all-targets".to_string(),
            command_family: "cargo clippy".to_string(),
            status: Some(101),
            failure_text: "error[E0422]: cannot find struct `TextStyle`".to_string(),
            repeated_command_family_count: 1,
            repeated_failure_summary_count: 1,
        };
        let mut tracker = RepairNoActionTracker::default();
        let before = repair_policy_snapshot(3, 2, Some(repair.clone()), BTreeMap::new());
        let after_first = repair_policy_snapshot(
            3,
            2,
            Some(repair.clone()),
            BTreeMap::from([("src/main.rs".to_string(), 1)]),
        );

        let first = tracker.observe(6, 1, &before, &after_first).unwrap();

        assert_eq!(first.consecutive_no_action_turns, 1);
        assert!(!first.escalation_required);

        let after_second = repair_policy_snapshot(
            3,
            2,
            Some(repair.clone()),
            BTreeMap::from([("src/main.rs".to_string(), 3)]),
        );
        let second = tracker.observe(7, 1, &after_first, &after_second).unwrap();

        assert_eq!(second.consecutive_no_action_turns, 2);
        assert!(second.escalation_required);
        assert_eq!(second.validation_repair_read_paths["src/main.rs"], 3);

        let prompt = validation_repair_no_action_prompt(&second);
        assert!(prompt.contains("Validation repair escalation is active"));
        assert!(prompt.contains("src/main.rs (3)"));
        assert!(prompt.contains("Do not restate the repair plan"));

        let after_write = repair_policy_snapshot(4, 2, Some(repair), BTreeMap::new());
        assert!(tracker.observe(8, 1, &after_second, &after_write).is_none());
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
            vec![StreamChunk::Content("FAIL blocked".to_string())],
        ]);

        let summary = fixture.run(&gateway, 4).await;

        assert_eq!(summary.final_summary, "FAIL blocked");
        let trace = std::fs::read_to_string(summary.trace_file).unwrap();
        assert!(trace.contains("\"kind\":\"agent.turn.empty_response_escalated\""));
        assert!(trace.contains("\"consecutive_empty_responses\":3"));
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
    }

    impl ScriptedGateway {
        fn new(streams: Vec<Vec<StreamChunk>>) -> Self {
            Self {
                streams: StdMutex::new(VecDeque::from(streams)),
            }
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
            _tools: Option<&'a [Box<dyn LlmTool>]>,
            _config: &'a CompletionConfig,
        ) -> Pin<Box<dyn futures::Stream<Item = mojentic::Result<StreamChunk>> + Send + 'a>>
        {
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
}
