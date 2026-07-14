#![deny(missing_docs)]
//! Axum API boundary for Lumi local development.
//!
//! Product routes will grow under `/api/v1`. Dioxus server functions may be
//! added later for narrow UI calls, but durable system contracts belong here.

mod account;
mod auth_api;
mod blob;
mod imports;
mod telegram;
mod web;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use axum::{
    body::{to_bytes, Body},
    extract::{DefaultBodyLimit, Multipart, Path, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
    Extension, Json, Router,
};
use lumi_core::{
    import_epub_fixture, rich_epub_fixture, s1_schema_migrations, simple_epub_fixture,
    AcceptedImport, Annotation, AnnotationExport, AnnotationId, BlobManifest, BlobManifestId,
    ContinueReadingEntry, CreateAnnotationCommand, DeleteAnnotationCommand, DiagnosticSeverity,
    DocumentRevision, DocumentRevisionId, EpubFixture, EpubLimits, HealthResponse,
    ImportDiagnostic, ImportStatusEntry, ImportWebUrlRequest, ImportedFixture, Job, JobId, JobKind,
    JobStage, JobStatus, LibraryEntry, LibraryState, Material, MaterialId, MaterialImportStatus,
    MoveReadingPositionCommand, NormalizedContentPackage, ReaderSettings, ReadingDocument,
    ReadingProgress, SchemaMigration, ServiceCapabilities, TelegramConnectionStatus,
    UpdateAnnotationCommand, UpdateLibraryStateCommand, UpdateReaderSettingsCommand, UserId,
};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    timeout::TimeoutLayer,
    trace::TraceLayer,
};

use account::{AccountStore, AuthenticatedSession, MemoryAccountStore, PgAccountStore};
use imports::{ImportService, ImportServiceError};
use telegram::{TelegramService, TelegramServiceError};

/// Default bind address for local development.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";
/// Default local PostgreSQL database URL.
pub const DEFAULT_DATABASE_URL: &str = "postgres://lumi:lumi-local@127.0.0.1:5432/lumi";
/// Default local web origin accepted by CORS and CSRF checks.
pub const DEFAULT_WEB_ORIGIN: &str = "http://127.0.0.1:5173";
/// Default content-addressed blob root for local development.
pub const DEFAULT_BLOB_ROOT: &str = ".local/blob-store";

const TELEGRAM_WEBHOOK_SECRET_HEADER: &str = "x-telegram-bot-api-secret-token";
const MAX_TELEGRAM_WEBHOOK_BODY_BYTES: usize = 256 * 1024;
const MAX_TELEGRAM_WEBHOOK_HEADERS: usize = 48;
const MAX_TELEGRAM_WEBHOOK_HEADER_BYTES: usize = 16 * 1024;
const TELEGRAM_WEBHOOK_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Eq, PartialEq)]
struct SecretString(String);

impl fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretString([redacted])")
    }
}

/// Runtime configuration for the Lumi server process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppConfig {
    bind_address: String,
    database_url: String,
    web_origin: String,
    auth_audience: String,
    secure_cookie: bool,
    blob_root: std::path::PathBuf,
    telegram_webhook_secret: Option<SecretString>,
    deployment_mode: String,
}

impl AppConfig {
    /// Read server configuration from environment variables.
    #[must_use]
    pub fn from_env() -> Self {
        let bind_address =
            std::env::var("LUMI_SERVER_BIND").unwrap_or_else(|_| DEFAULT_BIND_ADDRESS.to_owned());
        let database_url =
            std::env::var("DATABASE_URL").unwrap_or_else(|_| DEFAULT_DATABASE_URL.to_owned());
        let web_origin =
            std::env::var("LUMI_WEB_ORIGIN").unwrap_or_else(|_| DEFAULT_WEB_ORIGIN.to_owned());
        let auth_audience =
            std::env::var("LUMI_AUTH_AUDIENCE").unwrap_or_else(|_| web_origin.clone());
        let secure_cookie = std::env::var("LUMI_SECURE_COOKIE")
            .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
            .unwrap_or(false);
        let blob_root = std::env::var_os("LUMI_BLOB_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_BLOB_ROOT));
        let telegram_webhook_secret = std::env::var("LUMI_TELEGRAM_WEBHOOK_SECRET")
            .ok()
            .filter(|value| !value.is_empty())
            .map(SecretString);
        let deployment_mode =
            std::env::var("LUMI_DEPLOYMENT_MODE").unwrap_or_else(|_| "local".to_owned());

        Self {
            bind_address,
            database_url,
            web_origin,
            auth_audience,
            secure_cookie,
            blob_root,
            telegram_webhook_secret,
            deployment_mode,
        }
    }

    /// Address the server should bind.
    #[must_use]
    pub fn bind_address(&self) -> &str {
        &self.bind_address
    }

    /// PostgreSQL connection URL used by the durable repositories.
    #[must_use]
    pub fn database_url(&self) -> &str {
        &self.database_url
    }

    /// Web origin allowed to send browser API requests.
    #[must_use]
    pub fn web_origin(&self) -> &str {
        &self.web_origin
    }

    /// Filesystem root used by the local content-addressed blob backend.
    #[must_use]
    pub fn blob_root(&self) -> &std::path::Path {
        &self.blob_root
    }

    fn telegram_webhook_secret(&self) -> Option<&str> {
        self.telegram_webhook_secret
            .as_ref()
            .map(|secret| secret.0.as_str())
    }
}

#[derive(Clone, Debug)]
struct SecurityConfig {
    audience: String,
    allowed_origin: String,
    secure_cookie: bool,
}

impl SecurityConfig {
    fn from_app(config: &AppConfig) -> Self {
        Self {
            audience: config.auth_audience.clone(),
            allowed_origin: config.web_origin.clone(),
            secure_cookie: config.secure_cookie,
        }
    }

    fn local() -> Self {
        Self {
            audience: DEFAULT_WEB_ORIGIN.to_owned(),
            allowed_origin: DEFAULT_WEB_ORIGIN.to_owned(),
            secure_cookie: false,
        }
    }

    fn audience(&self) -> &str {
        &self.audience
    }

    fn allowed_origin(&self) -> &str {
        &self.allowed_origin
    }

    fn cookie_name(&self) -> &'static str {
        if self.secure_cookie {
            "__Host-lumi_session"
        } else {
            "lumi_session"
        }
    }

    fn session_cookie(&self, token: &str) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!(
            "{}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000{secure}",
            self.cookie_name()
        )
    }

    fn csrf_cookie(&self, token: &str) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!("lumi_csrf={token}; Path=/; SameSite=Lax; Max-Age=2592000{secure}")
    }

    fn expired_cookie(&self) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!(
            "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{secure}",
            self.cookie_name()
        )
    }

    fn expired_csrf_cookie(&self) -> String {
        let secure = if self.secure_cookie { "; Secure" } else { "" };
        format!("lumi_csrf=; Path=/; SameSite=Lax; Max-Age=0{secure}")
    }
}

/// Shared Axum application state.
#[derive(Clone)]
pub struct AppState {
    repository: Arc<RwLock<Repository>>,
    accounts: Arc<dyn AccountStore>,
    security: SecurityConfig,
    imports: Option<Arc<ImportService>>,
    telegram: Option<Arc<TelegramService>>,
    telegram_webhook_secret: Option<SecretString>,
}

impl AppState {
    /// Build a state object seeded with the S1 rich EPUB fixture.
    #[must_use]
    pub fn seeded() -> Self {
        let owner_id = UserId::now_v7();
        let accounts: Arc<dyn AccountStore> = Arc::new(MemoryAccountStore::seeded(owner_id));
        let fixture = rich_epub_fixture();
        match import_epub_fixture(owner_id, &fixture) {
            Ok(imported) => {
                Self::from_imported(imported, SourceDownload::from_fixture(&fixture), accounts)
            }
            Err(error) => {
                tracing::error!(%error, "failed to seed S1 fixture repository");
                Self::empty()
            }
        }
    }

    /// Build an empty state object.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            repository: Arc::new(RwLock::new(Repository::default())),
            accounts: Arc::new(MemoryAccountStore::empty()),
            security: SecurityConfig::local(),
            imports: None,
            telegram: None,
            telegram_webhook_secret: None,
        }
    }

    /// Connect the durable account repositories configured for the server.
    ///
    /// # Errors
    ///
    /// Returns an error when PostgreSQL is unavailable.
    pub async fn persistent(config: &AppConfig) -> anyhow::Result<Self> {
        Self::persistent_with_recovery(config, true).await
    }

    async fn persistent_with_recovery(
        config: &AppConfig,
        recover_imports: bool,
    ) -> anyhow::Result<Self> {
        validate_telegram_webhook_secret(config.telegram_webhook_secret())?;
        validate_deployment_security(config)?;
        tokio::fs::create_dir_all(config.blob_root())
            .await
            .map_err(|error| anyhow::anyhow!("failed to prepare blob root: {error}"))?;
        let accounts = PgAccountStore::connect(config.database_url())
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        let imports = Arc::new(ImportService::local(
            accounts.pool().clone(),
            config.blob_root().to_path_buf(),
        ));
        if recover_imports {
            imports
                .recover()
                .await
                .map_err(|error| anyhow::anyhow!(error))?;
        }
        let telegram = (config.deployment_mode == "local"
            || config.telegram_webhook_secret.is_some())
        .then(|| {
            Arc::new(TelegramService::from_env(
                accounts.pool().clone(),
                Arc::clone(&imports),
            ))
        });
        Ok(Self {
            repository: Arc::new(RwLock::new(Repository::default())),
            accounts: Arc::new(accounts),
            security: SecurityConfig::from_app(config),
            imports: Some(imports),
            telegram,
            telegram_webhook_secret: config.telegram_webhook_secret.clone(),
        })
    }

    fn from_imported(
        imported: ImportedFixture,
        source: SourceDownload,
        accounts: Arc<dyn AccountStore>,
    ) -> Self {
        let mut repository = Repository::default();
        repository.insert_imported_with_source(imported, source);

        Self {
            repository: Arc::new(RwLock::new(repository)),
            accounts,
            security: SecurityConfig::local(),
            imports: None,
            telegram: None,
            telegram_webhook_secret: None,
        }
    }

    fn accounts(&self) -> &dyn AccountStore {
        self.accounts.as_ref()
    }

    fn security(&self) -> &SecurityConfig {
        &self.security
    }

    fn imports(&self) -> Result<&Arc<ImportService>, AppError> {
        self.imports
            .as_ref()
            .ok_or(AppError::Unavailable("durable import service"))
    }

    fn telegram(&self) -> Result<&Arc<TelegramService>, AppError> {
        self.telegram
            .as_ref()
            .ok_or(AppError::Unavailable("Telegram provider service"))
    }
}

