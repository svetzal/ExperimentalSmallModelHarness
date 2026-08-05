use crate::agent::{
    AgentRunConfig, TranscriptPolicy, default_expected_output_tokens,
    default_max_thinking_only_tokens, default_repair_exit_thinking_tokens, run_agent,
};
use crate::baseline::{load_matrix_baseline, summarize_matrix};
use crate::contract::{Budgets, ContractSource, resolve_contract};
use crate::retry::{SequentialRunConfig, run_sequential};
use crate::trace_analysis::analyze_trace;
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

const DEFAULT_MODEL: &str = "qwen3.6:35b-a3b-coding-nvfp4";

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// Experiment root. Tool access is scoped to this directory.
    #[arg(long)]
    experiment: PathBuf,

    /// Trace destination. Defaults to `<experiment>/traces`; may be placed
    /// outside the tool-visible experiment root by an external evaluator.
    #[arg(long)]
    trace_dir: Option<PathBuf>,

    /// Goal file, relative to the experiment root unless absolute.
    #[arg(long, default_value = "task.md")]
    goal: PathBuf,

    /// Explicit run-contract JSON file, relative to the experiment root unless absolute.
    #[arg(long)]
    contract: Option<PathBuf>,

    /// Ollama model name.
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,

    /// Maximum Mojentic iterative-solver turns.
    #[arg(long, default_value_t = 10)]
    max_iterations: usize,

    /// Maximum model-requested tool calls per model turn.
    #[arg(long, default_value_t = 50)]
    max_tool_iterations: usize,

    /// Approximate model context window in tokens. Enables utilization traces.
    #[arg(long)]
    context_window_tokens: Option<usize>,

    /// Packet type used for default output-token budgeting.
    #[arg(long, default_value = "multi-file-edit")]
    packet_type: String,

    /// Override the packet output-token budget.
    #[arg(long)]
    expected_output_tokens: Option<usize>,

    /// Override Ollama num_predict / maximum generated tokens.
    #[arg(long)]
    num_predict: Option<usize>,

    /// Max reasoning-only tokens in one LLM call before hard-stop. Defaults to max(expected output, 25% of num_predict); 0 disables.
    #[arg(long)]
    max_thinking_only_tokens: Option<usize>,

    /// Post-validation hidden-only reasoning tokens before interrupting and retrying repair. 0 disables.
    #[arg(long)]
    repair_exit_thinking_tokens: Option<usize>,

    /// Pre-validation hidden action-intent reasoning tokens before interrupting and forcing an action. 0 disables.
    #[arg(long, default_value_t = 0)]
    action_boundary_interrupt_tokens: usize,

    /// Transcript retention policy for model/tool context.
    #[arg(long, default_value = "summarized-transcript")]
    transcript_policy: String,
}

impl RunArgs {
    fn into_config(self) -> Result<AgentRunConfig> {
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
            default_max_thinking_only_tokens(expected_output_tokens, self.num_predict)
        });
        let repair_exit_thinking_tokens = self
            .repair_exit_thinking_tokens
            .unwrap_or_else(default_repair_exit_thinking_tokens);
        Ok(AgentRunConfig {
            experiment_dir: self.experiment,
            trace_dir: self.trace_dir,
            goal_file: self.goal,
            contract_file: self.contract,
            model: self.model,
            max_iterations: self.max_iterations,
            max_tool_iterations: self.max_tool_iterations,
            context_window_tokens: self.context_window_tokens,
            packet_type: self.packet_type,
            expected_output_tokens,
            num_predict: self.num_predict,
            max_thinking_only_tokens,
            repair_exit_thinking_tokens,
            action_boundary_interrupt_tokens: self.action_boundary_interrupt_tokens,
            transcript_policy,
        })
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Mojentic-backed agent selected by the resolved contract profile.
    Run(RunArgs),

    /// Run bounded natural attempts on one retained worker artifact.
    RunSequential {
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
    },

    /// Summarize context assembly and tool payload pressure from trace JSONL files.
    AnalyzeTrace {
        /// Trace JSONL files to analyze.
        #[arg(required = true)]
        traces: Vec<PathBuf>,
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
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run(run) => {
            let summary = run_agent(run.into_config()?).await?;
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::RunSequential {
            run,
            artifact,
            expected_artifact,
            max_attempts,
            max_failed_repair_cycles,
            max_consecutive_unchanged_attempts,
        } => {
            let summary = run_sequential(SequentialRunConfig {
                agent: run.into_config()?,
                artifact,
                expected_artifact,
                max_attempts,
                max_failed_repair_cycles,
                max_consecutive_unchanged_attempts,
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
        Command::SummarizeMatrix => {
            let baseline = load_matrix_baseline();
            let summary = summarize_matrix(&baseline.cells);
            println!("{}", serde_json::to_string_pretty(&summary)?);
        }
        Command::ResolveContract { goal, contract } => {
            let source = match (goal, contract) {
                (Some(_), Some(_)) => {
                    bail!("--goal and --contract are mutually exclusive; supply exactly one")
                }
                (None, None) => {
                    bail!("one of --goal or --contract is required")
                }
                (Some(goal), None) => {
                    let goal_text = std::fs::read_to_string(&goal)
                        .with_context(|| format!("reading goal file {}", goal.display()))?;
                    ContractSource::Legacy {
                        goal_path: goal.display().to_string(),
                        goal_text,
                    }
                }
                (None, Some(contract)) => {
                    let json_text = std::fs::read_to_string(&contract)
                        .with_context(|| format!("reading contract file {}", contract.display()))?;
                    ContractSource::Explicit {
                        source_path: Some(contract.display().to_string()),
                        json_text,
                    }
                }
            };
            let resolved = resolve_contract(source, Budgets::default())?;
            println!("{}", serde_json::to_string_pretty(&resolved)?);
        }
    }
    Ok(())
}
