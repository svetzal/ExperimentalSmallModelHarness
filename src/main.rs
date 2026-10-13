use adaptive_agent_harness::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli::main_entry(false).await
}
