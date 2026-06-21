use std::net::SocketAddr;

use anyhow::{anyhow, Context};
use lumi_server::{build_router, shutdown_signal, AppConfig};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| anyhow!("failed to initialize tracing subscriber: {error}"))?;

    let config = AppConfig::from_env();
    let address: SocketAddr = config
        .bind_address()
        .parse()
        .with_context(|| format!("invalid LUMI_SERVER_BIND `{}`", config.bind_address()))?;

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind Lumi server to {address}"))?;

    tracing::info!(%address, "starting Lumi server");

    axum::serve(listener, build_router())
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Lumi server failed")?;

    Ok(())
}
