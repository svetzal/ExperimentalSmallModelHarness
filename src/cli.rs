use crate::acceptance_interactions::{
    AcceptanceInteractionCandidate, DEFAULT_MAX_INTERACTION_OUTPUT_TOKENS,
    DEFAULT_MAX_INTERACTION_SCENARIOS, author_interaction_evidence, plan_acceptance_interactions,
};
use crate::acceptance_plan::{
    AcceptancePlan, DEFAULT_MAX_PLAN_ITEMS, DEFAULT_MAX_PLAN_OUTPUT_TOKENS, plan_acceptance,
};
use crate::agent::{
    AgentRunConfig, Provider, RepairHandoffPolicy, TranscriptPolicy,
    default_expected_output_tokens, default_max_thinking_only_tokens,
    default_repair_exit_thinking_tokens, run_agent,
};
use crate::baseline::{load_matrix_baseline, summarize_matrix};
use crate::contract::{Budgets, ContractSource, resolve_contract};
use crate::retry::{SequentialRunConfig, run_sequential};
use crate::trace_analysis::analyze_trace;
use crate::transcript::{RenderTranscriptConfig, render_transcript};
use anyhow::{Context, Result, bail};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};

const DEFAULT_MODEL: &str = "Qwen3.8-27B-MLX-8bit";

fn model_context_window_tokens(model: &str) -> Option<usize> {
    match model {
        DEFAULT_MODEL | "qwen3.8:27b-mxfp8" => Some(262_144),
        _ => None,
    }
}

pub(crate) fn parse_reasoning_effort(
    value: &str,
) -> Result<mojentic::llm::gateway::ReasoningEffort, String> {
    use mojentic::llm::gateway::ReasoningEffort;
    match value {
        "enabled" => Ok(ReasoningEffort::Enabled),
        "xhigh" => Ok(ReasoningEffort::XHigh),
        "disabled" => Ok(ReasoningEffort::Disabled),
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        _ => Err(
            "expected disabled, enabled, low, medium, high, or xhigh; omit for the model default"
                .into(),
        ),
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "microgentic",
    version,
    about = "An adaptive coding agent optimized for small models",
    after_help = "Examples:\n  microgentic\n  microgentic setup\n  echo 'Hello!' | microgentic run\n  microgentic run --prompt 'Fix the parser'\n\nRun uses the existing agent tool and validation policies and can edit the selected project."
)]
struct Cli {
    /// User configuration file (default: $XDG_CONFIG_HOME/microgentic/config.json).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  echo 'Hello!' | microgentic run\n  microgentic run --prompt 'Fix the parser' --dir .\n  microgentic run --prompt 'Review this diff' < changes.diff\n\nRun can edit files and execute project commands under the existing runtime policy. Research controls retain their previous meanings. Wire recovery is disabled by default; SIGINT/SIGTERM persist terminal request evidence."
)]
struct RunArgs {
    /// Prompt. When omitted, read from piped or redirected stdin.
    #[arg(long, conflicts_with = "goal")]
    prompt: Option<String>,

    /// Project root for file and shell tools (default: current directory).
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// Emit the existing complete run summary as JSON instead of final response text.
    #[arg(long)]
    json: bool,

    /// Advanced: contract/goal base directory; independent of the tool root (--dir).
    #[arg(long, help_heading = "Advanced / research")]
    experiment: Option<PathBuf>,

    /// Trace destination. Defaults to `<experiment>/traces`; may be placed
    /// outside the tool-visible experiment root by an external evaluator.
    #[arg(long, help_heading = "Advanced / research")]
    trace_dir: Option<PathBuf>,