/// Build the Axum router without binding a socket.
pub fn build_router() -> Router {
    build_router_with_state(AppState::seeded())
}

/// Build the Axum router with an explicit state object.
pub fn build_router_with_state(state: AppState) -> Router {
    let public = Router::new()
        .route("/health", get(health))
        .route("/ready", get(readiness))
        .route("/capabilities", get(capabilities))
        .route("/schema/migrations", get(schema_migrations))
        .merge(auth_api::public_routes());
    let mut protected = Router::new()
        .merge(auth_api::protected_account_routes())
        .route("/materials", get(list_materials))
        .route("/materials/continue-reading", get(continue_reading))
        .route(
            "/materials/{material_id}",
            get(get_material).delete(delete_material),
        )
        .route(
            "/materials/{material_id}/library-state",
            patch(update_library_state).layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/materials/{material_id}/source", get(download_source_epub))
        .route(
            "/materials/{material_id}/annotations",
            get(list_annotations)
                .post(create_annotation)
                .layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route(
            "/materials/{material_id}/annotations/export",
            get(export_annotations),
        )
        .route(
            "/materials/{material_id}/annotations/{annotation_id}",
            put(update_annotation)
                .delete(delete_annotation)
                .layer(DefaultBodyLimit::max(512 * 1024)),
        )
        .route(
            "/materials/{material_id}/progress",
            get(get_progress)
                .put(move_reading_position)
                .layer(DefaultBodyLimit::max(256 * 1024)),
        )
        .route(
            "/reader/settings",
            get(get_reader_settings)
                .put(update_reader_settings)
                .layer(DefaultBodyLimit::max(64 * 1024)),
        )
        .route("/revisions/{revision_id}", get(get_revision))
        .route(
            "/revisions/{revision_id}/package",
            get(get_normalized_package),
        )
        .route(
            "/revisions/{revision_id}/reading-document",
            get(get_reading_document),
        )
        .route(
            "/revisions/{revision_id}/resources/{content_hash}",
            get(get_revision_resource),
        )
        .route("/blobs/{manifest_id}", get(get_blob_manifest))
        .route(
            "/imports/fixtures/{fixture_slug}",
            post(import_fixture_material),
        )
        .route("/imports", get(list_imports).post(upload_epub))
        .route(
            "/imports/url",
            post(import_web_url).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/jobs/{job_id}", get(get_job))
        .route("/jobs/{job_id}/diagnostics", get(get_job_diagnostics))
        .route("/jobs/{job_id}/cancel", post(cancel_job))
        .route("/jobs/{job_id}/retry", post(retry_job));
    if state.telegram.is_some() {
        protected = protected
            .route(
                "/providers/telegram/pairing",
                post(create_telegram_pairing).layer(DefaultBodyLimit::max(1024)),
            )
            .route(
                "/providers/telegram/connection",
                get(get_telegram_connection).delete(unlink_telegram),
            );
    }
    let protected = protected
        .layer(DefaultBodyLimit::max(101 * 1024 * 1024))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_api::require_session,
        ));
    let api = public.merge(protected).with_state(state.clone());
    let allowed_origin = state
        .security()
        .allowed_origin()
        .parse::<HeaderValue>()
        .unwrap_or_else(|_| HeaderValue::from_static(DEFAULT_WEB_ORIGIN));

    let mut router = Router::new().nest("/api/v1", api);
    if state.telegram_webhook_secret.is_some() {
        router = router.merge(
            Router::new()
                .route(
                    "/webhooks/telegram",
                    post(telegram_webhook)
                        .layer(DefaultBodyLimit::max(MAX_TELEGRAM_WEBHOOK_BODY_BYTES)),
                )
                .with_state(state.clone()),
        );
    }
    router
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::exact(allowed_origin))
                .allow_credentials(true)
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::ORIGIN,
                    header::HeaderName::from_static("x-lumi-csrf"),
                    header::HeaderName::from_static("idempotency-key"),
                ])
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::PATCH,
                    Method::DELETE,
                ]),
        )
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::SERVICE_UNAVAILABLE,
            Duration::from_secs(45),
        ))
        .layer(tower::limit::ConcurrencyLimitLayer::new(256))
        .layer(TraceLayer::new_for_http())
}

fn validate_telegram_webhook_secret(secret: Option<&str>) -> anyhow::Result<()> {
    if secret.is_some_and(|value| {
        !(32..=256).contains(&value.len())
            || value.bytes().any(|byte| !(0x21..=0x7e).contains(&byte))
    }) {
        anyhow::bail!(
            "LUMI_TELEGRAM_WEBHOOK_SECRET must contain 32 to 256 visible ASCII characters"
        );
    }
    Ok(())
}

fn validate_deployment_security(config: &AppConfig) -> anyhow::Result<()> {
    if !matches!(
        config.deployment_mode.as_str(),
        "local" | "local-container" | "staging" | "production"
    ) {
        anyhow::bail!("LUMI_DEPLOYMENT_MODE must be local, local-container, staging or production");
    }
    if matches!(config.deployment_mode.as_str(), "staging" | "production")
        && (!config.secure_cookie
            || !config.web_origin.starts_with("https://")
            || config.auth_audience != config.web_origin)
    {
        anyhow::bail!(
            "staging/production require HTTPS origin, matching auth audience and secure cookies"
        );
    }
    let bind_address = config
        .bind_address
        .parse::<std::net::SocketAddr>()
        .map_err(|_| anyhow::anyhow!("LUMI_SERVER_BIND must be a socket address"))?;
    if config.deployment_mode == "local" && !bind_address.ip().is_loopback() {
        anyhow::bail!("local deployment mode may bind loopback addresses only");
    }
    if config.deployment_mode == "local-container" {
        let origin = url::Url::parse(&config.web_origin)
            .map_err(|_| anyhow::anyhow!("local-container web origin must be a valid URL"))?;
        let canonical_origin = origin.port().map(|port| format!("http://127.0.0.1:{port}"));
        let canonical_loopback_origin = origin.scheme() == "http"
            && origin.host_str() == Some("127.0.0.1")
            && canonical_origin.as_deref() == Some(config.web_origin.as_str())
            && origin.path() == "/"
            && origin.query().is_none()
            && origin.fragment().is_none()
            && origin.username().is_empty()
            && origin.password().is_none();
        if !bind_address.ip().is_unspecified()
            || !canonical_loopback_origin
            || config.auth_audience != config.web_origin
            || config.secure_cookie
        {
            anyhow::bail!(
                "local-container requires a wildcard container bind, canonical http://127.0.0.1:<port> origin/audience and insecure local cookies"
            );
        }
    }
    Ok(())
}

