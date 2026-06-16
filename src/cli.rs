use crate::agent::{
    AgentRunConfig, TranscriptPolicy, default_expected_output_tokens,
    default_max_thinking_only_tokens, default_repair_exit_thinking_tokens, run_coding_agent,
};
use crate::trace_analysis::analyze_trace;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

const DEFAULT_MODEL: &str = "qwen3.6:35b-a3b-coding-nvfp4";

#[derive(Debug, Parser)]
#[command(author, version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the Mojentic-backed coding agent against an experiment directory.
    Run {
        /// Experiment root. Tool access is scoped to this directory.
        #[arg(long)]
        experiment: PathBuf,

        /// Goal file, relative to the experiment root unless absolute.
        #[arg(long, default_value = "task.md")]
        goal: PathBuf,

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
    },

    /// Summarize context assembly and tool payload pressure from trace JSONL files.
    AnalyzeTrace {
        /// Trace JSONL files to analyze.
        #[arg(required = true)]
        traces: Vec<PathBuf>,
    },
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            experiment,
            goal,
            model,
            max_iterations,
            max_tool_iterations,
            context_window_tokens,
            packet_type,
            expected_output_tokens,
            num_predict,
            max_thinking_only_tokens,
            repair_exit_thinking_tokens,
            action_boundary_interrupt_tokens,
            transcript_policy,
        } => {
            let transcript_policy =
                TranscriptPolicy::parse(&transcript_policy).ok_or_else(|| {
                    anyhow::anyhow!(
                        "invalid transcript policy {transcript_policy:?}; expected one of \
                         full-transcript, summarized-transcript, summarized-repair-handoff, \
                         validation-repair-packet"
                    )
                })?;
            let expected_output_tokens = expected_output_tokens
                .unwrap_or_else(|| default_expected_output_tokens(&packet_type));
            let max_thinking_only_tokens = max_thinking_only_tokens.unwrap_or_else(|| {
                default_max_thinking_only_tokens(expected_output_tokens, num_predict)
            });
            let repair_exit_thinking_tokens =
                repair_exit_thinking_tokens.unwrap_or_else(default_repair_exit_thinking_tokens);
            let summary = run_coding_agent(AgentRunConfig {
                experiment_dir: experiment,
                goal_file: goal,
                model,
                max_iterations,
                max_tool_iterations,
                context_window_tokens,
                packet_type,
                expected_output_tokens,
                num_predict,
                max_thinking_only_tokens,
                repair_exit_thinking_tokens,
                action_boundary_interrupt_tokens,
                transcript_policy,
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
    }
    Ok(())
}
