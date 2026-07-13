#![deny(missing_docs)]
//! Axum API boundary for Lumi local development.
//!
//! Product routes will grow under `/api/v1`. Dioxus server functions may be
//! added later for narrow UI calls, but durable system contracts belong here.

mod account;
mod auth_api;

use std::collections::HashMap;
use std::sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, Method, StatusCode},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, patch, post, put},
    Extension, Json, Router,
};
use lumi_core::{
    import_epub_fixture, rich_epub_fixture, s1_schema_migrations, simple_epub_fixture, Annotation,
    AnnotationExport, AnnotationId, BlobManifest, BlobManifestId, CreateAnnotationCommand,
    DiagnosticSeverity, DocumentRevision, DocumentRevisionId, EpubFixture, HealthResponse,
    ImportDiagnostic, ImportedFixture, Job, JobId, JobKind, JobStage, JobStatus, LibraryState,
    Material, MaterialId, MoveReadingPositionCommand, NormalizedContentPackage, ReadingDocument,
    ReadingProgress, SchemaMigration, ServiceCapabilities, UpdateAnnotationCommand,
    UpdateLibraryStateCommand, UserId,
};
use serde::{Deserialize, Serialize};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use account::{AccountStore, AuthenticatedSession, MemoryAccountStore, PgAccountStore};

/// Default bind address for local development.
pub const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8080";
/// Default local PostgreSQL database URL.
pub const DEFAULT_DATABASE_URL: &str = "postgres://lumi:lumi-local@127.0.0.1:5432/lumi";
/// Default local web origin accepted by CORS and CSRF checks.
pub const DEFAULT_WEB_ORIGIN: &str = "http://127.0.0.1:5173";

/// Runtime configuration for the Lumi server process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppConfig {
    bind_address: String,
    database_url: String,
    web_origin: String,
    auth_audience: String,
    secure_cookie: bool,
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

        Self {
            bind_address,
            database_url,
            web_origin,
            auth_audience,
            secure_cookie,
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
        }
    }

    /// Connect the durable account repositories configured for the server.
    ///
    /// # Errors
    ///
    /// Returns an error when PostgreSQL is unavailable.
    pub async fn persistent(config: &AppConfig) -> anyhow::Result<Self> {
        let accounts = PgAccountStore::connect(config.database_url())
            .await
            .map_err(|error| anyhow::anyhow!(error))?;
        Ok(Self {
            repository: Arc::new(RwLock::new(Repository::default())),
            accounts: Arc::new(accounts),
            security: SecurityConfig::from_app(config),
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
        }
    }

    fn accounts(&self) -> &dyn AccountStore {
        self.accounts.as_ref()
    }

    fn security(&self) -> &SecurityConfig {
        &self.security
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
    let protected = Router::new()
        .merge(auth_api::protected_account_routes())
        .route("/materials", get(list_materials))
        .route(
            "/materials/{material_id}",
            get(get_material).delete(delete_material),
        )
        .route(
            "/materials/{material_id}/library-state",
            patch(update_library_state),
        )
        .route("/materials/{material_id}/source", get(download_source_epub))
        .route(
            "/materials/{material_id}/annotations",
            get(list_annotations).post(create_annotation),
        )
        .route(
            "/materials/{material_id}/annotations/export",
            get(export_annotations),
        )
        .route(
            "/materials/{material_id}/annotations/{annotation_id}",
            put(update_annotation).delete(delete_annotation),
        )
        .route(
            "/materials/{material_id}/progress",
            get(get_progress).put(move_reading_position),
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
        .route("/blobs/{manifest_id}", get(get_blob_manifest))
        .route(
            "/imports/fixtures/{fixture_slug}",
            post(import_fixture_material),
        )
        .route("/jobs/{job_id}", get(get_job))
        .route("/jobs/{job_id}/diagnostics", get(get_job_diagnostics))
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

    Router::new()
        .nest("/api/v1", api)
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::exact(allowed_origin))
                .allow_credentials(true)
                .allow_headers([
                    header::CONTENT_TYPE,
                    header::ORIGIN,
                    header::HeaderName::from_static("x-lumi-csrf"),
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
        .layer(TraceLayer::new_for_http())
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

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse::ok("lumi-server"))
}

async fn readiness(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    state
        .accounts()
        .ready()
        .await
        .map_err(|_| AppError::Unavailable("account repository"))?;
    Ok(Json(HealthResponse::ok("lumi-server")))
}

async fn capabilities() -> Json<ServiceCapabilities> {
    Json(ServiceCapabilities::s1())
}

async fn schema_migrations() -> Json<Vec<SchemaMigration>> {
    Json(s1_schema_migrations())
}

async fn list_materials(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<Material>>, AppError> {
    let repository = read_repository(&state)?;
    let mut materials = repository
        .materials
        .values()
        .filter(|material| {
            material.owner_id == session.user_id && material.library_state != LibraryState::Deleted
        })
        .cloned()
        .collect::<Vec<_>>();
    materials.sort_by(|left, right| left.canonical_title.cmp(&right.canonical_title));

    Ok(Json(materials))
}

async fn get_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<Material>, AppError> {
    let repository = read_repository(&state)?;
    let material = repository
        .material_owned_by(session.user_id, material_id)
        .cloned()?;

    Ok(Json(material))
}

async fn update_library_state(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    Json(command): Json<UpdateLibraryStateCommand>,
) -> Result<Json<Material>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
    }

    let mut repository = write_repository(&state)?;
    let material = repository
        .materials
        .get_mut(&material_id)
        .filter(|material| material.owner_id == session.user_id)
        .ok_or(AppError::NotFound("material"))?;
    material.library_state = command.library_state;

    Ok(Json(material.clone()))
}

async fn delete_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<StatusCode, AppError> {
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
    let repository = read_repository(&state)?;
    repository.ensure_material_owned(session.user_id, material_id)?;
    let source = repository
        .source_downloads
        .get(&material_id)
        .cloned()
        .ok_or(AppError::NotFound("source_epub"))?;
    let file_name = source
        .file_name
        .chars()
        .filter(|character| !matches!(character, '"' | '\\'))
        .collect::<String>();

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, source.media_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{file_name}\""),
        )
        .body(Body::from(source.bytes))
        .map_err(|_| AppError::Internal("failed to build source response"))
}