    /// Goal file, relative to the experiment root unless absolute.
    #[arg(
        long,
        conflicts_with = "contract",
        help_heading = "Advanced / research"
    )]
    goal: Option<PathBuf>,

    /// Explicit run-contract JSON file, relative to the experiment root unless absolute.
    #[arg(long, help_heading = "Advanced / research")]
    contract: Option<PathBuf>,

    /// Local inference provider. Uses OMLX_HOST or OLLAMA_HOST.
    #[arg(long, value_enum)]
    provider: Option<Provider>,

    /// Provider model ID. Defaults to Qwen3.8-27B-MLX-8bit (oMLX) or qwen3.8:27b-mxfp8 (Ollama).
    #[arg(long)]
    model: Option<String>,

    /// Maximum Mojentic iterative-solver turns.
    #[arg(long, default_value_t = 10, help_heading = "Advanced / research")]
    max_iterations: usize,

    /// Maximum model interaction cycles per agent turn.
    #[arg(
        long,
        alias = "max-tool-iterations",
        default_value_t = 75,
        help_heading = "Advanced / research"
    )]
    max_model_interactions: usize,

    /// Context window for harness accounting and Ollama num_ctx; configure the actual oMLX limit on the server.
    /// Defaults to 262144 for Qwen3.8-27B-MLX-8bit and Mojentic accounting otherwise.
    #[arg(long, help_heading = "Advanced / research")]
    context_window_tokens: Option<usize>,

    /// Packet type used for default output-token budgeting.
    #[arg(
        long,
        default_value = "multi-file-edit",
        help_heading = "Advanced / research"
    )]
    packet_type: String,

    /// Override the packet output-token budget.
    #[arg(long, help_heading = "Advanced / research")]
    expected_output_tokens: Option<usize>,

    /// Maximum generated tokens per request, including reasoning (default: 16384).
    #[arg(
        long,
        visible_alias = "num-predict",
        help_heading = "Advanced / research"
    )]
    max_tokens: Option<usize>,

    /// Worker reasoning effort: disabled, enabled, low, medium, high, xhigh. Omit for the model default.
    /// Named levels depend on the model's chat template; advisory calls keep their defaults.
    #[arg(long, value_parser = parse_reasoning_effort)]
    reasoning_effort: Option<mojentic::llm::gateway::ReasoningEffort>,

    /// Explicit model capability profile; omit to leave validation to the provider.
    #[arg(long, value_parser = ["qwen3.8"], help_heading = "Advanced / research")]
    reasoning_profile: Option<String>,

    /// Preserve completed native assistant reasoning in history (separate from capped checkpoints).
    #[arg(long, help_heading = "Advanced / research")]
    preserve_reasoning: bool,

    /// Max reasoning-only tokens in one LLM call before an action-only handoff. Disabled by default; positive values explicitly opt into interruption.
    #[arg(long, help_heading = "Advanced / research")]
    max_thinking_only_tokens: Option<usize>,

    /// Post-validation hidden-only reasoning interruption. Disabled by default; positive values opt in.
    #[arg(long, help_heading = "Advanced / research")]
    repair_exit_thinking_tokens: Option<usize>,

    /// Action handoff policy: text-only preserves the legacy continuation;
    /// constrained ends the failed-probe turn and exposes only repair-action tools;
    /// constrained-action-only disables model reasoning after a capped repair attempt,
    /// withholding arbitrary shell during the native action-only request.
    #[arg(
        long,
        default_value = "text-only",
        help_heading = "Advanced / research"
    )]
    repair_handoff_policy: String,

    /// Pre-validation hidden action-intent reasoning tokens before interrupting and forcing an action. 0 disables.
    #[arg(long, default_value_t = 0, help_heading = "Advanced / research")]
    action_boundary_interrupt_tokens: usize,

    /// Retain this many tokens (approximately) from the tail of a hidden-only
    /// no-action turn as a continuity checkpoint for the next ordinary turn.
    /// This does not disable reasoning or narrow the tool surface. 0 disables.
    #[arg(long, default_value_t = 0, help_heading = "Advanced / research")]
    reasoning_checkpoint_tokens: usize,

    /// Transcript retention policy for model/tool context.
    #[arg(
        long,
        default_value = "summarized-transcript",
        help_heading = "Advanced / research"
    )]
    transcript_policy: String,

    /// Adapter-owned initial-context catalog JSON. Task-only assembly when omitted.
    #[arg(long, help_heading = "Advanced / research")]
    initial_context_catalog: Option<PathBuf>,

    /// Model used for isolated semantic advisory calls. Defaults to the worker model.
    #[arg(long, help_heading = "Advanced / research")]
    semantic_advisor_model: Option<String>,

    /// Generate a proposal-derived acceptance checklist and interaction ledger,
    /// retain it in worker context, and require current coverage before DONE.
    /// Ledger coverage is advisory and does not replace declared probes.
    #[arg(long, default_value_t = false, help_heading = "Advanced / research")]
    acceptance_ledger: bool,
    #[command(flatten, next_help_heading = "Advanced / research")]
    recovery: crate::recovery::RecoverySettings,
}