fn webhook_secret_matches(expected: &str, supplied: &str) -> bool {
    let expected_hash = sha2::Sha256::digest(expected.as_bytes());
    let supplied_hash = sha2::Sha256::digest(supplied.as_bytes());
    expected_hash
        .iter()
        .zip(supplied_hash.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

async fn telegram_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    let header_bytes = headers
        .iter()
        .map(|(name, value)| name.as_str().len().saturating_add(value.as_bytes().len()))
        .sum::<usize>();
    if headers.len() > MAX_TELEGRAM_WEBHOOK_HEADERS
        || header_bytes > MAX_TELEGRAM_WEBHOOK_HEADER_BYTES
    {
        return Err(AppError::PayloadTooLarge);
    }
    let expected = state
        .telegram_webhook_secret
        .as_ref()
        .ok_or(AppError::NotFound("Telegram webhook"))?;
    let supplied = headers
        .get(TELEGRAM_WEBHOOK_SECRET_HEADER)
        .and_then(|value| value.to_str().ok())
        .ok_or(AppError::Forbidden("Telegram webhook secret is required"))?;
    if !webhook_secret_matches(&expected.0, supplied) {
        return Err(AppError::Forbidden("Telegram webhook secret is invalid"));
    }
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_none_or(|value| {
            value
                .split(';')
                .next()
                .is_none_or(|media_type| media_type.trim() != "application/json")
        })
    {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let service = state.telegram.clone();
    let outcome = tokio::time::timeout(TELEGRAM_WEBHOOK_TIMEOUT, async move {
        let Ok(payload) = to_bytes(body, MAX_TELEGRAM_WEBHOOK_BODY_BYTES).await else {
            return Ok(None);
        };
        let update = match telegram::parse_webhook_update(&payload) {
            Ok(Some(update)) => update,
            Ok(None) | Err(TelegramServiceError::InvalidUpdate) => return Ok(None),
            Err(error) => return Err(map_telegram_webhook_error(error)),
        };
        let service = service.ok_or(AppError::Unavailable("Telegram provider service"))?;
        match service.handle_update(&update).await {
            Ok(reply) => Ok(Some(reply)),
            Err(
                TelegramServiceError::InvalidUpdate
                | TelegramServiceError::UpdateConflict
                | TelegramServiceError::PairingConflict,
            ) => Ok(None),
            Err(error) => Err(map_telegram_webhook_error(error)),
        }
    })
    .await
    .map_err(|_| AppError::Unavailable("Telegram webhook processing"))??;

    Ok(outcome.map_or_else(
        || StatusCode::NO_CONTENT.into_response(),
        |reply| {
            Json(serde_json::json!({
                "method": "sendMessage",
                "chat_id": reply.chat_id,
                "text": reply.text,
            }))
            .into_response()
        },
    ))
}

/// Apply the forward-only SQLx migration set to a PostgreSQL database.
///
/// Production deployments should run this as a separate deploy step before
/// starting new application instances.
///
/// # Errors
///
/// Returns an error when PostgreSQL is unavailable or a migration fails.
pub async fn run_migrations(database_url: &str) -> anyhow::Result<()> {
    let store = PgAccountStore::connect(database_url)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    let migrations_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let migrator = sqlx_core::migrate::Migrator::new(migrations_path)
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    migrator
        .run(store.pool())
        .await
        .map_err(|error| anyhow::anyhow!(error))?;
    Ok(())
}

/// Run the local-development Telegram long-polling transport.
///
/// # Errors
///
/// Returns an error when configuration, PostgreSQL, Telegram transport or the
/// durable update handler is unavailable.
pub async fn run_telegram_long_poll(config: &AppConfig) -> anyhow::Result<()> {
    if config.deployment_mode != "local" {
        anyhow::bail!("Telegram long polling is restricted to local deployment mode");
    }
    let token = std::env::var("LUMI_TELEGRAM_BOT_TOKEN")
        .map_err(|_| anyhow::anyhow!("LUMI_TELEGRAM_BOT_TOKEN is required"))?;
    std::env::var("LUMI_TELEGRAM_BOT_SCOPE")
        .map_err(|_| anyhow::anyhow!("LUMI_TELEGRAM_BOT_SCOPE is required for the runner"))?;
    // Claim/lease fencing makes concurrent startup recovery safe: active jobs
    // keep their lease and queued/expired jobs are claimed by exactly one worker.
    let state = AppState::persistent_with_recovery(config, true).await?;
    let service = Arc::clone(
        state
            .telegram()
            .map_err(|_| anyhow::anyhow!("Telegram provider service is unavailable"))?,
    );
    telegram::run_long_poll(service, &token)
        .await
        .map_err(|error| anyhow::anyhow!(error))
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::ok("lumi-server"))
}

async fn readiness(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    state
        .accounts()
        .ready()
        .await
        .map_err(|_| AppError::Unavailable("account repository"))?;
    if let Some(imports) = state.imports.as_ref() {
        imports
            .ready()
            .await
            .map_err(|_| AppError::Unavailable("migration or blob dependency"))?;
    }
    Ok(Json(HealthResponse::ok("lumi-server")))
}

async fn capabilities(State(state): State<AppState>) -> Json<ServiceCapabilities> {
    let mut capabilities = ServiceCapabilities::s1();
    if state.telegram.is_none() {
        capabilities.features.retain(|feature| {
            !matches!(
                feature.as_str(),
                "telegram-text-import" | "telegram-one-time-pairing"
            )
        });
    }
    if state.telegram_webhook_secret.is_some() {
        capabilities
            .route_groups
            .push("webhooks/telegram".to_owned());
        capabilities.features.push("telegram-webhook".to_owned());
    }
    Json(capabilities)
}

async fn schema_migrations() -> Json<Vec<SchemaMigration>> {
    Json(s1_schema_migrations())
}

async fn list_materials(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<LibraryEntry>>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .list(session.user_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    let mut materials = repository
        .materials
        .values()
        .filter(|material| {
            material.owner_id == session.user_id && material.library_state != LibraryState::Deleted
        })
        .map(|material| fixture_library_entry(&repository, material))
        .collect::<Result<Vec<_>, _>>()?;
    materials.sort_by(|left, right| left.canonical_title.cmp(&right.canonical_title));

    Ok(Json(materials))
}

async fn continue_reading(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<Option<ContinueReadingEntry>>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .continue_reading(session.user_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    let projection = repository
        .progress_by_material
        .values()
        .filter(|progress| progress.progress_fraction > 0.0)
        .filter_map(|progress| {
            let material = repository.materials.get(&progress.material_id)?;
            let entry = fixture_library_entry(&repository, material).ok()?;
            (material.owner_id == session.user_id
                && material.library_state == LibraryState::Active
                && progress.revision_id == material.active_revision_id
                && entry.import_status == MaterialImportStatus::Ready)
                .then_some((entry, progress))
        })
        .max_by_key(|(entry, progress)| (progress.updated_at, entry.id))
        .map(|(entry, progress)| {
            Ok(ContinueReadingEntry {
                entry,
                progress: progress.clone(),
            })
        })
        .transpose()?;
    Ok(Json(projection))
}

async fn get_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<LibraryEntry>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .material(session.user_id, material_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    let material = repository
        .material_owned_by(session.user_id, material_id)
        .and_then(|material| fixture_library_entry(&repository, material))?;

    Ok(Json(material))
}

async fn update_library_state(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    headers: HeaderMap,
    Json(command): Json<UpdateLibraryStateCommand>,
) -> Result<Json<LibraryEntry>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
    }
    let idempotency_key = required_idempotency_key(&headers)?;

    if let Some(imports) = state.imports.as_ref() {
        return imports
            .update_library_state(
                &session,
                material_id,
                command.library_state,
                idempotency_key,
            )
            .await
            .map(Json)
            .map_err(map_import_error);
    }

    let mut repository = write_repository(&state)?;
    {
        let material = repository
            .materials
            .get_mut(&material_id)
            .filter(|material| material.owner_id == session.user_id)
            .ok_or(AppError::NotFound("material"))?;
        material.library_state = command.library_state;
    }
    let material = repository
        .materials
        .get(&material_id)
        .ok_or(AppError::NotFound("material"))?;

    Ok(Json(fixture_library_entry(&repository, material)?))
}

async fn delete_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        let idempotency_key = required_idempotency_key(&headers)?;
        imports
            .delete_material(&session, material_id, idempotency_key)
            .await
            .map_err(map_import_error)?;
        return Ok(StatusCode::NO_CONTENT);
    }
    let mut repository = write_repository(&state)?;
    let material = repository
        .materials
        .get_mut(&material_id)
        .filter(|material| material.owner_id == session.user_id)
        .ok_or(AppError::NotFound("material"))?;
    material.library_state = LibraryState::Deleted;

    Ok(StatusCode::NO_CONTENT)
}

async fn download_source_epub(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Response, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        let (file_name, media_type, bytes) = imports
            .source(session.user_id, material_id)
            .await
            .map_err(map_import_error)?;
        return source_download_response(file_name, media_type, bytes);
    }
    let repository = read_repository(&state)?;
    repository.ensure_material_owned(session.user_id, material_id)?;
    let source = repository
        .source_downloads
        .get(&material_id)
        .cloned()
        .ok_or(AppError::NotFound("source_epub"))?;
    source_download_response(source.file_name, source.media_type, source.bytes)
}

async fn get_revision(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(revision_id): Path<DocumentRevisionId>,
) -> Result<Json<DocumentRevision>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .revision(session.user_id, revision_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    repository.ensure_revision_owned(session.user_id, revision_id)?;
    let revision = repository
        .revisions
        .get(&revision_id)
        .cloned()
        .ok_or(AppError::NotFound("revision"))?;

    Ok(Json(revision))
}

async fn get_normalized_package(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(revision_id): Path<DocumentRevisionId>,
) -> Result<Json<NormalizedContentPackage>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .package(session.user_id, revision_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    repository.ensure_revision_owned(session.user_id, revision_id)?;
    let package = repository
        .packages_by_revision
        .get(&revision_id)
        .cloned()
        .ok_or(AppError::NotFound("normalized_package"))?;

    Ok(Json(package))
}

async fn get_reading_document(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(revision_id): Path<DocumentRevisionId>,
) -> Result<Json<ReadingDocument>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .reading_document(session.user_id, revision_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    repository.ensure_revision_owned(session.user_id, revision_id)?;
    let document = repository
        .reading_documents_by_revision
        .get(&revision_id)
        .cloned()
        .ok_or(AppError::NotFound("reading_document"))?;

    Ok(Json(document))
}

async fn get_revision_resource(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((revision_id, content_hash)): Path<(DocumentRevisionId, String)>,
) -> Result<Response, AppError> {
    let imports = state.imports()?;
    let (media_type, bytes) = imports
        .resource(session.user_id, revision_id, &content_hash)
        .await
        .map_err(map_import_error)?;
    let content_type = HeaderValue::from_str(&media_type)
        .map_err(|_| AppError::Internal("stored resource media type is invalid"))?;
    Ok(([(header::CONTENT_TYPE, content_type)], bytes).into_response())
}

async fn get_blob_manifest(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(manifest_id): Path<BlobManifestId>,
) -> Result<Json<BlobManifest>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .manifest(session.user_id, manifest_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    repository.ensure_manifest_owned(session.user_id, manifest_id)?;
    let manifest = repository
        .blob_manifests
        .get(&manifest_id)
        .cloned()
        .ok_or(AppError::NotFound("blob_manifest"))?;

    Ok(Json(manifest))
}

async fn import_fixture_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(fixture_slug): Path<String>,
) -> Result<Json<ImportFixtureResponse>, AppError> {
    let fixture = match fixture_slug.as_str() {
        "simple" | "simple-epub" => simple_epub_fixture(),
        "rich" | "rich-epub" => rich_epub_fixture(),
        "empty" | "bad-empty" => empty_epub_fixture(),
        _ => return Err(AppError::BadRequest("unknown fixture slug".to_owned())),
    };
    let owner_id = session.user_id;
    let imported = match import_epub_fixture(owner_id, &fixture) {
        Ok(imported) => imported,
        Err(error) => {
            let job = failed_import_job(owner_id, error.to_string());
            let response = ImportFixtureResponse {
                material: None,
                revision: None,
                job: job.clone(),
            };
            let mut repository = write_repository(&state)?;
            repository.jobs.insert(job.id, job);

            return Ok(Json(response));
        }
    };
    let response = ImportFixtureResponse {
        material: Some(imported.material.clone()),
        revision: Some(imported.revision.clone()),
        job: imported.job.clone(),
    };
    let mut repository = write_repository(&state)?;
    repository.insert_imported_with_source(imported, SourceDownload::from_fixture(&fixture));

    Ok(Json(response))
}