async fn get_revision(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(revision_id): Path<DocumentRevisionId>,
) -> Result<Json<DocumentRevision>, AppError> {
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
    let repository = read_repository(&state)?;
    repository.ensure_revision_owned(session.user_id, revision_id)?;
    let document = repository
        .reading_documents_by_revision
        .get(&revision_id)
        .cloned()
        .ok_or(AppError::NotFound("reading_document"))?;

    Ok(Json(document))
}

async fn get_blob_manifest(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(manifest_id): Path<BlobManifestId>,
) -> Result<Json<BlobManifest>, AppError> {
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

async fn get_job(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(job_id): Path<JobId>,
) -> Result<Json<Job>, AppError> {
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
    let repository = read_repository(&state)?;
    let diagnostics = repository
        .jobs
        .get(&job_id)
        .filter(|job| job.account_id == session.user_id)
        .map(|job| job.diagnostics.clone())
        .ok_or(AppError::NotFound("job"))?;

    Ok(Json(diagnostics))
}

async fn list_annotations(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<Vec<Annotation>>, AppError> {
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
    Json(command): Json<CreateAnnotationCommand>,
) -> Result<Json<Annotation>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
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
    Json(command): Json<UpdateAnnotationCommand>,
) -> Result<Json<Annotation>, AppError> {
    if command.material_id != material_id || command.annotation_id != annotation_id {
        return Err(AppError::BadRequest(
            "material or annotation id in path and body must match".to_owned(),
        ));
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
) -> Result<Json<Annotation>, AppError> {
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

    Ok(Json(annotations.remove(index)))
}

async fn export_annotations(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<AnnotationExport>, AppError> {
    let repository = read_repository(&state)?;
    let material = repository.material_owned_by(session.user_id, material_id)?;
    let annotations = repository
        .annotations_by_material
        .get(&material_id)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    Ok(Json(AnnotationExport::for_material(material, annotations)))
}

async fn get_progress(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
) -> Result<Json<Option<ReadingProgress>>, AppError> {
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
    Json(command): Json<MoveReadingPositionCommand>,
) -> Result<Json<ReadingProgress>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
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

    #[tokio::test]
    async fn capabilities_route_reports_s1_contracts() -> Result<(), Box<dyn std::error::Error>> {
        let capabilities: ServiceCapabilities =
            json_get(build_router(), "/api/v1/capabilities").await?;

        assert!(capabilities
            .features
            .iter()
            .any(|feature| feature == "annotation-export"));
        Ok(())
    }

    #[tokio::test]
    async fn migrations_route_reports_s1_domain_migrations(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let migrations: Vec<SchemaMigration> =
            json_get(build_router(), "/api/v1/schema/migrations").await?;

        assert_eq!(migrations.len(), 7);
        Ok(())
    }

    #[tokio::test]
    async fn seeded_reader_document_opens_fixture_through_shared_core(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let app = build_router();
        let materials: Vec<Material> = json_get(app.clone(), "/api/v1/materials").await?;
        let material = materials
            .first()
            .ok_or_else(|| std::io::Error::other("seeded material missing"))?;
        let document: ReadingDocument = json_get(
            app,
            &format!(
                "/api/v1/revisions/{}/reading-document",
                material.active_revision_id
            ),
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
        let materials: Vec<Material> = json_get(app.clone(), "/api/v1/materials").await?;
        let material = materials
            .first()
            .ok_or_else(|| std::io::Error::other("seeded material missing"))?;
        let package: NormalizedContentPackage = json_get(
            app.clone(),
            &format!("/api/v1/revisions/{}/package", material.active_revision_id),
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
        let archived: Material = json_patch(
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

        let materials: Vec<Material> = json_get(app, "/api/v1/materials").await?;
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
            .ok_or_else(|| std::io::Error::other("imported material missing"))?
            .id;
        let response = app
            .oneshot(authenticated_request(
                Request::builder()
                    .uri(format!("/api/v1/materials/{material_id}"))
                    .body(Body::empty())?,
            ))
            .await?;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
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
    ) -> Result<T, Box<dyn std::error::Error>> {
        request_json(
            app,
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())?,
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