impl RunArgs {
    fn into_config(self) -> Result<AgentRunConfig> {
        if self.max_tokens == Some(0) {
            bail!("--max-tokens must be greater than zero");
        }
        let transcript_policy =
            TranscriptPolicy::parse(&self.transcript_policy).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid transcript policy {:?}; expected one of full-transcript, \
                 summarized-transcript, summarized-repair-handoff, \
                 validation-repair-packet",
                    self.transcript_policy
                )
            })?;
        let expected_output_tokens = self
            .expected_output_tokens
            .unwrap_or_else(|| default_expected_output_tokens(&self.packet_type));
        let max_thinking_only_tokens = self.max_thinking_only_tokens.unwrap_or_else(|| {
            default_max_thinking_only_tokens(expected_output_tokens, self.max_tokens)
        });
        let repair_exit_thinking_tokens = self
            .repair_exit_thinking_tokens
            .unwrap_or_else(default_repair_exit_thinking_tokens);
        let repair_handoff_policy = RepairHandoffPolicy::parse(&self.repair_handoff_policy)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid repair handoff policy {:?}; expected text-only, constrained, or \
                     constrained-action-only",
                    self.repair_handoff_policy
                )
            })?;
        let provider = self.provider.unwrap_or_default();
        let model = self
            .model
            .unwrap_or_else(|| provider.default_model().to_string());
        let context_window_tokens = self
            .context_window_tokens
            .or_else(|| model_context_window_tokens(&model));
        Ok(AgentRunConfig {
            recovery: self.recovery,
            provider,
            experiment_dir: self.experiment.unwrap_or_else(|| PathBuf::from(".")),
            trace_dir: self.trace_dir,
            goal_file: self.goal.unwrap_or_else(|| PathBuf::from("task.md")),
            contract_file: self.contract,
            model,
            max_iterations: self.max_iterations,
            max_model_interactions: self.max_model_interactions,
            context_window_tokens,
            packet_type: self.packet_type,
            expected_output_tokens,
            max_tokens: self.max_tokens,
            reasoning_effort: self.reasoning_effort,
            reasoning_profile: self
                .reasoning_profile
                .as_ref()
                .map(|_| mojentic::llm::ReasoningProfile::qwen3_8()),
            preserve_reasoning: self.preserve_reasoning,
            max_thinking_only_tokens,
            repair_exit_thinking_tokens,
            repair_handoff_policy,
            action_boundary_interrupt_tokens: self.action_boundary_interrupt_tokens,
            reasoning_checkpoint_tokens: self.reasoning_checkpoint_tokens,
            transcript_policy,
            initial_context_catalog_file: self.initial_context_catalog,
            semantic_advisor_model: self.semantic_advisor_model,
            acceptance_ledger: self.acceptance_ledger,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn benchmark_grammar_preserves_file_intake_and_runtime_defaults() {
        assert!(
            command_definition()
                .try_get_matches_from(["microgentic", "benchmark", "run"])
                .is_err()
        );
        let matches = command_definition()
            .try_get_matches_from([
                "microgentic",
                "benchmark",
                "run",
                "--experiment",
                "cell",
                "--goal",
                "unused.md",
                "--contract",
                "contract.json",
            ])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let Some(Command::Benchmark {
            command: BenchmarkCommand::Run(run),
        }) = cli.command
        else {
            panic!("expected benchmark run");
        };
        let config = run.into_config().unwrap();
        assert_eq!(config.provider, Provider::Omlx);
        assert_eq!(config.model, DEFAULT_MODEL);
        assert_eq!(config.max_iterations, 10);
        assert_eq!(config.max_model_interactions, 75);
        assert_eq!(config.goal_file, PathBuf::from("unused.md"));
        assert_eq!(config.contract_file, Some(PathBuf::from("contract.json")));
        assert!(config.reasoning_effort.is_none());
        assert!(config.max_tokens.is_none());

        let matches = command_definition()
            .try_get_matches_from([
                "microgentic",
                "benchmark",
                "run-sequential",
                "--experiment",
                "cell",
                "--artifact",
                "answer.txt",
                "--expected-artifact",
                "expected.txt",
            ])
            .unwrap();
        let cli = Cli::from_arg_matches(&matches).unwrap();
        let Some(Command::Benchmark {
            command: BenchmarkCommand::RunSequential(args),
        }) = cli.command
        else {
            panic!("expected benchmark sequential run");
        };
        assert_eq!(args.run.goal, Some(PathBuf::from("task.md")));
        assert_eq!(args.max_attempts, 8);
        assert_eq!(args.max_failed_repair_cycles, 8);
        assert_eq!(args.max_consecutive_unchanged_attempts, 2);
        // The everyday surface still rejects two authoritative input files.
        assert!(
            command_definition()
                .try_get_matches_from([
                    "microgentic",
                    "run",
                    "--goal",
                    "task.md",
                    "--contract",
                    "contract.json"
                ])
                .is_err()
        );
    }

    #[test]
    fn everyday_grammar_has_no_positional_prompt_or_required_experiment() {
        assert!(
            Cli::try_parse_from(["microgentic"])
                .unwrap()
                .command
                .is_none()
        );
        assert!(Cli::try_parse_from(["microgentic", "run", "--prompt", "Hello!"]).is_ok());
        assert!(Cli::try_parse_from(["microgentic", "run", "Hello!"]).is_err());
        assert!(
            Cli::try_parse_from(["microgentic", "run", "--prompt", "a", "--prompt", "b"]).is_err()
        );
    }

    #[test]
    fn prompt_sources_preserve_bytes_and_explicit_prompts_remain_authoritative() {
        let source = "  hello\nUnicode: 世界\n";
        assert_eq!(prompt_text(None, Some(source.into())).unwrap(), source);
        assert_eq!(prompt_text(Some(source.into()), None).unwrap(), source);
        let combined = prompt_text(Some("Review".into()), Some("diff\n".into())).unwrap();
        assert!(combined.starts_with("Review\n\n[Reference material"));
        assert!(combined.contains("diff\n"));
        assert!(prompt_text(None, None).is_err());
        assert!(prompt_text(None, Some(" \n".into())).is_err());
        assert!(prompt_text(Some("".into()), Some("fallback".into())).is_err());
    }

    #[test]
    fn reference_shell_fences_do_not_acquire_probe_authority() {
        let reference = "Old build instructions:\n```sh\ncargo test\n```\n";
        let (document, suffix) =
            prompt_document(Some("Review this text".into()), Some(reference.into())).unwrap();
        assert_eq!(suffix, ".json");
        let value: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert!(value["probes"].as_array().unwrap().is_empty());
        assert!(value["guidance"].as_str().unwrap().contains(reference));
        let (document, _) = prompt_document(
            Some("Run this check:\n```sh\ncargo test\n```\n".into()),
            Some(reference.into()),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert_eq!(value["probes"].as_array().unwrap().len(), 1);
        assert_eq!(value["probes"][0]["command"], "cargo test");
    }

    #[test]
    fn provider_defaults_and_explicit_model_are_resolved_together() {
        for (provider, model) in [
            ("omlx", "Qwen3.8-27B-MLX-8bit"),
            ("ollama", "qwen3.8:27b-mxfp8"),
        ] {
            for override_model in [None, Some("custom-model")] {
                let mut args = vec![
                    "harness",
                    "run",
                    "--experiment",
                    ".",
                    "--provider",
                    provider,
                ];
                if let Some(name) = override_model {
                    args.extend(["--model", name]);
                }
                let Command::Run(args) = Cli::try_parse_from(args).unwrap().command.unwrap() else {
                    panic!("run")
                };
                let config = args.into_config().unwrap();
                assert_eq!(config.provider.as_str(), provider);
                assert_eq!(config.model, override_model.unwrap_or(model));
                assert_eq!(
                    config.context_window_tokens,
                    if override_model.is_none() {
                        Some(262144)
                    } else {
                        None
                    }
                );
            }
        }
        assert!(
            Cli::try_parse_from([
                "harness",
                "run",
                "--experiment",
                ".",
                "--provider",
                "unknown"
            ])
            .is_err()
        );
    }

    #[test]
    fn output_budget_alias_and_reasoning_effort_reach_worker_config() {
        for flag in ["--max-tokens", "--num-predict"] {
            let cli = Cli::try_parse_from([
                "harness",
                "run",
                "--experiment",
                ".",
                flag,
                "32768",
                "--reasoning-effort",
                "low",
            ])
            .unwrap();
            let Command::Run(args) = cli.command.unwrap() else {
                panic!("expected run")
            };
            let config = args.into_config().unwrap();
            assert_eq!(config.max_tokens, Some(32768));
            assert_eq!(config.max_thinking_only_tokens, 0);
            assert_eq!(config.repair_exit_thinking_tokens, 0);
            assert_eq!(
                config.reasoning_effort,
                Some(mojentic::llm::gateway::ReasoningEffort::Low)
            );
        }
    }

    #[test]
    fn invalid_effort_and_zero_budget_are_rejected() {
        assert!(
            Cli::try_parse_from([
                "harness",
                "run",
                "--experiment",
                ".",
                "--reasoning-effort",
                "extreme",
            ])
            .is_err()
        );
        let cli = Cli::try_parse_from(["harness", "run", "--experiment", ".", "--max-tokens", "0"])
            .unwrap();
        let Command::Run(args) = cli.command.unwrap() else {
            panic!("expected run")
        };
        assert!(args.into_config().is_err());
    }

    #[test]
    fn model_interaction_limit_defaults_to_seventy_five() {
        let cli = Cli::try_parse_from(["harness", "run", "--experiment", "."]).unwrap();
        let Command::Run(args) = cli.command.unwrap() else {
            panic!("expected run command");
        };
        assert_eq!(args.max_model_interactions, 75);
    }

    #[test]
    fn deprecated_tool_iteration_flag_remains_an_alias() {
        let cli = Cli::try_parse_from([
            "harness",
            "run",
            "--experiment",
            ".",
            "--max-tool-iterations",
            "61",
        ])
        .unwrap();
        let Command::Run(args) = cli.command.unwrap() else {
            panic!("expected run command");
        };
        assert_eq!(args.max_model_interactions, 61);
    }

    #[test]
    fn omlx_qwen_uses_advertised_context_window() {
        assert_eq!(
            model_context_window_tokens("Qwen3.8-27B-MLX-8bit"),
            Some(262_144)
        );
        assert_eq!(model_context_window_tokens("unknown-omlx-model"), None);
    }

    #[test]
    fn unknown_models_leave_context_window_to_the_provider() {
        assert_eq!(model_context_window_tokens("another-model"), None);
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Configure provider, default model, and reasoning effort (previews unless --apply).
    Setup(SetupArgs),

    /// Run a prompt from stdin or --prompt using the existing runtime. [Mutates]
    Run(RunArgs),

    /// Internal isolated worker for the terminal conversation.
    #[command(hide = true)]
    ConversationWorker {
        #[command(flatten)]
        run: RunArgs,
        #[arg(long)]
        session_state: PathBuf,
    },

    /// Run isolated task contracts with historical defaults and JSON output. [Mutates]
    Benchmark {
        #[command(subcommand)]
        command: BenchmarkCommand,
    },

    /// Run bounded natural attempts on one retained worker artifact.
    RunSequential(SequentialArgs),

    /// Summarize context assembly and tool payload pressure from trace JSONL files.
    AnalyzeTrace {
        /// Trace JSONL files to analyze.
        #[arg(required = true)]
        traces: Vec<PathBuf>,
    },

    /// Render one or more harness traces as a self-contained HTML transcript.
    RenderTranscript {
        /// Trace JSONL files or directories to discover recursively.
        #[arg(required = true)]
        traces: Vec<PathBuf>,

        /// Destination HTML file.
        #[arg(long)]
        output: PathBuf,

        /// Report title embedded in the single-file transcript.
        #[arg(long, default_value = "Agent Transcript")]
        title: String,

        /// Optional generic transcript_evidence.v1 JSON file.
        #[arg(long)]
        evidence: Option<PathBuf>,
    },

    /// Summarize the preserved 30-cell demo matrix baseline into canonical
    /// counts (harness completion, independent validation, hard-stops,
    /// environment corrections), replacing matrix-specific classification.
    SummarizeMatrix,

    /// Resolve a run contract without starting a provider or gateway:
    /// either a legacy Markdown `task.md` (via shell-fence scraping) or an
    /// explicit contract JSON file. Prints the resolved contract. This is
    /// both the no-provider resolution command and the snapshot
    /// generator/checker for `fixtures/contracts/snapshots/`.
    ResolveContract {
        /// Legacy Markdown goal file. Mutually exclusive with `--contract`.
        #[arg(long)]
        goal: Option<PathBuf>,

        /// Explicit contract JSON file. Mutually exclusive with `--goal`.
        #[arg(long)]
        contract: Option<PathBuf>,
    },

    /// Propose and validate a measurement-only acceptance checklist without
    /// starting the worker loop or exposing tools.
    PlanTask {
        /// Legacy Markdown goal file. Mutually exclusive with --contract.
        #[arg(long)]
        goal: Option<PathBuf>,

        /// Explicit run contract. Mutually exclusive with --goal.
        #[arg(long)]
        contract: Option<PathBuf>,

        /// Trace directory for the isolated planning call.
        #[arg(long)]
        trace_dir: PathBuf,

        /// oMLX model ID from /v1/models.
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Maximum acceptance items allowed in the proposal.
        #[arg(long, default_value_t = DEFAULT_MAX_PLAN_ITEMS)]
        max_items: usize,

        /// Maximum generated tokens for the isolated planning call.
        #[arg(long, default_value_t = DEFAULT_MAX_PLAN_OUTPUT_TOKENS)]
        max_output_tokens: usize,
    },

    /// Propose and validate measurement-only combined scenarios over a fixed
    /// acceptance plan without starting the worker loop or exposing tools.
    PlanInteractions {
        /// Legacy Markdown goal file. Mutually exclusive with --contract.
        #[arg(long)]
        goal: Option<PathBuf>,

        /// Explicit run contract. Mutually exclusive with --goal.
        #[arg(long)]
        contract: Option<PathBuf>,

        /// Validated atomic acceptance-plan JSON file.
        #[arg(long)]
        plan: PathBuf,

        /// Trace directory for isolated interaction planning.
        #[arg(long)]
        trace_dir: PathBuf,

        /// oMLX model ID from /v1/models.
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Maximum interaction scenarios allowed in the proposal.
        #[arg(long, default_value_t = DEFAULT_MAX_INTERACTION_SCENARIOS)]
        max_scenarios: usize,

        /// Maximum generated tokens for each isolated attempt.
        #[arg(long, default_value_t = DEFAULT_MAX_INTERACTION_OUTPUT_TOKENS)]
        max_output_tokens: usize,
    },

    /// Author and validate measurement-only evidence for one deterministic
    /// interaction candidate without starting the worker loop or exposing tools.
    AuthorInteractionEvidence {
        /// Legacy Markdown goal file. Mutually exclusive with --contract.
        #[arg(long)]
        goal: Option<PathBuf>,

        /// Explicit run contract. Mutually exclusive with --goal.
        #[arg(long)]
        contract: Option<PathBuf>,

        /// Validated atomic acceptance-plan JSON file.
        #[arg(long)]
        plan: PathBuf,

        /// Deterministically supplied interaction-candidate JSON file.
        #[arg(long)]
        candidate: PathBuf,

        /// Trace directory for isolated evidence authoring.
        #[arg(long)]
        trace_dir: PathBuf,

        /// oMLX model ID from /v1/models.
        #[arg(long, default_value = DEFAULT_MODEL)]
        model: String,

        /// Maximum generated tokens for each isolated attempt.
        #[arg(long, default_value_t = DEFAULT_MAX_INTERACTION_OUTPUT_TOKENS)]
        max_output_tokens: usize,
    },
}

#[derive(Debug, Subcommand)]
#[command(
    after_help = "Examples:\n  microgentic benchmark run --experiment ./cell --dir ./cell/workspace\n  microgentic benchmark run --experiment ./cell --contract contract.json --provider ollama --model qwen3.8:27b-mxfp8\n\nUses task.md by default. Ignores stdin, personal setup, and conversation history. Outputs the complete JSON summary. Provider environment variables still select server URLs and credentials."
)]
enum BenchmarkCommand {
    /// Run one isolated task contract. [Mutates]
    Run(RunArgs),
    /// Run bounded natural attempts on one retained worker artifact. [Mutates]
    RunSequential(SequentialArgs),
}

#[derive(Debug, Args)]
struct SequentialArgs {
    #[command(flatten)]
    run: RunArgs,

    /// Root-relative worker artifact whose state is retained between attempts.
    #[arg(long)]
    artifact: PathBuf,

    /// Expected artifact file, relative to the experiment root unless absolute.
    #[arg(long)]
    expected_artifact: PathBuf,

    /// Maximum number of naturally completed attempts.
    #[arg(long, default_value_t = 8)]
    max_attempts: usize,

    /// Stop after this many completed failed edit-to-probe cycles.
    #[arg(long, default_value_t = 8)]
    max_failed_repair_cycles: usize,

    /// Stop after this many consecutive attempts leave the artifact unchanged.
    #[arg(long, default_value_t = 2)]
    max_consecutive_unchanged_attempts: usize,
}

// Reuse every runtime control while giving benchmark intake its historical grammar.
// Keep this in one command factory so parsing and help cannot diverge.
fn command_definition() -> clap::Command {
    fn benchmark_run(command: clap::Command) -> clap::Command {
        command
            .mut_arg("experiment", |arg| arg.required(true))
            .mut_arg("prompt", |arg| arg.hide(true))
            .mut_arg("json", |arg| arg.hide(true))
            .mut_arg("goal", |arg| {
                arg.conflicts_with(clap::builder::Resettable::<clap::Id>::Reset).default_value("task.md")
            })
            .after_help("Example:\n  microgentic benchmark run --experiment ./cell --dir ./cell/workspace --contract contract.json\n\nInput comes from --contract or --goal (default: task.md). A contract takes precedence over a goal. JSON output is unconditional; stdin, --config, and TUI history are ignored. --prompt is unavailable in benchmark mode. Runtime controls retain their historical meanings.")
    }
    Cli::command().mut_subcommand("benchmark", |command| {
        command
            .mut_subcommand("run", benchmark_run)
            .mut_subcommand("run-sequential", |command| {
                benchmark_run(command).after_help("Example:\n  microgentic benchmark run-sequential --experiment ./cell --dir ./cell/workspace --artifact answer.txt --expected-artifact expected.txt\n\nUses the same isolated file intake and JSON output as benchmark run. Retains the selected artifact between bounded task attempts.")
            })
    })
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  microgentic setup\n  microgentic setup --provider ollama --model qwen3.8:27b-mxfp8 --reasoning-effort low --apply\n\nCredentials and server URLs remain in OMLX_HOST/OMLX_API_KEY or OLLAMA_HOST. No provider call is made during setup."
)]
struct SetupArgs {
    #[arg(long, value_enum)]
    provider: Option<Provider>,
    #[arg(long)]
    model: Option<String>,
    /// Use default to preserve the model/provider default.
    #[arg(long, value_parser = ["default", "disabled", "enabled", "low", "medium", "high", "xhigh"])]
    reasoning_effort: Option<String>,
    /// Save the reviewed configuration. Without this flag setup only previews.
    #[arg(long)]
    apply: bool,
    #[arg(long)]
    json: bool,
}

