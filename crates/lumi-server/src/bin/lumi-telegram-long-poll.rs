use anyhow::anyhow;
use lumi_server::{run_telegram_long_poll, AppConfig};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| anyhow!("failed to initialize tracing: {error}"))?;
    run_telegram_long_poll(&AppConfig::from_env()).await
}
