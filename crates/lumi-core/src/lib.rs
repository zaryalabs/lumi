#![deny(missing_docs)]
//! Shared platform-independent contracts for Lumi.
//!
//! This crate is intentionally small at the scaffold stage. It establishes the
//! workspace boundary where reader, import, sync and API contracts can grow
//! without depending on Dioxus, DOM or Axum handler types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Current public API version used by the local Axum scaffold.
pub const API_VERSION: &str = "v1";

/// Stable user identifier type.
///
/// The target account model uses UUIDv7 or a newer time-ordered UUID variant.
pub type UserId = Uuid;

/// Stable material identifier type.
pub type MaterialId = Uuid;

/// Stable document revision identifier type.
pub type DocumentRevisionId = Uuid;

/// Health state for Lumi services.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    /// The service is reachable and ready for local development traffic.
    Ok,
}

/// Response returned by service health endpoints.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// Machine-readable service status.
    pub status: ServiceStatus,
    /// Service name that produced the response.
    pub service: String,
    /// Public API version for this response.
    pub api_version: String,
}

impl HealthResponse {
    /// Build a successful health response for `service`.
    #[must_use]
    pub fn ok(service: impl Into<String>) -> Self {
        Self {
            status: ServiceStatus::Ok,
            service: service.into(),
            api_version: API_VERSION.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_response_uses_current_api_version() {
        let response = HealthResponse::ok("lumi-server");

        assert_eq!(response.api_version, API_VERSION);
    }

    #[test]
    fn health_status_serializes_as_snake_case() -> Result<(), serde_json::Error> {
        let serialized = serde_json::to_string(&ServiceStatus::Ok)?;

        assert_eq!(serialized, "\"ok\"");
        Ok(())
    }
}
