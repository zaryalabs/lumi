#![deny(missing_docs)]
//! Axum API boundary for Lumi local development.
//!
//! Product routes will grow under `/api/v1`. Dioxus server functions may be
//! added later for narrow UI calls, but durable system contracts belong here.

use std::time::Duration;

use axum::{routing::get, Json, Router};
use lumi_core::HealthResponse;
use tower_http::trace::TraceLayer;

/// Default bind address for local development.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";

/// Runtime configuration for the Lumi server process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppConfig {
    bind_address: String,
}

impl AppConfig {
    /// Read server configuration from environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        let bind_address =
            std::env::var("LUMI_SERVER_BIND").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned());

        Self { bind_address }
    }

    /// Address the server should bind.
    #[must_use]
    pub fn bind_address(&self) -> &str {
        &self.bind_address
    }
}

/// Build the Axum router without binding a socket.
pub fn build_router() -> Router {
    let api = Router::new().route("/health", get(health));

    Router::new()
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::ok("lumi-server"))
}

/// Wait for an OS shutdown signal.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(%error, "failed to install Ctrl+C handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::warn!(%error, "failed to install terminate signal handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }

    tokio::time::sleep(Duration::from_millis(25)).await;
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn health_route_returns_ok() -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let request = Request::builder()
            .uri("/api/v1/health")
            .body(Body::empty())?;

        let response = app.oneshot(request).await?;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        Ok(())
    }

    #[test]
    fn config_uses_local_bind_address_by_default() {
        let config = AppConfig::from_env();

        assert!(!config.bind_address().is_empty());
    }
}