async fn list_imports(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<ImportStatusEntry>>, AppError> {
    state
        .imports()?
        .list(session.user_id)
        .await
        .map(Json)
        .map_err(map_import_error)
}

async fn upload_epub(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let imports = state.imports()?;
    let _upload_admission = imports
        .try_begin_upload(session.user_id)
        .map_err(map_import_error)?;
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::BadRequest("Idempotency-Key header is required".to_owned()))?;
    let mut upload = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::BadRequest("invalid multipart upload".to_owned()))?
    {
        if field.name() != Some("file") || upload.is_some() {
            continue;
        }
        let file_name = field
            .file_name()
            .ok_or_else(|| AppError::BadRequest("EPUB file name is required".to_owned()))?
            .to_owned();
        let mut bytes = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|_| AppError::BadRequest("failed to read multipart upload".to_owned()))?
        {
            if bytes.len().saturating_add(chunk.len()) > EpubLimits::s1().source_bytes as usize {
                return Err(AppError::PayloadTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        upload = Some((file_name, bytes));
    }
    let (file_name, bytes) = upload
        .ok_or_else(|| AppError::BadRequest("multipart field `file` is required".to_owned()))?;
    let accepted: AcceptedImport = imports
        .accept(&session, &file_name, idempotency_key, bytes)
        .await
        .map_err(map_import_error)?;
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn import_web_url(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    Json(command): Json<ImportWebUrlRequest>,
) -> Result<Response, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    let accepted = state
        .imports()?
        .accept_web(&session, &command.url, idempotency_key)
        .await
        .map_err(map_import_error)?;
    Ok((StatusCode::ACCEPTED, Json(accepted)).into_response())
}

async fn create_telegram_pairing(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Response, AppError> {
    let response = state
        .telegram()?
        .create_pairing(&session)
        .await
        .map_err(map_telegram_error)?;
    Ok((
        StatusCode::CREATED,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(response),
    )
        .into_response())
}

async fn get_telegram_connection(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<TelegramConnectionStatus>, AppError> {
    state
        .telegram()?
        .status(session.user_id)
        .await
        .map(Json)
        .map_err(map_telegram_error)
}

async fn unlink_telegram(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<StatusCode, AppError> {
    state
        .telegram()?
        .unlink_account(session.user_id)
        .await
        .map_err(map_telegram_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_job(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Job>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .job(session.user_id, job_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    let job = repository
        .jobs
        .get(&job_id)
        .filter(|job| job.account_id == session.user_id)
        .cloned()
        .ok_or(AppError::NotFound("job"))?;

    Ok(Json(job))
}

async fn get_job_diagnostics(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Vec<ImportDiagnostic>>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .diagnostics(session.user_id, job_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    let diagnostics = repository
        .jobs
        .get(&job_id)
        .filter(|job| job.account_id == session.user_id)
        .map(|job| job.diagnostics.clone())
        .ok_or(AppError::NotFound("job"))?;

    Ok(Json(diagnostics))
}

async fn cancel_job(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Job>, AppError> {
    state
        .imports()?
        .cancel(session.user_id, job_id)
        .await
        .map(Json)
        .map_err(map_import_error)
}

async fn retry_job(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Job>, AppError> {
    state
        .imports()?
        .retry(session.user_id, job_id)
        .await
        .map(Json)
        .map_err(map_import_error)
}

async fn list_annotations(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<Vec<Annotation>>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .annotations(session.user_id, material_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    repository.ensure_material_owned(session.user_id, material_id)?;
    let annotations = repository
        .annotations_by_material
        .get(&material_id)
        .cloned()
        .unwrap_or_default();

    Ok(Json(annotations))
}

async fn create_annotation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    headers: HeaderMap,
    Json(command): Json<CreateAnnotationCommand>,
) -> Result<Json<Annotation>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
    }
    let idempotency_key = required_idempotency_key(&headers)?;

    if let Some(imports) = state.imports.as_ref() {
        return imports
            .create_annotation(&session, command, idempotency_key)
            .await
            .map(Json)
            .map_err(map_import_error);
    }

    let mut repository = write_repository(&state)?;
    repository.ensure_material_owned(session.user_id, command.material_id)?;
    repository.ensure_revision_owned(session.user_id, command.revision_id)?;

    let annotation = Annotation::create(command, lumi_core::now_timestamp_ms());
    repository
        .annotations_by_material
        .entry(material_id)
        .or_default()
        .push(annotation.clone());

    Ok(Json(annotation))
}

async fn update_annotation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((material_id, annotation_id)): Path<(MaterialId, AnnotationId)>,
    headers: HeaderMap,
    Json(command): Json<UpdateAnnotationCommand>,
) -> Result<Json<Annotation>, AppError> {
    if command.material_id != material_id || command.annotation_id != annotation_id {
        return Err(AppError::BadRequest(
            "material or annotation id in path and body must match".to_owned(),
        ));
    }
    let idempotency_key = required_idempotency_key(&headers)?;

    if let Some(imports) = state.imports.as_ref() {
        return imports
            .update_annotation(&session, command, idempotency_key)
            .await
            .map(Json)
            .map_err(map_import_error);
    }

    let mut repository = write_repository(&state)?;
    repository.ensure_material_owned(session.user_id, material_id)?;
    let annotations = repository
        .annotations_by_material
        .get_mut(&material_id)
        .ok_or(AppError::NotFound("annotation"))?;
    let annotation = annotations
        .iter_mut()
        .find(|stored| stored.id == annotation_id)
        .ok_or(AppError::NotFound("annotation"))?;

    if annotation.revision != command.expected_revision {
        return Err(AppError::Conflict(format!(
            "annotation revision conflict: expected {}, found {}",
            command.expected_revision, annotation.revision
        )));
    }

    annotation.update_kind(command.kind, lumi_core::now_timestamp_ms());

    Ok(Json(annotation.clone()))
}

async fn delete_annotation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((material_id, annotation_id)): Path<(MaterialId, AnnotationId)>,
    headers: HeaderMap,
    Json(command): Json<DeleteAnnotationCommand>,
) -> Result<Json<Annotation>, AppError> {
    if command.material_id != material_id || command.annotation_id != annotation_id {
        return Err(AppError::BadRequest(
            "material or annotation id in path and body must match".to_owned(),
        ));
    }
    let idempotency_key = required_idempotency_key(&headers)?;
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .delete_annotation(&session, command, idempotency_key)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let mut repository = write_repository(&state)?;
    repository.ensure_material_owned(session.user_id, material_id)?;
    let annotations = repository
        .annotations_by_material
        .get_mut(&material_id)
        .ok_or(AppError::NotFound("annotation"))?;
    let index = annotations
        .iter()
        .position(|stored| stored.id == annotation_id)
        .ok_or(AppError::NotFound("annotation"))?;

    if annotations[index].revision != command.expected_revision {
        return Err(AppError::Conflict(format!(
            "annotation revision conflict: expected {}, found {}",
            command.expected_revision, annotations[index].revision
        )));
    }

    Ok(Json(annotations.remove(index)))
}

async fn export_annotations(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Response, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        let export = imports
            .export_annotations(session.user_id, material_id)
            .await
            .map_err(map_import_error)?;
        return annotation_export_response(export);
    }
    let repository = read_repository(&state)?;
    let material = repository.material_owned_by(session.user_id, material_id)?;
    let annotations = repository
        .annotations_by_material
        .get(&material_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    annotation_export_response(AnnotationExport::for_material(material, annotations))
}

fn annotation_export_response(export: AnnotationExport) -> Result<Response, AppError> {
    let file_name = format!("lumi-annotations-{}.json", export.material_id);
    let mut response = Json(export).into_response();
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{file_name}\""))
            .map_err(|_| AppError::Internal("invalid annotation export filename"))?,
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    response.headers_mut().insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    Ok(response)
}

async fn get_progress(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<Option<ReadingProgress>>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .reading_progress(session.user_id, material_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let repository = read_repository(&state)?;
    repository.ensure_material_owned(session.user_id, material_id)?;

    Ok(Json(
        repository.progress_by_material.get(&material_id).cloned(),
    ))
}

async fn move_reading_position(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    headers: HeaderMap,
    Json(command): Json<MoveReadingPositionCommand>,
) -> Result<Json<ReadingProgress>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
    }

    if let Some(imports) = state.imports.as_ref() {
        let idempotency_key = required_idempotency_key(&headers)?;
        return imports
            .move_reading_position(&session, command, idempotency_key)
            .await
            .map(Json)
            .map_err(map_import_error);
    }

    let mut repository = write_repository(&state)?;
    repository.ensure_material_owned(session.user_id, command.material_id)?;
    repository.ensure_revision_owned(session.user_id, command.revision_id)?;
    let progress = ReadingProgress {
        material_id,
        revision_id: command.revision_id,
        locator: command.locator,
        progress_fraction: normalized_progress_fraction(command.progress_fraction),
        updated_at: lumi_core::now_timestamp_ms(),
    };
    repository
        .progress_by_material
        .insert(material_id, progress.clone());

    Ok(Json(progress))
}

async fn get_reader_settings(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<ReaderSettings>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        return imports
            .reader_settings(session.user_id)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    Ok(Json(read_repository(&state)?.reader_settings))
}

async fn update_reader_settings(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    Json(command): Json<UpdateReaderSettingsCommand>,
) -> Result<Json<ReaderSettings>, AppError> {
    if let Some(imports) = state.imports.as_ref() {
        let idempotency_key = required_idempotency_key(&headers)?;
        return imports
            .update_reader_settings(&session, command.settings, idempotency_key)
            .await
            .map(Json)
            .map_err(map_import_error);
    }
    let settings = command.settings.normalized();
    write_repository(&state)?.reader_settings = settings;
    Ok(Json(settings))
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

fn normalized_progress_fraction(progress_fraction: f32) -> f32 {
    if progress_fraction.is_finite() {
        progress_fraction.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn source_download_response(
    file_name: String,
    media_type: String,
    bytes: Vec<u8>,
) -> Result<Response, AppError> {
    let safe_name = file_name
        .chars()
        .filter(|character| !matches!(character, '"' | '\\'))
        .collect::<String>();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, media_type)
        .header(header::CACHE_CONTROL, "private, no-store")
        .header("x-content-type-options", "nosniff")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{safe_name}\""),
        )
        .body(Body::from(bytes))
        .map_err(|_| AppError::Internal("failed to build source response"))
}

fn fixture_library_entry(
    repository: &Repository,
    material: &Material,
) -> Result<LibraryEntry, AppError> {
    let latest_job = repository
        .jobs
        .values()
        .filter(|job| job.material_id == Some(material.id))
        .max_by_key(|job| job.updated_at)
        .cloned()
        .ok_or(AppError::NotFound("material import job"))?;
    let import_status = match latest_job.status {
        JobStatus::Queued => MaterialImportStatus::Queued,
        JobStatus::Running => MaterialImportStatus::Importing,
        JobStatus::Succeeded => MaterialImportStatus::Ready,
        JobStatus::Failed => MaterialImportStatus::Failed,
        JobStatus::Cancelled => MaterialImportStatus::Cancelled,
    };
    Ok(LibraryEntry {
        id: material.id,
        owner_id: material.owner_id,
        kind: material.kind.clone(),
        canonical_title: material.canonical_title.clone(),
        title_override: material.title_override.clone(),
        active_revision_id: Some(material.active_revision_id),
        library_state: material.library_state,
        source_identity: material.source_identity.clone(),
        import_status,
        updated_at: latest_job.updated_at,
        latest_job,
        created_at: material.created_at,
    })
}

fn required_idempotency_key(headers: &HeaderMap) -> Result<&str, AppError> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| AppError::BadRequest("Idempotency-Key header is required".to_owned()))
}

fn map_import_error(error: ImportServiceError) -> AppError {
    match error {
        ImportServiceError::NotFound => AppError::NotFound("import object"),
        ImportServiceError::Conflict => AppError::Conflict("import command conflicts".to_owned()),
        ImportServiceError::BadRequest(detail) => AppError::BadRequest(detail.to_owned()),
        ImportServiceError::RateLimited => AppError::TooManyRequests(
            "Too many queued imports for this account; wait for current imports to finish.",
        ),
        ImportServiceError::Unavailable => AppError::Unavailable("durable import service"),
    }
}

fn map_telegram_error(error: TelegramServiceError) -> AppError {
    match error {
        TelegramServiceError::InvalidUpdate => AppError::BadRequest(error.to_string()),
        TelegramServiceError::UpdateConflict
        | TelegramServiceError::UpdateInProgress
        | TelegramServiceError::PairingConflict => AppError::Conflict(error.to_string()),
        TelegramServiceError::Unavailable => AppError::Unavailable("Telegram provider service"),
    }
}

fn map_telegram_webhook_error(error: TelegramServiceError) -> AppError {
    match error {
        TelegramServiceError::InvalidUpdate => AppError::BadRequest(error.to_string()),
        TelegramServiceError::UpdateConflict | TelegramServiceError::PairingConflict => {
            AppError::Conflict(error.to_string())
        }
        TelegramServiceError::UpdateInProgress | TelegramServiceError::Unavailable => {
            AppError::Unavailable("Telegram webhook processing")
        }
    }
}

fn read_repository(state: &AppState) -> Result<RwLockReadGuard<'_, Repository>, AppError> {
    state
        .repository
        .read()
        .map_err(|_| AppError::Internal("repository lock poisoned"))
}

