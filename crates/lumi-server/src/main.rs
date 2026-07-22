use std::future::IntoFuture;
use std::net::SocketAddr;

use anyhow::{anyhow, Context};
use lumi_server::{build_router_with_state, shutdown_signal, AppConfig, AppState};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .map_err(|error| anyhow!("failed to initialize tracing subscriber: {error}"))?;

    let config = AppConfig::from_env();
    let state = AppState::persistent(&config)
        .await
        .context("failed to connect persistent account repository")?;
    let address: SocketAddr = config
        .bind_address()
        .parse()
        .with_context(|| format!("invalid LUMI_SERVER_BIND `{}`", config.bind_address()))?;

    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind Lumi server to {address}"))?;

    tracing::info!(%address, "starting Lumi server");

    let cancellation = CancellationToken::new();
    let server = axum::serve(listener, build_router_with_state(state.clone()))
        .with_graceful_shutdown(cancellation.clone().cancelled_owned())
        .into_future();
    let telegram = state.run_telegram(cancellation.clone());
    tokio::pin!(server);
    tokio::pin!(telegram);

    tokio::select! {
        result = &mut server => {
            cancellation.cancel();
            result.context("Lumi server failed")?;
            telegram.await;
        }
        () = &mut telegram => {
            cancellation.cancel();
            server.await.context("Lumi server failed")?;
            anyhow::bail!("embedded Telegram supervisor stopped unexpectedly");
        }
        () = shutdown_signal() => {
            cancellation.cancel();
            server.await.context("Lumi server failed")?;
            telegram.await;
        }
    }

    Ok(())
}