/// Shared application boundary for the public and compatibility executables.
pub async fn main_entry(legacy: bool) -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none())
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "adaptive_agent_harness=info".into()),
        )
        .init();
    dispatch(legacy).await
}

pub async fn run() -> Result<()> {
    dispatch(false).await
}

async fn dispatch(legacy: bool) -> Result<()> {
    let cli = Cli::from_arg_matches(&command_definition().get_matches())?;
    let Some(command) = cli.command else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            bail!("interactive mode needs a terminal; try: echo 'Hello!' | microgentic run");
        }
        return crate::tui::run(cli.config).await;
    };
    match command {
        Command::Setup(args) => setup(args, cli.config.as_deref())?,
        Command::Run(run) => {
            let json = run.json || legacy;
            let config = prepare_run(run, cli.config.as_deref(), legacy)?;
            let summary = run_agent(config).await?;
            let output = if json {
                serde_json::to_string_pretty(&summary)?
            } else {
                summary.final_summary
            };
            writeln!(std::io::stdout().lock(), "{output}")?;
        }
        Command::ConversationWorker { run, session_state } => {
            if run.experiment.is_some() || run.goal.is_some() || run.contract.is_some() {
                bail!("conversation workers cannot accept benchmark contracts or experiments");
            }
            let prompt = run
                .prompt
                .clone()
                .context("conversation worker requires --prompt")?;
            let session_state = absolute(&session_state)?;
            // The TUI already resolved and sealed its settings at session entry.
            let config = prepare_run_with_mode(run, cli.config.as_deref(), true, true)?;
            crate::conversation::run_worker(config, prompt, session_state).await?;
        }
        Command::Benchmark { command } => match command {
            BenchmarkCommand::Run(run) => {
                let summary = run_agent(prepare_benchmark_run(run)?).await?;
                println!("{}", serde_json::to_string_pretty(&summary)?);
            }
            BenchmarkCommand::RunSequential(args) => {
                let agent = prepare_benchmark_run(args.run)?;
                let summary = run_sequential(SequentialRunConfig {
                    agent,
                    artifact: args.artifact,
                    expected_artifact: args.expected_artifact,
                    max_attempts: args.max_attempts,
                    max_failed_repair_cycles: args.max_failed_repair_cycles,
                    max_consecutive_unchanged_attempts: args.max_consecutive_unchanged_attempts,
                })
                .await?;
                println!("{}", serde_json::to_string_pretty(&summary)?);
            }
        },
        Command::RunSequential(args) => {
            let summary = run_sequential(SequentialRunConfig {
                agent: prepare_run(args.run, cli.config.as_deref(), legacy)?,
                artifact: args.artifact,
                expected_artifact: args.expected_artifact,
                max_attempts: args.max_attempts,
                max_failed_repair_cycles: args.max_failed_repair_cycles,
                max_consecutive_unchanged_attempts: args.max_consecutive_unchanged_attempts,
            })
            .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::AnalyzeTrace { traces } => {
            let summaries = traces
                .iter()
                .map(analyze_trace)
                .collect::<Result<Vec<_>>>()?;
            println!("{}", serde_json::to_string_pretty(&summaries)?);
        }
        Command::RenderTranscript {
            traces,
            output,
            title,
            evidence,
        } => {
            let summary = render_transcript(RenderTranscriptConfig {
                inputs: traces,
                output,
                title,
                evidence_file: evidence,
            })?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::SummarizeMatrix => {
            let baseline = load_matrix_baseline();
            let summary = summarize_matrix(&baseline.cells);
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::ResolveContract { goal, contract } => {
            let source = contract_source(goal, contract)?;
            let resolved = resolve_contract(source, Budgets::default())?;
            println!("{}", serde_json::to_string_pretty(&resolved)?);
        }
        Command::PlanTask {
            goal,
            contract,
            trace_dir,
            model,
            max_items,
            max_output_tokens,
        } => {
            if max_items == 0 {
                bail!("--max-items must be greater than zero");
            }
            if max_output_tokens == 0 {
                bail!("--max-output-tokens must be greater than zero");
            }
            let resolved = resolve_contract(contract_source(goal, contract)?, Budgets::default())?;
            let summary = plan_acceptance(
                &model,
                &resolved.guidance,
                &trace_dir,
                max_items,
                max_output_tokens,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::PlanInteractions {
            goal,
            contract,
            plan,
            trace_dir,
            model,
            max_scenarios,
            max_output_tokens,
        } => {
            if max_scenarios == 0 {
                bail!("--max-scenarios must be greater than zero");
            }
            if max_output_tokens == 0 {
                bail!("--max-output-tokens must be greater than zero");
            }
            let resolved = resolve_contract(contract_source(goal, contract)?, Budgets::default())?;
            let plan_text = std::fs::read_to_string(&plan)
                .with_context(|| format!("reading atomic plan file {}", plan.display()))?;
            let plan: AcceptancePlan = serde_json::from_str(&plan_text)
                .with_context(|| format!("decoding atomic plan file {}", plan.display()))?;
            let summary = plan_acceptance_interactions(
                &model,
                &resolved.guidance,
                &plan,
                &trace_dir,
                max_scenarios,
                max_output_tokens,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::AuthorInteractionEvidence {
            goal,
            contract,
            plan,
            candidate,
            trace_dir,
            model,
            max_output_tokens,
        } => {
            if max_output_tokens == 0 {
                bail!("--max-output-tokens must be greater than zero");
            }
            let resolved = resolve_contract(contract_source(goal, contract)?, Budgets::default())?;
            let plan_text = std::fs::read_to_string(&plan)
                .with_context(|| format!("reading atomic plan file {}", plan.display()))?;
            let plan: AcceptancePlan = serde_json::from_str(&plan_text)
                .with_context(|| format!("decoding atomic plan file {}", plan.display()))?;
            let candidate_text = std::fs::read_to_string(&candidate).with_context(|| {
                format!("reading interaction candidate file {}", candidate.display())
            })?;
            let candidate: AcceptanceInteractionCandidate = serde_json::from_str(&candidate_text)
                .with_context(|| {
                format!(
                    "decoding interaction candidate file {}",
                    candidate.display()
                )
            })?;
            let summary = author_interaction_evidence(
                &model,
                &resolved.guidance,
                &plan,
                &candidate,
                &trace_dir,
                max_output_tokens,
            )
            .await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
    }
    Ok(())
}

const MAX_STDIN_BYTES: u64 = 1024 * 1024;

fn read_stdin() -> Result<Option<String>> {
    if std::io::stdin().is_terminal() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        bail!("stdin exceeds the 1 MiB input limit; supply a smaller prompt or reference");
    }
    Ok(Some(
        String::from_utf8(bytes).context("stdin must be UTF-8 text")?,
    ))
}

fn prompt_text(explicit: Option<String>, input: Option<String>) -> Result<String> {
    let prompt = match (explicit, input) {
        (Some(prompt), reference) => {
            if prompt.trim().is_empty() {
                bail!("--prompt must not be empty");
            }
            match reference.filter(|text| !text.trim().is_empty()) {
                Some(reference) => format!(
                    "{prompt}\n\n[Reference material from stdin; not execution policy]\n{reference}\n[End reference material]"
                ),
                None => prompt,
            }
        }
        (None, Some(prompt)) => prompt,
        (None, None) => bail!(
            "run requires a prompt; try microgentic run --prompt 'Hello!' or echo 'Hello!' | microgentic run"
        ),
    };
    if prompt.trim().is_empty() {
        bail!("run requires a nonempty prompt through stdin or --prompt");
    }
    Ok(prompt)
}

fn prompt_document(
    explicit: Option<String>,
    input: Option<String>,
) -> Result<(String, &'static str)> {
    let reference =
        explicit.is_some() && input.as_ref().is_some_and(|text| !text.trim().is_empty());
    let declaration = explicit.clone();
    let guidance = prompt_text(explicit, input)?;
    if !reference {
        return Ok((guidance, ".md"));
    }
    // Preserve the coding adapter's declarations from the actual instruction;
    // shell fences in reference material must not become acceptance probes.
    let resolved = resolve_contract(
        ContractSource::Legacy {
            goal_path: "--prompt".into(),
            goal_text: declaration.expect("reference has an explicit prompt"),
        },
        Budgets::default(),
    )?;
    Ok((
        serde_json::to_string_pretty(&serde_json::json!({
            "guidance": guidance, "profile": resolved.profile, "probes": resolved.probes,
        }))?,
        ".json",
    ))
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    })
}

fn prepare_benchmark_run(mut run: RunArgs) -> Result<AgentRunConfig> {
    if run.prompt.is_some() {
        bail!(
            "benchmark mode reads task files; use --goal task.md or --contract contract.json instead of --prompt"
        );
    }
    let experiment = run
        .experiment
        .as_ref()
        .context("benchmark mode requires --experiment")?;
    run.experiment = Some(absolute(experiment)?);
    if let Some(trace) = &mut run.trace_dir {
        *trace = absolute(trace)?;
    }
    let root = absolute(&run.dir)?
        .canonicalize()
        .context("opening --dir project directory")?;
    if !root.is_dir() {
        bail!("--dir must name a project directory");
    }
    // No personal defaults, stdin intake, prompt files, or conversation state.
    std::env::set_current_dir(root)?;
    run.into_config()
}

fn prepare_run(run: RunArgs, config_path: Option<&Path>, legacy: bool) -> Result<AgentRunConfig> {
    prepare_run_with_mode(run, config_path, legacy, false)
}

fn prepare_run_with_mode(
    mut run: RunArgs,
    config_path: Option<&Path>,
    legacy: bool,
    conversational: bool,
) -> Result<AgentRunConfig> {
    let root = absolute(&run.dir)?
        .canonicalize()
        .context("opening --dir project directory")?;
    if !root.is_dir() {
        bail!("--dir must name a project directory");
    }
    let advanced = run.experiment.is_some() || run.goal.is_some() || run.contract.is_some();
    // Benchmark and historical invocations must not inherit personal defaults.
    if !legacy && !advanced {
        let path = crate::preferences::config_path(config_path)?;
        if let Some(prefs) = crate::preferences::load(&path)? {
            let provider = run.provider.unwrap_or(prefs.provider);
            if run.model.is_none() && provider == prefs.provider {
                run.model = Some(prefs.model);
            }
            if run.reasoning_effort.is_none() && provider == prefs.provider {
                run.reasoning_effort = prefs
                    .reasoning_effort
                    .as_deref()
                    .map(parse_reasoning_effort)
                    .transpose()
                    .map_err(anyhow::Error::msg)?;
            }
            run.provider = Some(provider);
        }
    }
    let input = if !advanced || run.prompt.is_some() {
        read_stdin()?
    } else {
        None
    };
    if let Some(contract) = &run.contract {
        if let Some(prompt) = &run.prompt {
            if input.as_ref().is_some_and(|text| !text.trim().is_empty()) {
                bail!("--contract cannot receive additional stdin reference material");
            }
            let base = run
                .experiment
                .as_ref()
                .map(|p| absolute(p))
                .transpose()?
                .unwrap_or(root.clone());
            let path = if contract.is_absolute() {
                contract.clone()
            } else {
                base.join(contract)
            };
            let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            if prompt.trim().is_empty() || value["guidance"].as_str() != Some(prompt.as_str()) {
                bail!(
                    "--prompt must exactly match --contract guidance; the contract is authoritative"
                );
            }
        }
    } else if !advanced || run.prompt.is_some() {
        let (prompt, suffix) = if conversational {
            (
                crate::conversation::message_document(prompt_text(run.prompt.take(), input)?),
                ".json",
            )
        } else {
            prompt_document(run.prompt.take(), input)?
        };
        let base = if let Some(experiment) = &run.experiment {
            absolute(experiment)?
        } else {
            let state = crate::preferences::state_root()?;
            std::fs::create_dir_all(&state)?;
            if state.canonicalize()?.starts_with(&root) {
                bail!(
                    "Microgentic state must be outside --dir; set XDG_STATE_HOME to another directory"
                );
            }
            tempfile::Builder::new()
                .prefix("run-")
                .tempdir_in(state)?
                .keep()
        };
        let file = tempfile::Builder::new()
            .prefix("prompt-")
            .suffix(suffix)
            .tempfile_in(&base)?;
        std::fs::write(file.path(), prompt)?;
        let (_, path) = file.keep()?;
        run.experiment = Some(base);
        if suffix == ".json" {
            run.contract = Some(path.clone());
        }
        run.goal = Some(path);
    }
    // Resolve relative management paths before changing the one process tool cwd.
    if let Some(experiment) = &mut run.experiment {
        *experiment = absolute(experiment)?;
    }
    if let Some(trace) = &mut run.trace_dir {
        *trace = absolute(trace)?;
    }
    std::env::set_current_dir(root)?;
    run.into_config()
}

fn setup(args: SetupArgs, explicit_path: Option<&Path>) -> Result<()> {
    let path = crate::preferences::config_path(explicit_path)?;
    let before = crate::preferences::load(&path)?;
    let mut prefs = before.clone().unwrap_or_default();
    let wizard = args.provider.is_none() && args.model.is_none() && args.reasoning_effort.is_none();
    if wizard && (!std::io::stdin().is_terminal() || !std::io::stdout().is_terminal()) {
        bail!(
            "setup needs settings or a terminal; try microgentic setup --provider ollama --model qwen3.8:27b-mxfp8 --reasoning-effort default --apply"
        );
    }
    let provider = if wizard {
        let value = ask("Provider (omlx/ollama)", prefs.provider.as_str())?;
        match value.as_str() {
            "omlx" => Provider::Omlx,
            "ollama" => Provider::Ollama,
            _ => bail!("provider must be omlx or ollama"),
        }
    } else {
        args.provider.unwrap_or(prefs.provider)
    };
    if provider != prefs.provider {
        prefs.model = provider.default_model().into();
        prefs.reasoning_effort = None;
    }
    prefs.provider = provider;
    prefs.model = if wizard {
        ask("Default model ID", &prefs.model)?
    } else {
        args.model.unwrap_or(prefs.model)
    };
    let effort = if wizard {
        Some(ask(
            "Default effort (default/disabled/enabled/low/medium/high/xhigh)",
            prefs.reasoning_effort.as_deref().unwrap_or("default"),
        )?)
    } else {
        args.reasoning_effort
    };
    if let Some(effort) = effort {
        prefs.reasoning_effort = (effort != "default").then_some(effort);
    }
    prefs.validate()?;
    if args.apply {
        crate::preferences::save(&path, &prefs)?;
    }
    if args.json {
        writeln!(
            std::io::stdout().lock(),
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"path":path,"before":before,"after":prefs,"applied":args.apply})
            )?
        )?;
    } else {
        println!(
            "{} {}",
            if args.apply {
                "Saved configuration:"
            } else {
                "Configuration preview:"
            },
            path.display()
        );
        println!(
            "Provider: {} → {}",
            before
                .as_ref()
                .map(|p| p.provider.as_str())
                .unwrap_or("unset"),
            prefs.provider.as_str()
        );
        println!(
            "Model: {} → {}",
            before.as_ref().map(|p| p.model.as_str()).unwrap_or("unset"),
            prefs.model
        );
        println!(
            "Effort: {} → {}",
            before
                .as_ref()
                .and_then(|p| p.reasoning_effort.as_deref())
                .unwrap_or("model default"),
            prefs.reasoning_effort.as_deref().unwrap_or("model default")
        );
        if !args.apply {
            println!(
                "Save these settings with:\n  microgentic --config {} setup --provider {} --model {} --reasoning-effort {} --apply",
                shell_quote(&path.to_string_lossy()),
                prefs.provider.as_str(),
                shell_quote(&prefs.model),
                prefs.reasoning_effort.as_deref().unwrap_or("default")
            );
        }
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn ask(label: &str, default: &str) -> Result<String> {
    eprint!("{label} [{default}]: ");
    std::io::stderr().flush()?;
    let mut text = String::new();
    if std::io::stdin().read_line(&mut text)? == 0 {
        bail!("setup input ended; configuration was not saved");
    }
    let text = text.trim();
    Ok(if text.is_empty() {
        default.to_string()
    } else {
        text.to_string()
    })
}

fn contract_source(goal: Option<PathBuf>, contract: Option<PathBuf>) -> Result<ContractSource> {
    match (goal, contract) {
        (Some(_), Some(_)) => {
            bail!("--goal and --contract are mutually exclusive; supply exactly one")
        }
        (None, None) => bail!("one of --goal or --contract is required"),
        (Some(goal), None) => {
            let goal_text = std::fs::read_to_string(&goal)
                .with_context(|| format!("reading goal file {}", goal.display()))?;
            Ok(ContractSource::Legacy {
                goal_path: goal.display().to_string(),
                goal_text,
            })
        }
        (None, Some(contract)) => {
            let json_text = std::fs::read_to_string(&contract)
                .with_context(|| format!("reading contract file {}", contract.display()))?;
            Ok(ContractSource::Explicit {
                source_path: Some(contract.display().to_string()),
                json_text,
            })
        }
    }
}
