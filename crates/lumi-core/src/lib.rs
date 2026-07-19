#![deny(missing_docs)]
//! Shared platform-independent contracts for Lumi.
//!
//! The S1 slice keeps EPUB-specific work behind an importer boundary and gives
//! the server and web adapter one shared model for materials, revisions,
//! normalized content, reading documents, anchors, annotations and jobs.

mod auth;
mod epub;
mod fixtures;
mod models;
mod reader;
mod sources;

pub use auth::*;
pub use epub::*;
pub use fixtures::{
    import_epub_fixture, rich_epub_fixture, sample_fixture_highlight, simple_epub_fixture,
    EpubFixture, EpubFixtureResource, EpubFixtureSection, ImportError, ImportedFixture,
};
pub use models::*;
pub use reader::*;
pub use sources::*;

use serde::{Deserialize, Serialize};

/// Current public API version used by the local Axum scaffold.
pub const API_VERSION: &str = "v1";

/// Current domain schema marker for the S1 contracts.
pub const DOMAIN_SCHEMA_VERSION: &str = "s1.2026-07-13.sources-v2";

/// Current normalized content package marker for reflowable S1 documents.
pub const NORMALIZED_PACKAGE_VERSION: &str = "normalized.reflowable.s1";

const S0_DOMAIN_SCHEMA_VERSION: &str = "s0.2026-06-21";
const S0_NORMALIZED_PACKAGE_VERSION: &str = "normalized.reflowable.s0";

/// Importer id used by the S0 EPUB fixture importer spike.
pub const EPUB_FIXTURE_IMPORTER_ID: &str = "lumi.epub.fixture";

/// Importer version used by the S0 EPUB fixture importer spike.
pub const EPUB_FIXTURE_IMPORTER_VERSION: &str = "s0.1";

/// Importer id used by the real DRM-free EPUB pipeline.
pub const EPUB_IMPORTER_ID: &str = "lumi.epub";

/// Version of the deterministic real EPUB importer.
pub const EPUB_IMPORTER_VERSION: &str = "s1.3";

/// Importer id used by the baseline raw web snapshot pipeline.
pub const WEB_IMPORTER_ID: &str = "lumi.web.raw-snapshot";

/// Version of the deterministic baseline web extractor.
pub const WEB_IMPORTER_VERSION: &str = "s1.0";

/// Importer id used by the Telegram text normalizer.
pub const TELEGRAM_IMPORTER_ID: &str = "lumi.telegram.text";

/// Version of the deterministic Telegram text normalizer.
pub const TELEGRAM_IMPORTER_VERSION: &str = "s1.0";

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

/// Capabilities exposed by an S0-compatible server.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceCapabilities {
    /// API version served by the process.
    pub api_version: String,
    /// Domain schema version served by the process.
    pub domain_schema_version: String,
    /// Normalized package version accepted by the reader path.
    pub normalized_package_version: String,
    /// Route groups currently present behind `/api/v1`.
    pub route_groups: Vec<String>,
    /// Feature flags available in the current slice.
    pub features: Vec<String>,
}

impl ServiceCapabilities {
    /// Build the capabilities advertised by the S0 implementation.
    #[must_use]
    pub fn s0() -> Self {
        Self {
            api_version: API_VERSION.to_owned(),
            domain_schema_version: S0_DOMAIN_SCHEMA_VERSION.to_owned(),
            normalized_package_version: S0_NORMALIZED_PACKAGE_VERSION.to_owned(),
            route_groups: vec![
                "auth".to_owned(),
                "account".to_owned(),
                "materials".to_owned(),
                "revisions".to_owned(),
                "blobs".to_owned(),
                "imports".to_owned(),
                "jobs".to_owned(),
                "reader".to_owned(),
            ],
            features: vec![
                "seed-derived-ed25519-auth".to_owned(),
                "persistent-web-sessions".to_owned(),
                "account-scoped-routes".to_owned(),
                "sync-ready-postgresql".to_owned(),
                "content-addressed-local-dev-blobs".to_owned(),
                "real-epub-importer".to_owned(),
                "durable-import-jobs".to_owned(),
                "import-cancel-retry-recovery".to_owned(),
                "api-backed-library".to_owned(),
                "durable-library-lifecycle".to_owned(),
                "reading-document-reader-core".to_owned(),
                "anchor-backed-annotations".to_owned(),
            ],
        }
    }

    /// Build the capabilities advertised by the S1 web EPUB reader slice.
    #[must_use]
    pub fn s1() -> Self {
        Self {
            api_version: API_VERSION.to_owned(),
            domain_schema_version: DOMAIN_SCHEMA_VERSION.to_owned(),
            normalized_package_version: NORMALIZED_PACKAGE_VERSION.to_owned(),
            route_groups: vec![
                "auth".to_owned(),
                "account".to_owned(),
                "materials".to_owned(),
                "revisions".to_owned(),
                "blobs".to_owned(),
                "imports".to_owned(),
                "jobs".to_owned(),
                "reader".to_owned(),
                "exports".to_owned(),
            ],
            features: vec![
                "seed-derived-ed25519-auth".to_owned(),
                "persistent-web-sessions".to_owned(),
                "account-scoped-routes".to_owned(),
                "sync-ready-postgresql".to_owned(),
                "content-addressed-local-dev-blobs".to_owned(),
                "real-epub-importer".to_owned(),
                "durable-import-jobs".to_owned(),
                "api-backed-library".to_owned(),
                "durable-library-lifecycle".to_owned(),
                "reading-document-reader-core".to_owned(),
                "browser-measured-page-map".to_owned(),
                "typed-internal-links-footnotes".to_owned(),
                "durable-reader-settings-progress".to_owned(),
                "anchor-backed-annotations".to_owned(),
                "annotation-crud".to_owned(),
                "annotation-export".to_owned(),
                "library-archive-delete".to_owned(),
                "source-epub-download".to_owned(),
                "import-diagnostics".to_owned(),
                "public-web-url-import".to_owned(),
                "telegram-text-import".to_owned(),
                "telegram-one-time-pairing".to_owned(),
            ],
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

    #[test]
    fn capabilities_include_core_s0_route_groups() {
        let capabilities = ServiceCapabilities::s0();

        assert!(capabilities
            .route_groups
            .iter()
            .any(|group| group == "materials"));
    }
}