fn write_repository(state: &AppState) -> Result<RwLockWriteGuard<'_, Repository>, AppError> {
    state
        .repository
        .write()
        .map_err(|_| AppError::Internal("repository lock poisoned"))
}

#[derive(Default)]
struct Repository {
    materials: HashMap<MaterialId, Material>,
    revisions: HashMap<DocumentRevisionId, DocumentRevision>,
    packages_by_revision: HashMap<DocumentRevisionId, NormalizedContentPackage>,
    reading_documents_by_revision: HashMap<DocumentRevisionId, ReadingDocument>,
    blob_manifests: HashMap<BlobManifestId, BlobManifest>,
    source_downloads: HashMap<MaterialId, SourceDownload>,
    annotations_by_material: HashMap<MaterialId, Vec<Annotation>>,
    progress_by_material: HashMap<MaterialId, ReadingProgress>,
    reader_settings: ReaderSettings,
    jobs: HashMap<JobId, Job>,
}

impl Repository {
    fn insert_imported_with_source(&mut self, imported: ImportedFixture, source: SourceDownload) {
        let material_id = imported.material.id;
        self.blob_manifests.insert(
            imported.package.resources.id,
            imported.package.resources.clone(),
        );
        self.source_downloads.insert(material_id, source);
        self.reading_documents_by_revision
            .insert(imported.revision.id, imported.reading_document);
        self.packages_by_revision
            .insert(imported.revision.id, imported.package);
        self.materials
            .insert(imported.material.id, imported.material);
        self.revisions
            .insert(imported.revision.id, imported.revision);
        self.jobs.insert(imported.job.id, imported.job);
    }

    fn material_owned_by(
        &self,
        owner_id: UserId,
        material_id: MaterialId,
    ) -> Result<&Material, AppError> {
        self.materials
            .get(&material_id)
            .filter(|material| material.owner_id == owner_id)
            .ok_or(AppError::NotFound("material"))
    }

    fn ensure_material_owned(
        &self,
        owner_id: UserId,
        material_id: MaterialId,
    ) -> Result<(), AppError> {
        self.material_owned_by(owner_id, material_id).map(|_| ())
    }

    fn ensure_revision_owned(
        &self,
        owner_id: UserId,
        revision_id: DocumentRevisionId,
    ) -> Result<(), AppError> {
        let owned = self.revisions.contains_key(&revision_id)
            && self.materials.values().any(|material| {
                material.owner_id == owner_id && material.active_revision_id == revision_id
            });
        if owned {
            Ok(())
        } else {
            Err(AppError::NotFound("revision"))
        }
    }

    fn ensure_manifest_owned(
        &self,
        owner_id: UserId,
        manifest_id: BlobManifestId,
    ) -> Result<(), AppError> {
        let revision_id = self
            .packages_by_revision
            .iter()
            .find_map(|(revision_id, package)| {
                (package.resources.id == manifest_id).then_some(*revision_id)
            })
            .ok_or(AppError::NotFound("blob_manifest"))?;
        self.ensure_revision_owned(owner_id, revision_id)
    }
}

#[derive(Clone)]
struct SourceDownload {
    file_name: String,
    media_type: String,
    bytes: Vec<u8>,
}

impl SourceDownload {
    fn from_fixture(fixture: &EpubFixture) -> Self {
        Self {
            file_name: fixture.file_name.clone(),
            media_type: "application/epub+zip".to_owned(),
            bytes: fixture.source_bytes(),
        }
    }
}

fn empty_epub_fixture() -> EpubFixture {
    EpubFixture {
        slug: "empty-epub".to_owned(),
        file_name: "empty.epub".to_owned(),
        title: "Empty EPUB".to_owned(),
        creators: Vec::new(),
        language: Some("en".to_owned()),
        sections: Vec::new(),
        resources: Vec::new(),
    }
}

fn failed_import_job(account_id: UserId, detail: String) -> Job {
    let timestamp = lumi_core::now_timestamp_ms();

    Job {
        id: JobId::now_v7(),
        account_id,
        kind: JobKind::Import,
        status: JobStatus::Failed,
        stage: JobStage::SourceAccepted,
        material_id: None,
        revision_id: None,
        diagnostics: vec![ImportDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "epub_fixture_import_failed".to_owned(),
            message: detail,
            source_path: Some("empty.epub".to_owned()),
        }],
        created_at: timestamp,
        updated_at: timestamp,
    }
}

#[derive(Debug)]
enum AppError {
    NotFound(&'static str),
    BadRequest(String),
    Unauthorized,
    Forbidden(&'static str),
    Conflict(String),
    TooManyRequests(&'static str),
    PayloadTooLarge,
    Unavailable(&'static str),
    Internal(&'static str),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, title, detail) = match self {
            AppError::NotFound(resource) => (
                StatusCode::NOT_FOUND,
                "not_found",
                format!("Requested {resource} was not found."),
            ),
            AppError::BadRequest(detail) => (StatusCode::BAD_REQUEST, "bad_request", detail),
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "A valid Lumi session is required.".to_owned(),
            ),
            AppError::Forbidden(detail) => (StatusCode::FORBIDDEN, "forbidden", detail.to_owned()),
            AppError::Conflict(detail) => (StatusCode::CONFLICT, "conflict", detail),
            AppError::TooManyRequests(detail) => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_requests",
                detail.to_owned(),
            ),
            AppError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "EPUB upload exceeds the configured source limit.".to_owned(),
            ),
            AppError::Unavailable(resource) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "service_unavailable",
                format!("The {resource} is unavailable."),
            ),
            AppError::Internal(detail) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal_server_error",
                detail.to_owned(),
            ),
        };
        let problem = ProblemDetails {
            problem_type: "about:blank".to_owned(),
            title: title.to_owned(),
            status: status.as_u16(),
            detail,
        };

        (
            status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            Json(problem),
        )
            .into_response()
    }
}

#[derive(Serialize)]
struct ProblemDetails {
    #[serde(rename = "type")]
    problem_type: String,
    title: String,
    status: u16,
    detail: String,
}

