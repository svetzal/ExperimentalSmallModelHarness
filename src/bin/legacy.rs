//! Temporary compatibility entry point for preserved scripts and fixtures.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    adaptive_agent_harness::cli::main_entry(true).await
}
