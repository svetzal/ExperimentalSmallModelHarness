use adaptive_agent_harness::cli;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "adaptive_agent_harness=info".into()),
        )
        .init();

    cli::run().await
}