#[derive(Serialize, Deserialize)]
struct ImportFixtureResponse {
    material: Option<Material>,
    revision: Option<DocumentRevision>,
    job: Job,
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Request};
    use lumi_core::{
        sample_fixture_highlight, AnnotationKind, HighlightStyle, ImportedFixture, WebAccount,
    };
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

    #[test]
    fn deployment_security_rejects_public_host_local_bind() {
        let mut local = AppConfig::from_env();
        local.deployment_mode = "local".to_owned();
        local.bind_address = "0.0.0.0:8080".to_owned();

        assert!(validate_deployment_security(&local).is_err());
    }

    #[test]
    fn deployment_security_accepts_canonical_local_container_boundary() {
        let mut local_container = AppConfig::from_env();
        local_container.deployment_mode = "local-container".to_owned();
        local_container.bind_address = "0.0.0.0:8080".to_owned();
        local_container.web_origin = "http://127.0.0.1:5173".to_owned();
        local_container.auth_audience = local_container.web_origin.clone();
        local_container.secure_cookie = false;

        assert!(validate_deployment_security(&local_container).is_ok());
    }

    #[test]
    fn deployment_security_rejects_noncanonical_local_container_origin() {
        let mut local_container = AppConfig::from_env();
        local_container.deployment_mode = "local-container".to_owned();
        local_container.bind_address = "0.0.0.0:8080".to_owned();
        local_container.web_origin = "http://localhost:5173".to_owned();
        local_container.auth_audience = local_container.web_origin.clone();
        local_container.secure_cookie = false;

        assert!(validate_deployment_security(&local_container).is_err());
    }

    #[test]
    fn deployment_security_rejects_mismatched_local_container_audience() {
        let mut local_container = AppConfig::from_env();
        local_container.deployment_mode = "local-container".to_owned();
        local_container.bind_address = "0.0.0.0:8080".to_owned();
        local_container.web_origin = "http://127.0.0.1:5173".to_owned();
        local_container.auth_audience = "http://127.0.0.1:5174".to_owned();
        local_container.secure_cookie = false;

        assert!(validate_deployment_security(&local_container).is_err());
    }

    #[test]
    fn deployment_security_rejects_loopback_local_container_bind() {
        let mut local_container = AppConfig::from_env();
        local_container.deployment_mode = "local-container".to_owned();
        local_container.bind_address = "127.0.0.1:8080".to_owned();
        local_container.web_origin = "http://127.0.0.1:5173".to_owned();
        local_container.auth_audience = local_container.web_origin.clone();
        local_container.secure_cookie = false;

        assert!(validate_deployment_security(&local_container).is_err());
    }

    #[test]
    fn deployment_security_rejects_secure_local_container_cookie() {
        let mut local_container = AppConfig::from_env();
        local_container.deployment_mode = "local-container".to_owned();
        local_container.bind_address = "0.0.0.0:8080".to_owned();
        local_container.web_origin = "http://127.0.0.1:5173".to_owned();
        local_container.auth_audience = local_container.web_origin.clone();
        local_container.secure_cookie = true;

        assert!(validate_deployment_security(&local_container).is_err());
    }

    #[test]
    fn deployment_security_requires_secure_https_staging() {
        let mut staging = AppConfig::from_env();
        staging.deployment_mode = "staging".to_owned();
        staging.bind_address = "0.0.0.0:8080".to_owned();
        staging.web_origin = "https://reader.staging.example".to_owned();
        staging.auth_audience = staging.web_origin.clone();
        staging.secure_cookie = false;
        assert!(validate_deployment_security(&staging).is_err());

        staging.secure_cookie = true;
        assert!(validate_deployment_security(&staging).is_ok());
    }

    #[tokio::test]
    async fn disabled_telegram_omits_capability_and_provider_routes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router_with_state(AppState::empty());
        let capabilities: ServiceCapabilities =
            json_get(app.clone(), "/api/v1/capabilities").await?;
        assert!(!capabilities
            .features
            .iter()
            .any(|feature| feature.starts_with("telegram-")));
        let response = app
            .oneshot(authenticated_request(
                Request::builder()
                    .uri("/api/v1/providers/telegram/connection")
                    .body(Body::empty())?,
            ))
            .await?;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn capabilities_route_reports_s1_contracts() -> Result<(), Box<dyn std::error::Error>> {
        let capabilities: ServiceCapabilities =
            json_get(build_router(), "/api/v1/capabilities").await?;

        assert!(capabilities
            .features
            .iter()
            .any(|feature| feature == "annotation-export"));
        assert!(!capabilities
            .features
            .iter()
            .any(|feature| feature == "telegram-webhook"));
        Ok(())
    }

    #[tokio::test]
    async fn telegram_webhook_is_absent_without_secret() -> Result<(), Box<dyn std::error::Error>> {
        let response = build_router()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/telegram")
                    .body(Body::empty())?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        Ok(())
    }

    #[tokio::test]
    async fn telegram_webhook_rejects_secret_before_parsing_body(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut state = AppState::empty();
        state.telegram_webhook_secret = Some(SecretString("a".repeat(32)));
        let response = build_router_with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/telegram")
                    .header(TELEGRAM_WEBHOOK_SECRET_HEADER, "wrong-secret")
                    .body(Body::from("this is intentionally not JSON"))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_malformed_webhook_is_acknowledged_without_retry(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let secret = "a".repeat(32);
        let mut state = AppState::empty();
        state.telegram_webhook_secret = Some(SecretString(secret.clone()));
        let response = build_router_with_state(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhooks/telegram")
                    .header(TELEGRAM_WEBHOOK_SECRET_HEADER, secret)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from("not-json"))?,
            )
            .await?;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        Ok(())
    }

    #[test]
    fn telegram_webhook_secret_is_redacted_and_validated() {
        let value = "sensitive-webhook-secret-value-123";
        let secret = SecretString(value.to_owned());

        assert!(!format!("{secret:?}").contains(value));
        assert!(validate_telegram_webhook_secret(Some(value)).is_ok());
        assert!(validate_telegram_webhook_secret(Some("short")).is_err());
        assert!(webhook_secret_matches(value, value));
        assert!(!webhook_secret_matches(
            value,
            "different-webhook-secret-value-12"
        ));
    }

    #[tokio::test]
    async fn migrations_route_reports_s1_domain_migrations(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let migrations: Vec<SchemaMigration> =
            json_get(build_router(), "/api/v1/schema/migrations").await?;

        assert_eq!(migrations.len(), 10);
        Ok(())
    }

    #[tokio::test]
    async fn seeded_reader_document_opens_fixture_through_shared_core(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let materials: Vec<LibraryEntry> = json_get(app.clone(), "/api/v1/materials").await?;
        let material = materials
            .first()
            .ok_or_else(|| std::io::Error::other("seeded material missing"))?;
        let revision_id = material
            .active_revision_id
            .ok_or_else(|| std::io::Error::other("seeded revision missing"))?;
        let document: ReadingDocument = json_get(
            app,
            &format!("/api/v1/revisions/{revision_id}/reading-document"),
        )
        .await?;

        assert_eq!(document.title, "Architecture Notes for Readers");
        Ok(())
    }

    #[tokio::test]
    async fn seed_auth_registration_stores_public_identity_boundary(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[0x42; 32]);
        let request = lumi_core::RegisterAccountRequest {
            lookup_id: lumi_core::encode_auth_bytes(&[0x24; 32]),
            public_key: lumi_core::encode_auth_bytes(signing_key.verifying_key().as_bytes()),
            nickname: Some("reader".to_owned()),
            device_name: "Test browser".to_owned(),
            idempotency_key: "register-test-reader".to_owned(),
        };
        let session: lumi_core::SessionBootstrap = json_post(
            build_router(),
            "/api/v1/auth/register",
            json_body(&request)?,
        )
        .await?;

        assert_eq!(session.account.nickname.as_deref(), Some("reader"));
        Ok(())
    }

    #[tokio::test]
    async fn blob_manifest_route_returns_source_and_resources(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let materials: Vec<LibraryEntry> = json_get(app.clone(), "/api/v1/materials").await?;
        let material = materials
            .first()
            .ok_or_else(|| std::io::Error::other("seeded material missing"))?;
        let revision_id = material
            .active_revision_id
            .ok_or_else(|| std::io::Error::other("seeded revision missing"))?;
        let package: NormalizedContentPackage = json_get(
            app.clone(),
            &format!("/api/v1/revisions/{revision_id}/package"),
        )
        .await?;
        let manifest: BlobManifest =
            json_get(app, &format!("/api/v1/blobs/{}", package.resources.id)).await?;

        assert!(manifest
            .blobs
            .iter()
            .any(|blob| { matches!(blob.role, lumi_core::BlobRole::Source) }));
        Ok(())
    }

    #[tokio::test]
    async fn fixture_import_creates_material_revision_and_job(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let response: ImportFixtureResponse = json_post(
            build_router(),
            "/api/v1/imports/fixtures/simple",
            Body::empty(),
        )
        .await?;
        let material = response
            .material
            .ok_or_else(|| std::io::Error::other("import material missing"))?;
        let revision = response
            .revision
            .ok_or_else(|| std::io::Error::other("import revision missing"))?;

        assert_eq!(material.active_revision_id, revision.id);
        Ok(())
    }

    #[tokio::test]
    async fn failed_fixture_import_keeps_diagnostic_job() -> Result<(), Box<dyn std::error::Error>>
    {
        let app = build_router();
        let response: ImportFixtureResponse =
            json_post(app.clone(), "/api/v1/imports/fixtures/empty", Body::empty()).await?;
        let diagnostics: Vec<ImportDiagnostic> = json_get(
            app,
            &format!("/api/v1/jobs/{}/diagnostics", response.job.id),
        )
        .await?;

        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error));
        Ok(())
    }

    #[tokio::test]
    async fn library_state_can_archive_and_delete_material(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "simple").await?;
        let command = UpdateLibraryStateCommand {
            material_id: imported.material.id,
            library_state: LibraryState::Archived,
        };
        let archived: LibraryEntry = json_patch(
            app.clone(),
            &format!("/api/v1/materials/{}/library-state", imported.material.id),
            json_body(&command)?,
        )
        .await?;

        assert_eq!(archived.library_state, LibraryState::Archived);

        let status = request_status(
            app.clone(),
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/materials/{}", imported.material.id))
                .body(Body::empty())?,
        )
        .await?;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let materials: Vec<LibraryEntry> = json_get(app, "/api/v1/materials").await?;
        assert!(!materials
            .iter()
            .any(|material| material.id == imported.material.id));
        Ok(())
    }

    #[tokio::test]
    async fn source_epub_download_returns_original_fixture_bytes(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "simple").await?;
        let response = app
            .oneshot(authenticated_request(
                Request::builder()
                    .uri(format!("/api/v1/materials/{}/source", imported.material.id))
                    .body(Body::empty())?,
            ))
            .await?;
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;

        assert_eq!(content_type, "application/epub+zip");
        assert!(!bytes.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn annotation_command_persists_for_material() -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "rich").await?;
        let command = sample_fixture_highlight(&imported)
            .ok_or_else(|| std::io::Error::other("fixture highlight missing"))?;
        let annotation: Annotation = json_post(
            app.clone(),
            &format!("/api/v1/materials/{}/annotations", imported.material.id),
            json_body(&command)?,
        )
        .await?;
        let annotations: Vec<Annotation> = json_get(
            app,
            &format!("/api/v1/materials/{}/annotations", imported.material.id),
        )
        .await?;

        assert_eq!(
            annotations.first().map(|stored| stored.id),
            Some(annotation.id)
        );
        Ok(())
    }

    #[tokio::test]
    async fn reading_progress_command_persists_for_material(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "rich").await?;
        let command = sample_fixture_highlight(&imported)
            .ok_or_else(|| std::io::Error::other("fixture highlight missing"))?;
        let move_command = MoveReadingPositionCommand {
            material_id: imported.material.id,
            revision_id: imported.revision.id,
            locator: command.anchor,
            progress_fraction: 1.25,
        };
        let progress: ReadingProgress = json_put(
            app.clone(),
            &format!("/api/v1/materials/{}/progress", imported.material.id),
            json_body(&move_command)?,
        )
        .await?;
        let persisted: Option<ReadingProgress> = json_get(
            app,
            &format!("/api/v1/materials/{}/progress", imported.material.id),
        )
        .await?;

        assert_eq!(
            persisted.map(|stored| stored.progress_fraction),
            Some(progress.progress_fraction)
        );
        Ok(())
    }

    #[tokio::test]
    async fn annotation_kind_supports_note_payload() -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "simple").await?;
        let mut command = sample_fixture_highlight(&imported)
            .ok_or_else(|| std::io::Error::other("fixture highlight missing"))?;
        command.kind = AnnotationKind::Note {
            body: "Durable S0 note".to_owned(),
        };
        let annotation: Annotation = json_post(
            app,
            &format!("/api/v1/materials/{}/annotations", imported.material.id),
            json_body(&command)?,
        )
        .await?;

        assert!(matches!(annotation.kind, AnnotationKind::Note { .. }));
        Ok(())
    }

    #[tokio::test]
    async fn annotation_kind_supports_highlight_style() -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "simple").await?;
        let mut command = sample_fixture_highlight(&imported)
            .ok_or_else(|| std::io::Error::other("fixture highlight missing"))?;
        command.kind = AnnotationKind::Highlight {
            style: HighlightStyle::Green,
        };
        let annotation: Annotation = json_post(
            app,
            &format!("/api/v1/materials/{}/annotations", imported.material.id),
            json_body(&command)?,
        )
        .await?;

        assert!(matches!(
            annotation.kind,
            AnnotationKind::Highlight {
                style: HighlightStyle::Green
            }
        ));
        Ok(())
    }

    #[tokio::test]
    async fn annotation_can_be_updated_deleted_and_exported(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "simple").await?;
        let mut command = sample_fixture_highlight(&imported)
            .ok_or_else(|| std::io::Error::other("fixture highlight missing"))?;
        command.kind = AnnotationKind::Note {
            body: "First note".to_owned(),
        };
        let annotation: Annotation = json_post(
            app.clone(),
            &format!("/api/v1/materials/{}/annotations", imported.material.id),
            json_body(&command)?,
        )
        .await?;
        let update = UpdateAnnotationCommand {
            material_id: imported.material.id,
            annotation_id: annotation.id,
            expected_revision: annotation.revision,
            kind: AnnotationKind::Note {
                body: "Edited note".to_owned(),
            },
        };
        let edited: Annotation = json_put(
            app.clone(),
            &format!(
                "/api/v1/materials/{}/annotations/{}",
                imported.material.id, annotation.id
            ),
            json_body(&update)?,
        )
        .await?;
        let export: AnnotationExport = json_get(
            app.clone(),
            &format!(
                "/api/v1/materials/{}/annotations/export",
                imported.material.id
            ),
        )
        .await?;
        let deleted: Annotation = json_delete(
            app,
            &format!(
                "/api/v1/materials/{}/annotations/{}",
                imported.material.id, annotation.id
            ),
            json_body(&DeleteAnnotationCommand {
                material_id: imported.material.id,
                annotation_id: annotation.id,
                expected_revision: edited.revision,
            })?,
        )
        .await?;

        assert_eq!(edited.revision, 2);
        assert_eq!(
            export
                .entries
                .first()
                .and_then(|entry| entry.note_body.as_deref()),
            Some("Edited note")
        );
        assert_eq!(deleted.id, annotation.id);
        Ok(())
    }

    #[tokio::test]
    async fn account_owned_routes_hide_another_accounts_material(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let second = register_test_session(app.clone(), 0x73).await?;
        let imported: ImportFixtureResponse = request_json_with_session(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/v1/imports/fixtures/simple")
                .body(Body::empty())?,
            &second,
        )
        .await?;
        let material_id = imported
            .material
            .ok_or_else(|| std::io::Error::other("imported material missing"))?;
        let revision_id = imported
            .revision
            .ok_or_else(|| std::io::Error::other("imported revision missing"))?
            .id;
        let protected = [
            format!("/api/v1/materials/{}", material_id.id),
            format!("/api/v1/materials/{}/source", material_id.id),
            format!("/api/v1/materials/{}/progress", material_id.id),
            format!("/api/v1/materials/{}/annotations", material_id.id),
            format!("/api/v1/materials/{}/annotations/export", material_id.id),
            format!("/api/v1/revisions/{revision_id}"),
            format!("/api/v1/revisions/{revision_id}/package"),
            format!("/api/v1/revisions/{revision_id}/reading-document"),
        ];
        for uri in protected {
            let response = app
                .clone()
                .oneshot(authenticated_request(
                    Request::builder().uri(uri).body(Body::empty())?,
                ))
                .await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        Ok(())
    }

    #[tokio::test]
    async fn postgres_account_session_csrf_route_matrix() -> Result<(), Box<dyn std::error::Error>>
    {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        run_migrations(&database_url).await?;
        let blob_root =
            std::env::temp_dir().join(format!("lumi-route-matrix-{}", uuid::Uuid::now_v7()));
        let mut config = AppConfig::from_env();
        config.database_url = database_url;
        config.blob_root = blob_root.clone();
        config.bind_address = DEFAULT_BIND_ADDRESS.to_owned();
        config.deployment_mode = "local".to_owned();
        let app = build_router_with_state(AppState::persistent(&config).await?);
        let owner = register_test_session(app.clone(), 0x81).await?;
        let foreign = register_test_session(app.clone(), 0x82).await?;
        let imported: ImportFixtureResponse = request_json_with_session(
            app.clone(),
            Request::builder()
                .method("POST")
                .uri("/api/v1/imports/fixtures/simple")
                .body(Body::empty())?,
            &owner,
        )
        .await?;
        let material_id = imported
            .material
            .ok_or_else(|| std::io::Error::other("matrix material missing"))?
            .id;
        let foreign_read = app
            .clone()
            .oneshot(
                foreign.apply(
                    Request::builder()
                        .uri(format!("/api/v1/materials/{material_id}"))
                        .body(Body::empty())?,
                ),
            )
            .await?;
        assert_eq!(foreign_read.status(), StatusCode::NOT_FOUND);
        let command = UpdateLibraryStateCommand {
            material_id,
            library_state: LibraryState::Archived,
        };
        let missing_csrf = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/materials/{material_id}/library-state"))
                    .header(header::COOKIE, &owner.cookie)
                    .header(header::ORIGIN, DEFAULT_WEB_ORIGIN)
                    .header("idempotency-key", "matrix-missing-csrf")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json_body(&command)?)?,
            )
            .await?;
        assert_eq!(missing_csrf.status(), StatusCode::FORBIDDEN);
        let foreign_write = app
            .clone()
            .oneshot(
                foreign.apply(
                    Request::builder()
                        .method("PATCH")
                        .uri(format!("/api/v1/materials/{material_id}/library-state"))
                        .header("idempotency-key", "matrix-foreign-write")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(json_body(&command)?)?,
                ),
            )
            .await?;
        assert_eq!(foreign_write.status(), StatusCode::NOT_FOUND);
        let logout = app
            .clone()
            .oneshot(
                owner.apply(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/auth/logout")
                        .body(Body::empty())?,
                ),
            )
            .await?;
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        let replay = app
            .oneshot(
                owner.apply(
                    Request::builder()
                        .uri("/api/v1/account/me")
                        .body(Body::empty())?,
                ),
            )
            .await?;
        let _ = tokio::fs::remove_dir_all(blob_root).await;
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn downloads_are_private_attachments_with_nosniff(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let imported = import_fixture(app.clone(), "simple").await?;
        for uri in [
            format!("/api/v1/materials/{}/source", imported.material.id),
            format!(
                "/api/v1/materials/{}/annotations/export",
                imported.material.id
            ),
        ] {
            let response = app
                .clone()
                .oneshot(authenticated_request(
                    Request::builder().uri(uri).body(Body::empty())?,
                ))
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers().get(header::CACHE_CONTROL),
                Some(&HeaderValue::from_static("private, no-store"))
            );
            assert_eq!(
                response.headers().get("x-content-type-options"),
                Some(&HeaderValue::from_static("nosniff"))
            );
            assert!(response.headers().contains_key(header::CONTENT_DISPOSITION));
        }
        Ok(())
    }

    #[tokio::test]
    async fn login_challenge_is_consumed_once_and_revoked_session_is_rejected(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use ed25519_dalek::Signer;

        let app = build_router();
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
        let lookup_id = [0x15; 32];
        let request = lumi_core::RegisterAccountRequest {
            lookup_id: lumi_core::encode_auth_bytes(&lookup_id),
            public_key: lumi_core::encode_auth_bytes(signing_key.verifying_key().as_bytes()),
            nickname: None,
            device_name: "First browser".to_owned(),
            idempotency_key: "register-challenge-test".to_owned(),
        };
        let _: lumi_core::SessionBootstrap =
            json_post(app.clone(), "/api/v1/auth/register", json_body(&request)?).await?;
        let challenge: lumi_core::ChallengeResponse = json_post(
            app.clone(),
            "/api/v1/auth/challenges",
            json_body(&lumi_core::CreateChallengeRequest {
                lookup_id: lumi_core::encode_auth_bytes(&lookup_id),
            })?,
        )
        .await?;
        let transcript = lumi_core::AuthChallenge {
            id: challenge.challenge_id,
            lookup_id: lumi_core::decode_auth_bytes(&challenge.lookup_id)?,
            nonce: lumi_core::decode_auth_bytes(&challenge.nonce)?,
            audience: challenge.audience.clone(),
            expires_at: challenge.expires_at,
        }
        .signing_bytes();
        assert_eq!(
            lumi_core::encode_auth_bytes(&transcript),
            challenge.transcript
        );
        let signature = signing_key.sign(&transcript);
        let login = lumi_core::CompleteLoginRequest {
            challenge_id: challenge.challenge_id,
            signature: lumi_core::encode_auth_bytes(&signature.to_bytes()),
            device_name: "Recovered browser".to_owned(),
        };
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json_body(&login)?)?,
            )
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        let session = test_session_from_response(response).await?;
        let replay = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json_body(&login)?)?,
            )
            .await?;
        assert_eq!(replay.status(), StatusCode::UNAUTHORIZED);
        let logout = app
            .clone()
            .oneshot(
                session.apply(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/auth/logout")
                        .body(Body::empty())?,
                ),
            )
            .await?;
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        let after_logout = app
            .oneshot(
                session.apply(
                    Request::builder()
                        .uri("/api/v1/account/me")
                        .body(Body::empty())?,
                ),
            )
            .await?;
        assert_eq!(after_logout.status(), StatusCode::UNAUTHORIZED);
        Ok(())
    }

    #[tokio::test]
    async fn profile_command_is_idempotent_and_rejects_stale_revision(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let command = lumi_core::UpdateAccountProfileRequest {
            nickname: Some("Updated reader".to_owned()),
            expected_revision: 1,
            idempotency_key: "profile-update-1".to_owned(),
        };
        let first: lumi_core::AccountSummary =
            json_patch(app.clone(), "/api/v1/account/profile", json_body(&command)?).await?;
        let retry: lumi_core::AccountSummary =
            json_patch(app.clone(), "/api/v1/account/profile", json_body(&command)?).await?;
        let stale = lumi_core::UpdateAccountProfileRequest {
            nickname: Some("Stale overwrite".to_owned()),
            expected_revision: 1,
            idempotency_key: "profile-update-2".to_owned(),
        };
        let response = app
            .oneshot(authenticated_request(
                Request::builder()
                    .method("PATCH")
                    .uri("/api/v1/account/profile")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json_body(&stale)?)?,
            ))
            .await?;

        assert_eq!(first, retry);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        Ok(())
    }

    async fn import_fixture(
        app: Router,
        slug: &str,
    ) -> Result<ImportedFixture, Box<dyn std::error::Error>> {
        let response: ImportFixtureResponse = json_post(
            app.clone(),
            &format!("/api/v1/imports/fixtures/{slug}"),
            Body::empty(),
        )
        .await?;
        let material = response
            .material
            .ok_or_else(|| std::io::Error::other("import material missing"))?;
        let revision = response
            .revision
            .ok_or_else(|| std::io::Error::other("import revision missing"))?;
        let document: ReadingDocument = json_get(
            app,
            &format!("/api/v1/revisions/{}/reading-document", revision.id),
        )
        .await?;
        let fixture = ImportedFixture {
            account: WebAccount {
                user_id: material.owner_id,
                profile: lumi_core::AccountProfile { nickname: None },
                status: lumi_core::AccountStatus::Active,
                auth: lumi_core::SeedAuthPrototype {
                    account_lookup_key: String::new(),
                    verifier: String::new(),
                    algorithm: lumi_core::SeedAuthAlgorithm::ReplaceableChallengeSigningSha256,
                },
                created_at: 0,
            },
            material,
            revision,
            package: NormalizedContentPackage {
                id: lumi_core::NormalizedPackageId::now_v7(),
                revision_id: document.revision_id,
                manifest: lumi_core::NormalizedPackageManifest::s0(
                    document.title.clone(),
                    document.creators.clone(),
                    None,
                    document
                        .navigation
                        .iter()
                        .map(|item| item.id.clone())
                        .collect(),
                    lumi_core::SourceIdentity {
                        format: lumi_core::SourceFormat::Epub,
                        source_name: "test.epub".to_owned(),
                        source_hash: String::new(),
                    },
                ),
                units: Vec::new(),
                blocks: Vec::new(),
                navigation: document.navigation.clone(),
                resources: BlobManifest {
                    id: BlobManifestId::now_v7(),
                    schema_version: String::new(),
                    blobs: Vec::new(),
                },
                diagnostics: Vec::new(),
            },
            reading_document: document,
            job: response.job,
        };

        Ok(fixture)
    }

    fn json_body<T: Serialize>(value: &T) -> Result<Body, serde_json::Error> {
        serde_json::to_vec(value).map(Body::from)
    }

    async fn json_get<T: for<'de> Deserialize<'de>>(
        app: Router,
        uri: &str,
    ) -> Result<T, Box<dyn std::error::Error>> {
        request_json(app, Request::builder().uri(uri).body(Body::empty())?).await
    }

    async fn json_post<T: for<'de> Deserialize<'de>>(
        app: Router,
        uri: &str,
        body: Body,
    ) -> Result<T, Box<dyn std::error::Error>> {
        request_json(
            app,
            Request::builder()
                .method("POST")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)?,
        )
        .await
    }

    async fn json_patch<T: for<'de> Deserialize<'de>>(
        app: Router,
        uri: &str,
        body: Body,
    ) -> Result<T, Box<dyn std::error::Error>> {
        request_json(
            app,
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)?,
        )
        .await
    }

    async fn json_put<T: for<'de> Deserialize<'de>>(
        app: Router,
        uri: &str,
        body: Body,
    ) -> Result<T, Box<dyn std::error::Error>> {
        request_json(
            app,
            Request::builder()
                .method("PUT")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)?,
        )
        .await
    }

    async fn json_delete<T: for<'de> Deserialize<'de>>(
        app: Router,
        uri: &str,
        body: Body,
    ) -> Result<T, Box<dyn std::error::Error>> {
        request_json(
            app,
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .body(body)?,
        )
        .await
    }

    async fn request_status(
        app: Router,
        request: Request<Body>,
    ) -> Result<StatusCode, Box<dyn std::error::Error>> {
        Ok(app.oneshot(authenticated_request(request)).await?.status())
    }

    async fn request_json<T: for<'de> Deserialize<'de>>(
        app: Router,
        request: Request<Body>,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let response = app.oneshot(authenticated_request(request)).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let parsed = serde_json::from_slice(&bytes)?;

        Ok(parsed)
    }

    fn authenticated_request(mut request: Request<Body>) -> Request<Body> {
        request.headers_mut().insert(
            header::COOKIE,
            HeaderValue::from_static("lumi_session=lumi-local-seeded-session"),
        );
        if !matches!(
            *request.method(),
            axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
        ) {
            request
                .headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_static(DEFAULT_WEB_ORIGIN));
            request.headers_mut().insert(
                header::HeaderName::from_static("x-lumi-csrf"),
                HeaderValue::from_static("lumi-local-seeded-csrf"),
            );
            request
                .headers_mut()
                .entry("idempotency-key")
                .or_insert_with(|| {
                    HeaderValue::from_str(&uuid::Uuid::now_v7().to_string())
                        .unwrap_or_else(|_| HeaderValue::from_static("test-idempotency"))
                });
        }
        request
    }

    #[derive(Clone)]
    struct TestSession {
        cookie: String,
        csrf: String,
    }

    impl TestSession {
        fn apply(&self, mut request: Request<Body>) -> Request<Body> {
            request.headers_mut().insert(
                header::COOKIE,
                HeaderValue::from_str(&self.cookie)
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );
            if !matches!(
                *request.method(),
                axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
            ) {
                request
                    .headers_mut()
                    .insert(header::ORIGIN, HeaderValue::from_static(DEFAULT_WEB_ORIGIN));
                request.headers_mut().insert(
                    header::HeaderName::from_static("x-lumi-csrf"),
                    HeaderValue::from_str(&self.csrf)
                        .unwrap_or_else(|_| HeaderValue::from_static("")),
                );
            }
            request
        }
    }

    async fn register_test_session(
        app: Router,
        seed: u8,
    ) -> Result<TestSession, Box<dyn std::error::Error>> {
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
        let request = lumi_core::RegisterAccountRequest {
            lookup_id: lumi_core::encode_auth_bytes(&[seed.wrapping_add(1); 32]),
            public_key: lumi_core::encode_auth_bytes(signing_key.verifying_key().as_bytes()),
            nickname: None,
            device_name: "Isolation browser".to_owned(),
            idempotency_key: format!("register-{seed}"),
        };
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/register")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(json_body(&request)?)?,
            )
            .await?;
        test_session_from_response(response).await
    }

    async fn test_session_from_response(
        response: Response,
    ) -> Result<TestSession, Box<dyn std::error::Error>> {
        let cookies = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .collect::<Vec<_>>();
        let cookie = cookies
            .iter()
            .find_map(|value| value.split(';').next())
            .ok_or_else(|| std::io::Error::other("session cookie missing"))?
            .to_owned();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        let bootstrap: lumi_core::SessionBootstrap = serde_json::from_slice(&body)?;
        Ok(TestSession {
            cookie,
            csrf: bootstrap.csrf_token,
        })
    }

    async fn request_json_with_session<T: for<'de> Deserialize<'de>>(
        app: Router,
        request: Request<Body>,
        session: &TestSession,
    ) -> Result<T, Box<dyn std::error::Error>> {
        let response = app.oneshot(session.apply(request)).await?;
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}
