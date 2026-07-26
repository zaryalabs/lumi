//! Durable real EPUB upload, worker lifecycle and PostgreSQL projections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lumi_core::{
    build_generated_lum_package, content_hash, import_epub, import_lum, import_markdown,
    import_telegram_composite, import_telegram_text, import_web_snapshot, normalize_pdf,
    AcceptedImport, Annotation, AnnotationExport, AnnotationKind, AnnotationStatus,
    AnnotationTarget, AnnotationType, BlobManifest, BlobRef, BlobRole, ContinueReadingEntry,
    CreateAnnotationCommand, DeleteAnnotationCommand, DerivedMaterialRelation, DiagnosticSeverity,
    DocumentRevision, DocumentRevisionId, EpubImportError, EpubImportRequest, EpubLimits,
    FixedLayoutContentPackage, GeneratedLumPackageRequest, ImportDiagnostic, ImportStatusEntry,
    ImportedEpub, ImportedPdf, ImportedPublication, ImportedPublicationResource, Job, JobId,
    JobKind, JobStage, JobStatus, LibraryEntry, LibraryState, LumImportError, LumImportRequest,
    LumLimits, MarkdownImportError, MarkdownImportRequest, MarkdownLimits, Material, MaterialId,
    MaterialImportStatus, MaterialKind, MoveReadingPositionCommand, NormalizedContentPackage,
    PageFidelityDocument, PdfImportRequest, PdfLimits, ReaderSettings, ReadingDocument,
    ReadingNode, ReadingNodeKind, ReadingProgress, RenderPlan, SourceIdentity,
    TelegramCapturedImage, TelegramMessageSnapshot, TelegramPhotoDescriptor, TelegramUpdate,
    TelegramWebSection, UpdateAnnotationCommand, GENERATED_LUM_PROVENANCE_VERSION,
    LUM_SOURCE_MEDIA_TYPE,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, PgRow, Postgres};
use time::OffsetDateTime;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::account::AuthenticatedSession;
use crate::blob::{BlobStore, BlobStoreError, LocalBlobStore, StoredBlob};
use crate::jobs::{ImportJobRepository, JobRuntime, JobRuntimeError};
use crate::pdf_engine::{PdfEngine, PopplerPdfEngine};
use crate::telegram_media::{
    RuntimeTelegramMediaCapture, TelegramMediaCapture, TelegramMediaRegistry,
    MAX_TELEGRAM_IMAGE_BYTES,
};
use crate::web::{BoundedWebFetcher, WebCapture};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
    #[cfg(test)]
    pub(crate) use sqlx_core::query_as::query_as;
    pub(crate) use sqlx_core::query_scalar::query_scalar;
}

const EPUB_SOURCE_MEDIA_TYPE: &str = "application/epub+zip";
const PDF_SOURCE_MEDIA_TYPE: &str = "application/pdf";
const MARKDOWN_SOURCE_MEDIA_TYPE: &str = "text/markdown; charset=utf-8";
const MAX_ACTIVE_IMPORT_WORKERS: usize = 8;
const MAX_PENDING_IMPORTS_PER_ACCOUNT: i64 = 16;
const MAX_CONCURRENT_DOCUMENT_UPLOADS: usize = 2;
const MAX_TELEGRAM_IMAGES: usize = 10;
const MAX_TELEGRAM_TOTAL_IMAGE_BYTES: usize = 30 * 1024 * 1024;
const MAX_TELEGRAM_LINKS: usize = 8;
const MAX_TELEGRAM_WEB_FETCHES: usize = 3;
const WORKER_LEASE_SQL: &str = "30 minutes";
const SOURCE_RESERVATION_LEASE_SQL: &str = "1 minute";

#[cfg(test)]
pub(crate) static POSTGRES_RECOVERY_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());
const REQUIRED_MIGRATION_COUNT: i64 = 20;
#[cfg(not(test))]
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(test)]
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
#[cfg(not(test))]
const SOURCE_RESERVATION_HEARTBEAT_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);
#[cfg(test)]
const SOURCE_RESERVATION_HEARTBEAT_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(50);

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImportServiceError {
    #[error("import object was not found")]
    NotFound,
    #[error("import command conflicts with current state")]
    Conflict,
    #[error("invalid import request: {0}")]
    BadRequest(&'static str),
    #[error("too many pending imports")]
    RateLimited,
    #[error("import service is unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct ImportService {
    pool: PgPool,
    blobs: Arc<dyn BlobStore>,
    web_capture: Arc<dyn WebCapture>,
    pdf_engine: Arc<dyn PdfEngine>,
    telegram_media: Arc<dyn TelegramMediaCapture>,
    telegram_media_registry: Arc<TelegramMediaRegistry>,
    cancellations: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
    worker_slots: Arc<Semaphore>,
    upload_slots: Arc<Semaphore>,
    account_upload_slots: Arc<Mutex<HashMap<Uuid, Arc<Semaphore>>>>,
    job_runtime: JobRuntime<ImportJobRepository>,
}

pub(crate) struct UploadAdmission {
    _global: OwnedSemaphorePermit,
    _account: OwnedSemaphorePermit,
}

#[derive(Clone, Copy)]
struct ImportWorkerClaim {
    job_id: JobId,
    claim_id: Uuid,
    fence: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FileUploadKind {
    Epub,
    Pdf,
    Markdown,
    Lum,
}

impl FileUploadKind {
    fn detect(file_name: &str, source: &[u8]) -> Result<Self, ImportServiceError> {
        let lowercase = file_name.to_ascii_lowercase();
        let has_pdf_header = source
            .get(..source.len().min(1_024))
            .is_some_and(|prefix| prefix.windows(5).any(|window| window == b"%PDF-"));
        if has_pdf_header || lowercase.ends_with(".pdf") {
            Ok(Self::Pdf)
        } else if lowercase.ends_with(".epub") {
            Ok(Self::Epub)
        } else if lowercase.ends_with(".md") || lowercase.ends_with(".markdown") {
            Ok(Self::Markdown)
        } else if lowercase.ends_with(".lum") {
            Ok(Self::Lum)
        } else {
            Err(ImportServiceError::BadRequest(
                "file must be an EPUB, PDF, Markdown or LUM document",
            ))
        }
    }

    const fn source_kind(self) -> &'static str {
        match self {
            Self::Epub => "epub",
            Self::Pdf => "pdf",
            Self::Markdown => "markdown",
            Self::Lum => "lum",
        }
    }

    const fn media_type(self) -> &'static str {
        match self {
            Self::Epub => EPUB_SOURCE_MEDIA_TYPE,
            Self::Pdf => PDF_SOURCE_MEDIA_TYPE,
            Self::Markdown => MARKDOWN_SOURCE_MEDIA_TYPE,
            Self::Lum => LUM_SOURCE_MEDIA_TYPE,
        }
    }

    const fn source_limit(self) -> u64 {
        match self {
            Self::Epub => EpubLimits::s1().source_bytes,
            Self::Pdf => PdfLimits::web_v1().source_bytes,
            Self::Markdown => MarkdownLimits::web_v1().source_bytes,
            Self::Lum => LumLimits::web_v1().source_bytes,
        }
    }
}

impl ImportService {
    pub(crate) fn local(pool: PgPool, blob_root: PathBuf) -> Self {
        let telegram_media_registry = TelegramMediaRegistry::new();
        Self {
            job_runtime: JobRuntime::new(ImportJobRepository::new(pool.clone())),
            pool,
            blobs: Arc::new(LocalBlobStore::new(blob_root)),
            web_capture: Arc::new(BoundedWebFetcher::from_env()),
            pdf_engine: Arc::new(PopplerPdfEngine::from_environment()),
            telegram_media: RuntimeTelegramMediaCapture::new(Arc::clone(&telegram_media_registry)),
            telegram_media_registry,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            worker_slots: Arc::new(Semaphore::new(MAX_ACTIVE_IMPORT_WORKERS)),
            upload_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_DOCUMENT_UPLOADS)),
            account_upload_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn ready(&self) -> Result<(), ImportServiceError> {
        let applied: i64 =
            sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE success = TRUE")
                .fetch_one(&self.pool)
                .await
                .map_err(log_storage_error)?;
        if applied < REQUIRED_MIGRATION_COUNT {
            return Err(ImportServiceError::Unavailable);
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), self.blobs.ready())
            .await
            .map_err(|_| ImportServiceError::Unavailable)?
            .map_err(map_blob_error)
    }

    #[cfg(test)]
    pub(crate) fn local_with_web_fixtures_and_media(
        pool: PgPool,
        blob_root: PathBuf,
        fixture_root: PathBuf,
        telegram_media: Arc<dyn TelegramMediaCapture>,
    ) -> Self {
        let telegram_media_registry = TelegramMediaRegistry::new();
        Self {
            job_runtime: JobRuntime::new(ImportJobRepository::new(pool.clone())),
            pool,
            blobs: Arc::new(LocalBlobStore::new(blob_root)),
            web_capture: Arc::new(BoundedWebFetcher::fixtures(fixture_root)),
            pdf_engine: Arc::new(PopplerPdfEngine::from_environment()),
            telegram_media,
            telegram_media_registry,
            cancellations: Arc::new(Mutex::new(HashMap::new())),
            worker_slots: Arc::new(Semaphore::new(MAX_ACTIVE_IMPORT_WORKERS)),
            upload_slots: Arc::new(Semaphore::new(MAX_CONCURRENT_DOCUMENT_UPLOADS)),
            account_upload_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn telegram_media_registry(&self) -> Arc<TelegramMediaRegistry> {
        Arc::clone(&self.telegram_media_registry)
    }

    pub(crate) fn try_begin_upload(
        &self,
        user_id: Uuid,
    ) -> Result<UploadAdmission, ImportServiceError> {
        let global = Arc::clone(&self.upload_slots)
            .try_acquire_owned()
            .map_err(|_| ImportServiceError::RateLimited)?;
        let account_slot = {
            let mut slots = self
                .account_upload_slots
                .lock()
                .map_err(|_| ImportServiceError::Unavailable)?;
            Arc::clone(
                slots
                    .entry(user_id)
                    .or_insert_with(|| Arc::new(Semaphore::new(1))),
            )
        };
        let account = account_slot
            .try_acquire_owned()
            .map_err(|_| ImportServiceError::RateLimited)?;
        Ok(UploadAdmission {
            _global: global,
            _account: account,
        })
    }

    /// Assemble, ordinary-import and atomically publish one completed
    /// abridgement candidate.
    pub(crate) async fn publish_abridgement_artifact(
        &self,
        owner_id: Uuid,
        artifact_id: Uuid,
    ) -> Result<DerivedMaterialRelation, ImportServiceError> {
        if let Some(relation) = self.derivation_by_artifact(owner_id, artifact_id).await? {
            return Ok(relation);
        }
        let repository = crate::ai::repository::PgAiRepository::new(self.pool.clone());
        let input = repository
            .abridgement_publication_input(owner_id, artifact_id)
            .await
            .map_err(|error| match error {
                crate::ai::repository::AiRepositoryError::NotFound => ImportServiceError::NotFound,
                crate::ai::repository::AiRepositoryError::Conflict
                | crate::ai::repository::AiRepositoryError::StaleClaim => {
                    ImportServiceError::Conflict
                }
                crate::ai::repository::AiRepositoryError::Invalid(_) => {
                    ImportServiceError::BadRequest("abridgement artifact is invalid")
                }
                crate::ai::repository::AiRepositoryError::Storage => {
                    ImportServiceError::Unavailable
                }
            })?;
        let material_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let source_name = format!("abridged-{}.lum", input.artifact_id.simple());
        let book_id = format!("lumi-abridged-{}", input.artifact_id.simple());
        let package_bytes = build_generated_lum_package(GeneratedLumPackageRequest {
            book_id: &book_id,
            source_material_id: input.source_material_id,
            source_revision_id: input.source_revision_id,
            task_id: input.task_id,
            artifact_id: input.artifact_id,
            prompt_version: &input.prompt_version,
            artifact_schema_version: &input.schema_version,
            payload: &input.payload,
            source_refs: &input.source_refs,
        })
        .map_err(|_| ImportServiceError::BadRequest("generated LUM assembly failed"))?;
        let imported = import_lum(
            LumImportRequest {
                owner_id,
                material_id,
                revision_id,
                source_name: &source_name,
                source: &package_bytes,
            },
            LumLimits::web_v1(),
            || false,
        )
        .map_err(|_| ImportServiceError::BadRequest("generated LUM validation failed"))?;
        let mut publication = PersistablePublication::from_reflowable(imported)?;
        publication.revision.created_at = timestamp_ms(OffsetDateTime::now_utc());

        let source_hash = content_hash(&package_bytes);
        let stored_source = self
            .blobs
            .put(&source_hash, &package_bytes)
            .await
            .map_err(map_blob_error)?;
        let mut stored_resources = Vec::with_capacity(publication.resources.len());
        for resource in &publication.resources {
            let stored = self
                .blobs
                .put(&resource.content_hash, &resource.bytes)
                .await
                .map_err(map_blob_error)?;
            stored_resources.push((resource.clone(), stored));
        }
        let normalized_bytes = serde_json::to_vec(&publication.package_payload)
            .map_err(|_| ImportServiceError::Unavailable)?;
        let normalized_hash = content_hash(&normalized_bytes);
        let stored_normalized = self
            .blobs
            .put(&normalized_hash, &normalized_bytes)
            .await
            .map_err(map_blob_error)?;

        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 8))")
            .bind(artifact_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
        if let Some(relation) =
            derivation_by_artifact_in_transaction(&mut tx, owner_id, artifact_id).await?
        {
            tx.rollback().await.map_err(log_storage_error)?;
            return Ok(relation);
        }
        let source_row = sqlx::query(
            "SELECT material.space_id FROM materials material JOIN document_revisions revision ON revision.revision_id = $2 AND revision.material_id = material.material_id AND revision.space_id = material.space_id WHERE material.material_id = $1 AND material.owner_user_id = $3 AND material.deleted_at IS NULL FOR SHARE OF material, revision",
        )
        .bind(input.source_material_id)
        .bind(input.source_revision_id)
        .bind(owner_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let space_id: Uuid = source_row.try_get("space_id").map_err(log_storage_error)?;
        let device_id: Uuid = sqlx::query_scalar(
            "SELECT device_id FROM sync_devices WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at, device_id LIMIT 1",
        )
        .bind(owner_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let job_id = Uuid::now_v7();
        let source_ref = SourceRef::Lum {
            blob_hash: source_hash.clone(),
            file_name: source_name.clone(),
            media_type: LUM_SOURCE_MEDIA_TYPE.to_owned(),
            device_id,
        };
        sqlx::query(
            "INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status, created_at, updated_at) VALUES ($1, $2, $3, 'lum', $4, 'active', $5, 'ready', $6, $6)",
        )
        .bind(material_id)
        .bind(space_id)
        .bind(owner_id)
        .bind(&publication.title)
        .bind(serde_json::to_value(&publication.source_identity).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        insert_blob_record(&mut tx, &source_hash, LUM_SOURCE_MEDIA_TYPE, &stored_source).await?;
        for (resource, stored) in &stored_resources {
            insert_blob_record(
                &mut tx,
                &resource.content_hash,
                &resource.media_type,
                stored,
            )
            .await?;
        }
        insert_blob_record(
            &mut tx,
            &normalized_hash,
            "application/vnd.lumi.normalized+json",
            &stored_normalized,
        )
        .await?;
        sqlx::query(
            "INSERT INTO document_revisions (revision_id, material_id, space_id, source_format, source_hash, importer_id, importer_version, created_at, normalized_hash, package_format_version, source_blob_hash, supersedes_revision_id) VALUES ($1, $2, $3, 'lum', $4, $5, $6, $7, $8, $9, $10, NULL)",
        )
        .bind(revision_id)
        .bind(material_id)
        .bind(space_id)
        .bind(&publication.revision.source_hash)
        .bind(&publication.revision.importer_id)
        .bind(&publication.revision.importer_version)
        .bind(now)
        .bind(&publication.revision.normalized_hash)
        .bind(&publication.revision.package_format_version)
        .bind(&source_hash)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, revision_id, idempotency_key, attempt, max_attempts, cancellation_requested, error_code, started_at, finished_at, source_kind, worker_fence, created_at, updated_at) VALUES ($1, $2, $3, 'succeeded', 'committed', $4, $5, $6, $7, 1, 1, false, NULL, $8, $8, 'lum', 0, $8, $8)",
        )
        .bind(job_id)
        .bind(owner_id)
        .bind(space_id)
        .bind(serde_json::to_value(&source_ref).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(material_id)
        .bind(revision_id)
        .bind(format!("abridgement:{}", input.artifact_id))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let manifest_id = publication.resources_manifest.id;
        sqlx::query(
            "INSERT INTO blob_manifests (manifest_id, space_id, schema_version, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(manifest_id)
        .bind(space_id)
        .bind(&publication.resources_manifest.schema_version)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        insert_manifest_entry(
            &mut tx,
            manifest_id,
            &source_hash,
            &format!("source/{source_name}"),
            "source",
        )
        .await?;
        for (resource, _) in &stored_resources {
            insert_manifest_entry(
                &mut tx,
                manifest_id,
                &resource.content_hash,
                &resource.path,
                "resource",
            )
            .await?;
        }
        insert_manifest_entry(
            &mut tx,
            manifest_id,
            &normalized_hash,
            "normalized/package.json",
            "normalized_package",
        )
        .await?;
        sqlx::query(
            "INSERT INTO normalized_packages (package_id, revision_id, schema_version, payload, source_map, manifest_id, package_blob_hash, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(publication.package_id)
        .bind(revision_id)
        .bind(&publication.revision.package_format_version)
        .bind(publication.package_payload)
        .bind(publication.source_map)
        .bind(manifest_id)
        .bind(&normalized_hash)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "UPDATE materials SET active_revision_id = $2, latest_import_job_id = $3 WHERE material_id = $1",
        )
        .bind(material_id)
        .bind(revision_id)
        .bind(job_id)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "INSERT INTO material_derivations (derivation_id, user_id, space_id, derived_material_id, derived_revision_id, source_material_id, source_revision_id, kind, task_id, artifact_id, provenance_schema_version, source_refs) VALUES ($1, $2, $3, $4, $5, $6, $7, 'abridgement', $8, $9, $10, $11)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_id)
        .bind(space_id)
        .bind(material_id)
        .bind(revision_id)
        .bind(input.source_material_id)
        .bind(input.source_revision_id)
        .bind(input.task_id)
        .bind(input.artifact_id)
        .bind(GENERATED_LUM_PROVENANCE_VERSION)
        .bind(serde_json::to_value(&input.source_refs).map_err(|_| ImportServiceError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let activated = sqlx::query(
            "UPDATE ai_artifacts SET status = 'active', object_revision = object_revision + 1, updated_at = $3 WHERE artifact_id = $1 AND user_id = $2 AND kind = 'abridgement_artifact' AND status = 'candidate'",
        )
        .bind(input.artifact_id)
        .bind(owner_id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        if activated.rows_affected() != 1 {
            return Err(ImportServiceError::Conflict);
        }
        append_import_change(
            &mut tx,
            ImportChange {
                space_id,
                object_id: material_id,
                device_id,
                idempotency_key: &format!("abridgement:{}:publish", input.artifact_id),
                change_kind: "create",
                payload: serde_json::json!({
                    "kind": "lum",
                    "import_status": "ready",
                    "revision_id": revision_id,
                    "derived_from": input.source_material_id,
                }),
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(DerivedMaterialRelation {
            kind: "abridgement".to_owned(),
            source_material_id: input.source_material_id,
            source_revision_id: input.source_revision_id,
            task_id: input.task_id,
            artifact_id: input.artifact_id,
            source_changed: false,
            source_refs: input.source_refs,
        })
    }

    /// Build and ordinary-import an abridgement result before the fenced task
    /// completion is allowed to commit.
    pub(crate) async fn preflight_abridgement_completion(
        &self,
        owner_id: Uuid,
        request: &lumi_core::CompleteAiTaskRequest,
    ) -> Result<(), ImportServiceError> {
        let row = sqlx::query(
            "SELECT task.source_material_id, task.source_revision_id, task.prompt_version, task.output_schema_version, context.source_refs FROM ai_tasks task JOIN ai_runs run ON run.run_id = task.active_run_id AND run.task_id = task.task_id AND run.user_id = task.user_id JOIN ai_context_packs context ON context.context_pack_id = run.context_pack_id AND context.task_id = task.task_id AND context.user_id = task.user_id WHERE task.task_id = $1 AND task.user_id = $2 AND task.kind = 'abridgement' AND task.status = 'running' AND run.run_id = $3",
        )
        .bind(request.task_id)
        .bind(owner_id)
        .bind(request.run_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::Conflict)?;
        let payload: lumi_core::AbridgementArtifactPayload =
            serde_json::from_value(request.result.clone())
                .map_err(|_| ImportServiceError::BadRequest("abridgement result is invalid"))?;
        payload
            .validate()
            .map_err(|_| ImportServiceError::BadRequest("abridgement result is invalid"))?;
        let source_refs: Vec<lumi_core::SourceCitation> =
            serde_json::from_value(row.try_get("source_refs").map_err(log_storage_error)?)
                .map_err(|_| ImportServiceError::Unavailable)?;
        let source_material_id: Uuid = row
            .try_get("source_material_id")
            .map_err(log_storage_error)?;
        let source_revision_id: Uuid = row
            .try_get("source_revision_id")
            .map_err(log_storage_error)?;
        let book_id = format!("lumi-abridged-preflight-{}", request.run_id.simple());
        let package = build_generated_lum_package(GeneratedLumPackageRequest {
            book_id: &book_id,
            source_material_id,
            source_revision_id,
            task_id: request.task_id,
            artifact_id: request.run_id,
            prompt_version: &row
                .try_get::<String, _>("prompt_version")
                .map_err(log_storage_error)?,
            artifact_schema_version: &row
                .try_get::<String, _>("output_schema_version")
                .map_err(log_storage_error)?,
            payload: &payload,
            source_refs: &source_refs,
        })
        .map_err(|_| ImportServiceError::BadRequest("generated LUM assembly failed"))?;
        import_lum(
            LumImportRequest {
                owner_id,
                material_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                source_name: "abridgement-preflight.lum",
                source: &package,
            },
            LumLimits::web_v1(),
            || false,
        )
        .map(|_| ())
        .map_err(|_| ImportServiceError::BadRequest("generated LUM validation failed"))
    }

    /// Reconcile completed abridgement artifacts that survived a restart
    /// between task completion and atomic library publication.
    pub(crate) async fn recover_abridgements(&self) -> Result<(), ImportServiceError> {
        let candidates = sqlx::query(
            "SELECT artifact.user_id, artifact.artifact_id FROM ai_artifacts artifact JOIN ai_tasks task ON task.task_id = artifact.task_id AND task.user_id = artifact.user_id WHERE artifact.kind = 'abridgement_artifact' AND artifact.status = 'candidate' AND task.status = 'succeeded' ORDER BY artifact.created_at, artifact.artifact_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        for row in candidates {
            let owner_id: Uuid = row.try_get("user_id").map_err(log_storage_error)?;
            let artifact_id: Uuid = row.try_get("artifact_id").map_err(log_storage_error)?;
            match self
                .publish_abridgement_artifact(owner_id, artifact_id)
                .await
            {
                Ok(_) => {}
                Err(ImportServiceError::BadRequest(_)) => {
                    sqlx::query(
                        "UPDATE ai_artifacts SET status = 'rejected', object_revision = object_revision + 1, updated_at = now() WHERE artifact_id = $1 AND user_id = $2 AND status = 'candidate'",
                    )
                    .bind(artifact_id)
                    .bind(owner_id)
                    .execute(&self.pool)
                    .await
                    .map_err(log_storage_error)?;
                    tracing::warn!(%artifact_id, "invalid abridgement candidate rejected during recovery");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    async fn derivation_by_artifact(
        &self,
        owner_id: Uuid,
        artifact_id: Uuid,
    ) -> Result<Option<DerivedMaterialRelation>, ImportServiceError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let relation =
            derivation_by_artifact_in_transaction(&mut tx, owner_id, artifact_id).await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(relation)
    }

    pub(crate) async fn recover(self: &Arc<Self>) -> Result<(), ImportServiceError> {
        self.recover_source_reservations().await?;
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let cancelled = sqlx::query(
            "UPDATE import_jobs SET status = 'cancelled', worker_claim_id = NULL, lease_expires_at = NULL, finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE status = 'reserving_source' AND cancellation_requested = true RETURNING result_material_id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        for row in cancelled {
            let material_id: Option<Uuid> = row
                .try_get("result_material_id")
                .map_err(log_storage_error)?;
            if let Some(material_id) = material_id {
                sqlx::query("UPDATE materials SET import_status = 'cancelled', updated_at = now() WHERE material_id = $1")
                    .bind(material_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(log_storage_error)?;
            }
        }
        tx.commit().await.map_err(log_storage_error)?;
        self.job_runtime
            .recover_expired(None)
            .await
            .map_err(|_| ImportServiceError::Unavailable)?;

        // Feature-specific projection reconciliation remains in ImportService;
        // claim/lease/retry/cancel decisions above belong to the common runtime.
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        sqlx::query(
            "UPDATE materials m SET import_status = j.status, updated_at = now() FROM import_jobs j WHERE j.result_material_id = m.material_id AND j.status IN ('failed', 'cancelled') AND m.import_status IS DISTINCT FROM j.status",
        )
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "INSERT INTO import_diagnostics (job_id, severity, code, message, source_path, attempt) SELECT j.job_id, 'error', 'import_retry_exhausted', 'Import recovery exhausted the configured retry budget.', NULL, GREATEST(j.attempt, 1) FROM import_jobs j WHERE j.status = 'failed' AND j.error_code = 'import_retry_exhausted' AND NOT EXISTS (SELECT 1 FROM import_diagnostics d WHERE d.job_id = j.job_id AND d.code = 'import_retry_exhausted' AND d.attempt = GREATEST(j.attempt, 1))",
        )
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        tx.commit().await.map_err(log_storage_error)?;
        let pending: Vec<Uuid> = sqlx::query_scalar(
            "SELECT job_id FROM import_jobs WHERE status = 'queued' AND cancellation_requested = false ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        for job_id in pending {
            self.spawn(job_id);
        }
        Ok(())
    }

    async fn recover_source_reservations(&self) -> Result<(), ImportServiceError> {
        loop {
            let claim_id = Uuid::now_v7();
            let row = sqlx::query(
                "WITH candidate AS (SELECT job_id FROM import_jobs WHERE status = 'reserving_source' AND cancellation_requested = false AND (lease_expires_at IS NULL OR lease_expires_at < now()) ORDER BY created_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE import_jobs j SET worker_claim_id = $1, lease_expires_at = now() + $2::interval, updated_at = now(), object_revision = object_revision + 1 FROM candidate WHERE j.job_id = candidate.job_id RETURNING j.job_id, j.result_material_id, j.source_kind, j.source_ref",
            )
            .bind(claim_id)
            .bind(SOURCE_RESERVATION_LEASE_SQL)
            .fetch_optional(&self.pool)
            .await
            .map_err(log_storage_error)?;
            let Some(row) = row else {
                break;
            };
            let job_id: Uuid = row.try_get("job_id").map_err(log_storage_error)?;
            let material_id: Uuid = row
                .try_get("result_material_id")
                .map_err(log_storage_error)?;
            let source_kind: String = row.try_get("source_kind").map_err(log_storage_error)?;
            let source_ref =
                decode_source_ref(row.try_get("source_ref").map_err(log_storage_error)?)?;
            let Some(hash) = source_ref.source_blob_hash() else {
                self.fail_reserved_source(
                    job_id,
                    claim_id,
                    material_id,
                    source_unavailable_diagnostic(&source_kind),
                )
                .await?;
                continue;
            };
            match self.blobs.get(hash).await {
                Ok(bytes) => {
                    let media_type = source_ref.source_media_type();
                    self.persist_blob_parts(hash, media_type, &bytes).await?;
                    self.activate_reserved_source(job_id, claim_id).await?;
                }
                Err(BlobStoreError::NotFound | BlobStoreError::HashMismatch) => {
                    self.fail_reserved_source(
                        job_id,
                        claim_id,
                        material_id,
                        source_unavailable_diagnostic(&source_kind),
                    )
                    .await?;
                }
                Err(error) => return Err(map_blob_error(error)),
            }
        }
        Ok(())
    }

    pub(crate) async fn accept(
        self: &Arc<Self>,
        session: &AuthenticatedSession,
        file_name: &str,
        idempotency_key: &str,
        source: Vec<u8>,
    ) -> Result<AcceptedImport, ImportServiceError> {
        let file_name = safe_file_name(file_name)?;
        let upload_kind = FileUploadKind::detect(&file_name, &source)?;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(ImportServiceError::BadRequest(
                "Idempotency-Key must contain 1 to 200 characters",
            ));
        }
        if source.is_empty() {
            return Err(ImportServiceError::BadRequest("document upload is empty"));
        }
        let source_len = u64::try_from(source.len()).unwrap_or(u64::MAX);
        if source_len > upload_kind.source_limit() {
            return Err(ImportServiceError::BadRequest(
                "document upload exceeds the configured source limit",
            ));
        }
        let source_hash = content_hash(&source);
        let request_hash = Sha256::digest(&source);
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        lock_idempotency_key(&mut tx, space_id, idempotency_key).await?;
        if let Some(row) = sqlx::query(
            "SELECT operation, request_hash, response_body FROM idempotency_keys WHERE scope_id = $1 AND idempotency_key = $2",
        )
        .bind(space_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        {
            let stored_operation: String = row.try_get("operation").map_err(log_storage_error)?;
            if stored_operation != "import.upload" {
                return Err(ImportServiceError::Conflict);
            }
            let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(log_storage_error)?;
            if stored_hash.as_slice() != request_hash.as_slice() {
                return Err(ImportServiceError::Conflict);
            }
            let response: serde_json::Value = row.try_get("response_body").map_err(log_storage_error)?;
            return serde_json::from_value(response).map_err(|_| ImportServiceError::Unavailable);
        }
        enforce_account_backpressure(&mut tx, session.user_id).await?;

        let material_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let reservation_claim_id = Uuid::now_v7();
        let title = upload_title(&file_name);
        let source_identity = serde_json::json!({
            "format": upload_kind.source_kind(),
            "source_name": file_name,
            "source_hash": source_hash,
        });
        sqlx::query(
            "INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'active', $6, 'queued', $7, $7)",
        )
        .bind(material_id)
        .bind(space_id)
        .bind(session.user_id)
        .bind(upload_kind.source_kind())
        .bind(&title)
        .bind(source_identity)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let source_ref = match upload_kind {
            FileUploadKind::Epub => SourceRef::Epub {
                blob_hash: source_hash.clone(),
                file_name: file_name.clone(),
                media_type: EPUB_SOURCE_MEDIA_TYPE.to_owned(),
                device_id: session.device_id,
            },
            FileUploadKind::Pdf => SourceRef::Pdf {
                blob_hash: source_hash.clone(),
                file_name: file_name.clone(),
                media_type: PDF_SOURCE_MEDIA_TYPE.to_owned(),
                device_id: session.device_id,
            },
            FileUploadKind::Markdown => SourceRef::Markdown {
                blob_hash: source_hash.clone(),
                file_name: file_name.clone(),
                media_type: MARKDOWN_SOURCE_MEDIA_TYPE.to_owned(),
                device_id: session.device_id,
            },
            FileUploadKind::Lum => SourceRef::Lum {
                blob_hash: source_hash.clone(),
                file_name: file_name.clone(),
                media_type: LUM_SOURCE_MEDIA_TYPE.to_owned(),
                device_id: session.device_id,
            },
        };
        sqlx::query(
            "INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, worker_claim_id, lease_expires_at, created_at, updated_at) VALUES ($1, $2, $3, 'reserving_source', 'source_accepted', $4, $5, $6, $7, $8, $9 + $10::interval, $9, $9)",
        )
        .bind(job_id)
        .bind(session.user_id)
        .bind(space_id)
        .bind(serde_json::to_value(&source_ref).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(material_id)
        .bind(idempotency_key)
        .bind(upload_kind.source_kind())
        .bind(reservation_claim_id)
        .bind(now)
        .bind(SOURCE_RESERVATION_LEASE_SQL)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query("UPDATE materials SET latest_import_job_id = $2 WHERE material_id = $1")
            .bind(material_id)
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
        append_import_change(
            &mut tx,
            ImportChange {
                space_id,
                object_id: material_id,
                device_id: session.device_id,
                idempotency_key: &format!("{idempotency_key}:material"),
                change_kind: "create",
                payload: serde_json::json!({
                    "kind": upload_kind.source_kind(),
                    "import_status": "queued"
                }),
                now,
            },
        )
        .await?;
        let job = Job {
            id: job_id,
            account_id: session.user_id,
            kind: JobKind::Import,
            status: JobStatus::Queued,
            stage: JobStage::SourceAccepted,
            material_id: Some(material_id),
            revision_id: None,
            diagnostics: Vec::new(),
            created_at: timestamp_ms(now),
            updated_at: timestamp_ms(now),
        };
        let accepted = AcceptedImport { material_id, job };
        sqlx::query(
            "INSERT INTO idempotency_keys (scope_id, idempotency_key, operation, request_hash, response_status, response_body, created_at) VALUES ($1, $2, 'import.upload', $3, 202, $4, $5)",
        )
        .bind(space_id)
        .bind(idempotency_key)
        .bind(request_hash.as_slice())
        .bind(serde_json::to_value(&accepted).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        tx.commit().await.map_err(log_storage_error)?;
        let reservation_heartbeat = self.spawn_reservation_heartbeat(job_id, reservation_claim_id);
        let pending_blob = PendingBlob {
            hash: source_hash,
            media_type: upload_kind.media_type(),
            bytes: source,
        };
        match self.persist_reserved_blob(&pending_blob).await {
            Ok(()) => {
                reservation_heartbeat.abort();
                self.activate_reserved_source(job_id, reservation_claim_id)
                    .await?;
                self.spawn(job_id);
            }
            Err(error) => {
                reservation_heartbeat.abort();
                tracing::error!(%job_id, %error, "reserved document source could not be stored");
                self.fail_reserved_source(
                    job_id,
                    reservation_claim_id,
                    material_id,
                    source_unavailable_diagnostic(upload_kind.source_kind()),
                )
                .await?;
            }
        }
        Ok(accepted)
    }

    pub(crate) async fn accept_web(
        self: &Arc<Self>,
        session: &AuthenticatedSession,
        raw_url: &str,
        idempotency_key: &str,
    ) -> Result<AcceptedImport, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let url = crate::web::validate_public_url_input(raw_url)
            .map_err(|_| ImportServiceError::BadRequest("URL must be a public HTTP(S) address"))?;
        let parsed = url::Url::parse(&url).map_err(|_| ImportServiceError::Unavailable)?;
        let title = parsed.host_str().unwrap_or("Web article").to_owned();
        let request_hash = Sha256::digest(url.as_bytes());
        self.accept_pending_source(
            session,
            idempotency_key,
            "web_page",
            title,
            SourceIdentity {
                format: lumi_core::SourceFormat::WebPage,
                source_name: url.clone(),
                source_hash: content_hash(url.as_bytes()),
            },
            SourceRef::WebPage {
                url,
                snapshot_blob_hash: None,
                device_id: session.device_id,
            },
            request_hash.as_slice(),
            "import.web_url",
            None,
        )
        .await
    }

    pub(crate) async fn accept_telegram_composite(
        self: &Arc<Self>,
        session: &AuthenticatedSession,
        envelope: &TelegramUpdate,
        idempotency_key: &str,
    ) -> Result<AcceptedImport, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        if !envelope.has_importable_content() {
            return Err(ImportServiceError::BadRequest(
                "Telegram envelope contains no importable content",
            ));
        }
        let bytes = serde_json::to_vec(envelope).map_err(|_| ImportServiceError::Unavailable)?;
        let source_hash = content_hash(&bytes);
        let title = envelope
            .forward_origin
            .clone()
            .or_else(|| {
                envelope
                    .text
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(|text| text.chars().take(80).collect())
            })
            .unwrap_or_else(|| "Материал из Telegram".to_owned());
        let request_hash = Sha256::digest(&bytes);
        let image_blobs = envelope
            .photos
            .iter()
            .cloned()
            .map(|descriptor| TelegramImageArtifact {
                descriptor,
                blob_hash: None,
                media_type: None,
                diagnostic: None,
            })
            .collect();
        let web_snapshots = envelope
            .links
            .iter()
            .cloned()
            .map(|url| TelegramWebArtifact {
                url,
                snapshot_blob_hash: None,
                diagnostic: None,
            })
            .collect();
        self.accept_pending_source(
            session,
            idempotency_key,
            "telegram",
            title,
            SourceIdentity {
                format: lumi_core::SourceFormat::Telegram,
                source_name: format!("telegram:{}/{}", envelope.chat_id, envelope.message_id),
                source_hash: source_hash.clone(),
            },
            SourceRef::TelegramComposite {
                message_blob_hash: source_hash.clone(),
                image_blobs,
                web_snapshots,
                bot_id: envelope.bot_id,
                device_id: session.device_id,
            },
            request_hash.as_slice(),
            "import.telegram_composite",
            Some(PendingBlob {
                hash: source_hash,
                media_type: "application/vnd.lumi.telegram-envelope+json",
                bytes,
            }),
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the pending import command explicitly carries its durable source provenance"
    )]
    async fn accept_pending_source(
        self: &Arc<Self>,
        session: &AuthenticatedSession,
        idempotency_key: &str,
        material_kind: &str,
        title: String,
        source_identity: SourceIdentity,
        source_ref: SourceRef,
        request_hash: &[u8],
        operation: &str,
        pending_blob: Option<PendingBlob>,
    ) -> Result<AcceptedImport, ImportServiceError> {
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        lock_idempotency_key(&mut tx, space_id, idempotency_key).await?;
        if let Some(row) = sqlx::query(
            "SELECT operation, request_hash, response_body FROM idempotency_keys WHERE scope_id = $1 AND idempotency_key = $2",
        )
        .bind(space_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        {
            let stored_operation: String = row.try_get("operation").map_err(log_storage_error)?;
            if stored_operation != operation {
                return Err(ImportServiceError::Conflict);
            }
            let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(log_storage_error)?;
            if stored_hash != request_hash {
                return Err(ImportServiceError::Conflict);
            }
            let response: serde_json::Value = row.try_get("response_body").map_err(log_storage_error)?;
            return serde_json::from_value(response).map_err(|_| ImportServiceError::Unavailable);
        }
        enforce_account_backpressure(&mut tx, session.user_id).await?;
        let material_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let reservation_claim_id = pending_blob.as_ref().map(|_| Uuid::now_v7());
        sqlx::query(
            "INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, 'active', $6, 'queued', $7, $7)",
        )
        .bind(material_id)
        .bind(space_id)
        .bind(session.user_id)
        .bind(material_kind)
        .bind(&title)
        .bind(serde_json::to_value(&source_identity).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let job_status = if pending_blob.is_some() {
            "reserving_source"
        } else {
            "queued"
        };
        sqlx::query(
            "INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, worker_claim_id, lease_expires_at, created_at, updated_at) VALUES ($1, $2, $3, $4, 'source_accepted', $5, $6, $7, $8, $9, CASE WHEN $4 = 'reserving_source' THEN $10 + $11::interval ELSE NULL END, $10, $10)",
        )
        .bind(job_id)
        .bind(session.user_id)
        .bind(space_id)
        .bind(job_status)
        .bind(serde_json::to_value(&source_ref).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(material_id)
        .bind(idempotency_key)
        .bind(material_kind)
        .bind(reservation_claim_id)
        .bind(now)
        .bind(SOURCE_RESERVATION_LEASE_SQL)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query("UPDATE materials SET latest_import_job_id = $2 WHERE material_id = $1")
            .bind(material_id)
            .bind(job_id)
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
        append_import_change(
            &mut tx,
            ImportChange {
                space_id,
                object_id: material_id,
                device_id: session.device_id,
                idempotency_key: &format!("{idempotency_key}:material"),
                change_kind: "create",
                payload: serde_json::json!({ "kind": material_kind, "import_status": "queued" }),
                now,
            },
        )
        .await?;
        let accepted = AcceptedImport {
            material_id,
            job: Job {
                id: job_id,
                account_id: session.user_id,
                kind: JobKind::Import,
                status: JobStatus::Queued,
                stage: JobStage::SourceAccepted,
                material_id: Some(material_id),
                revision_id: None,
                diagnostics: Vec::new(),
                created_at: timestamp_ms(now),
                updated_at: timestamp_ms(now),
            },
        };
        sqlx::query(
            "INSERT INTO idempotency_keys (scope_id, idempotency_key, operation, request_hash, response_status, response_body, created_at) VALUES ($1, $2, $3, $4, 202, $5, $6)",
        )
        .bind(space_id)
        .bind(idempotency_key)
        .bind(operation)
        .bind(request_hash)
        .bind(serde_json::to_value(&accepted).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        tx.commit().await.map_err(log_storage_error)?;
        if let Some(blob) = pending_blob {
            let reservation_claim_id =
                reservation_claim_id.ok_or(ImportServiceError::Unavailable)?;
            let reservation_heartbeat =
                self.spawn_reservation_heartbeat(job_id, reservation_claim_id);
            match self.persist_reserved_blob(&blob).await {
                Ok(()) => {
                    reservation_heartbeat.abort();
                    self.activate_reserved_source(job_id, reservation_claim_id)
                        .await?;
                    self.spawn(job_id);
                }
                Err(error) => {
                    reservation_heartbeat.abort();
                    tracing::error!(%job_id, %error, "reserved source could not be stored");
                    self.fail_reserved_source(
                        job_id,
                        reservation_claim_id,
                        material_id,
                        source_unavailable_diagnostic(material_kind),
                    )
                    .await?;
                }
            }
        } else {
            self.spawn(job_id);
        }
        Ok(accepted)
    }

    pub(crate) async fn list(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ImportStatusEntry>, ImportServiceError> {
        let rows = sqlx::query(
            "SELECT m.material_id, m.owner_user_id, m.kind, m.canonical_title, m.title_override, m.active_revision_id, m.library_state, m.source_identity, m.import_status, m.created_at, m.updated_at, j.job_id, (SELECT jsonb_build_object('kind', d.kind, 'source_material_id', d.source_material_id, 'source_revision_id', d.source_revision_id, 'task_id', d.task_id, 'artifact_id', d.artifact_id, 'source_changed', d.source_changed OR source.active_revision_id IS DISTINCT FROM d.source_revision_id, 'source_refs', d.source_refs) FROM material_derivations d JOIN materials source ON source.material_id = d.source_material_id AND source.owner_user_id = d.user_id WHERE d.derived_material_id = m.material_id AND d.user_id = m.owner_user_id) AS derivation FROM materials m JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL ORDER BY m.updated_at DESC, m.material_id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let job_ids = rows
            .iter()
            .map(|row| row.try_get("job_id").map_err(log_storage_error))
            .collect::<Result<Vec<Uuid>, _>>()?;
        let mut jobs = self.jobs_by_id(user_id, &job_ids).await?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let job_id: Uuid = row.try_get("job_id").map_err(log_storage_error)?;
            let latest_job = jobs
                .remove(&job_id)
                .ok_or(ImportServiceError::Unavailable)?;
            entries.push(library_entry_from_row(&row, latest_job)?);
        }
        Ok(entries)
    }

    async fn jobs_by_id(
        &self,
        user_id: Uuid,
        job_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Job>, ImportServiceError> {
        if job_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let diagnostic_rows = sqlx::query(
            "SELECT d.job_id, d.severity, d.code, d.message, d.source_path FROM import_diagnostics d JOIN import_jobs j ON j.job_id = d.job_id WHERE j.user_id = $1 AND d.job_id = ANY($2) ORDER BY d.job_id, d.diagnostic_id",
        )
        .bind(user_id)
        .bind(job_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let mut diagnostics = HashMap::<Uuid, Vec<ImportDiagnostic>>::new();
        for row in diagnostic_rows {
            let job_id = row.try_get("job_id").map_err(log_storage_error)?;
            diagnostics
                .entry(job_id)
                .or_default()
                .push(diagnostic_from_row(row)?);
        }
        let rows = sqlx::query(
            "SELECT job_id, user_id, status, stage, result_material_id, revision_id, created_at, updated_at FROM import_jobs WHERE user_id = $1 AND job_id = ANY($2)",
        )
        .bind(user_id)
        .bind(job_ids)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        rows.into_iter()
            .map(|row| {
                let job_id = row.try_get("job_id").map_err(log_storage_error)?;
                let job = job_from_row(&row, diagnostics.remove(&job_id).unwrap_or_default())?;
                Ok((job_id, job))
            })
            .collect()
    }

    pub(crate) async fn continue_reading(
        &self,
        user_id: Uuid,
    ) -> Result<Option<ContinueReadingEntry>, ImportServiceError> {
        let row = sqlx::query(
            "SELECT m.material_id, m.owner_user_id, m.kind, m.canonical_title, m.title_override, m.active_revision_id, m.library_state, m.source_identity, m.import_status, m.created_at, m.updated_at, j.job_id, j.user_id AS job_user_id, j.status AS job_status, j.stage AS job_stage, j.result_material_id AS job_result_material_id, j.revision_id AS job_revision_id, j.created_at AS job_created_at, j.updated_at AS job_updated_at, rp.revision_id AS progress_revision_id, rp.locator AS progress_locator, rp.progress_fraction, rp.updated_at AS progress_updated_at, COALESCE((SELECT jsonb_agg(jsonb_build_object('severity', d.severity, 'code', d.code, 'message', d.message, 'source_path', d.source_path) ORDER BY d.diagnostic_id) FROM import_diagnostics d WHERE d.job_id = j.job_id), '[]'::jsonb) AS job_diagnostics, (SELECT jsonb_build_object('kind', derivation.kind, 'source_material_id', derivation.source_material_id, 'source_revision_id', derivation.source_revision_id, 'task_id', derivation.task_id, 'artifact_id', derivation.artifact_id, 'source_changed', derivation.source_changed OR source.active_revision_id IS DISTINCT FROM derivation.source_revision_id, 'source_refs', derivation.source_refs) FROM material_derivations derivation JOIN materials source ON source.material_id = derivation.source_material_id AND source.owner_user_id = derivation.user_id WHERE derivation.derived_material_id = m.material_id AND derivation.user_id = m.owner_user_id) AS derivation FROM reading_progress rp JOIN materials m ON m.material_id = rp.material_id AND m.space_id = rp.space_id JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL AND m.library_state = 'active' AND m.import_status = 'ready' AND rp.deleted_at IS NULL AND rp.progress_fraction > 0 AND rp.revision_id = m.active_revision_id ORDER BY rp.updated_at DESC, m.material_id DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let diagnostics = serde_json::from_value::<Vec<ImportDiagnostic>>(
            row.try_get("job_diagnostics").map_err(log_storage_error)?,
        )
        .map_err(|_| ImportServiceError::Unavailable)?;
        let latest_job = projected_job_from_row(&row, diagnostics)?;
        let entry = library_entry_from_row(&row, latest_job)?;
        let progress = ReadingProgress {
            material_id: entry.id,
            revision_id: row
                .try_get("progress_revision_id")
                .map_err(log_storage_error)?,
            locator: serde_json::from_value(
                row.try_get("progress_locator").map_err(log_storage_error)?,
            )
            .map_err(|_| ImportServiceError::Unavailable)?,
            progress_fraction: row
                .try_get("progress_fraction")
                .map_err(log_storage_error)?,
            updated_at: timestamp_ms(
                row.try_get("progress_updated_at")
                    .map_err(log_storage_error)?,
            ),
        };
        Ok(Some(ContinueReadingEntry { entry, progress }))
    }

    pub(crate) async fn material(
        &self,
        user_id: Uuid,
        material_id: MaterialId,
    ) -> Result<LibraryEntry, ImportServiceError> {
        let row = sqlx::query(
            "SELECT m.material_id, m.owner_user_id, m.kind, m.canonical_title, m.title_override, m.active_revision_id, m.library_state, m.source_identity, m.import_status, m.created_at, m.updated_at, j.job_id, (SELECT jsonb_build_object('kind', d.kind, 'source_material_id', d.source_material_id, 'source_revision_id', d.source_revision_id, 'task_id', d.task_id, 'artifact_id', d.artifact_id, 'source_changed', d.source_changed OR source.active_revision_id IS DISTINCT FROM d.source_revision_id, 'source_refs', d.source_refs) FROM material_derivations d JOIN materials source ON source.material_id = d.source_material_id AND source.owner_user_id = d.user_id WHERE d.derived_material_id = m.material_id AND d.user_id = m.owner_user_id) AS derivation FROM materials m JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.material_id = $1 AND m.owner_user_id = $2 AND m.deleted_at IS NULL",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let job_id: Uuid = row.try_get("job_id").map_err(log_storage_error)?;
        library_entry_from_row(&row, self.job(user_id, job_id).await?)
    }

    pub(crate) async fn update_library_state(
        &self,
        session: &AuthenticatedSession,
        material_id: MaterialId,
        library_state: LibraryState,
        idempotency_key: &str,
    ) -> Result<LibraryEntry, ImportServiceError> {
        if !matches!(library_state, LibraryState::Active | LibraryState::Archived) {
            return Err(ImportServiceError::BadRequest(
                "library-state accepts active or archived; use DELETE for deletion",
            ));
        }
        validate_idempotency_key(idempotency_key)?;
        let state_name = library_state_name(library_state);
        let payload = serde_json::json!({ "library_state": state_name });
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let row = sqlx::query(
            "SELECT space_id, object_revision FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(material_id)
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let space_id: Uuid = row.try_get("space_id").map_err(log_storage_error)?;
        let object_revision: i64 = row
            .try_get::<i64, _>("object_revision")
            .map_err(log_storage_error)?
            .saturating_add(1);
        if library_change_exists(&mut tx, space_id, material_id, idempotency_key, &payload).await? {
            tx.commit().await.map_err(log_storage_error)?;
            return self.material(session.user_id, material_id).await;
        }
        sqlx::query(
            "UPDATE materials SET library_state = $3, object_revision = $4, updated_at = $5 WHERE material_id = $1 AND owner_user_id = $2",
        )
        .bind(material_id)
        .bind(session.user_id)
        .bind(state_name)
        .bind(object_revision)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        append_library_change(
            &mut tx,
            LibraryChange {
                space_id,
                material_id,
                object_revision,
                device_id: session.device_id,
                idempotency_key,
                change_kind: "update",
                payload,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        self.material(session.user_id, material_id).await
    }

    pub(crate) async fn delete_material(
        &self,
        session: &AuthenticatedSession,
        material_id: MaterialId,
        idempotency_key: &str,
    ) -> Result<(), ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let payload = serde_json::json!({ "library_state": "deleted" });
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let row = sqlx::query(
            "SELECT space_id, object_revision FROM materials WHERE material_id = $1 AND owner_user_id = $2 FOR UPDATE",
        )
        .bind(material_id)
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let space_id: Uuid = row.try_get("space_id").map_err(log_storage_error)?;
        if library_change_exists(&mut tx, space_id, material_id, idempotency_key, &payload).await? {
            tx.commit().await.map_err(log_storage_error)?;
            return Ok(());
        }
        let object_revision = row
            .try_get::<i64, _>("object_revision")
            .map_err(log_storage_error)?
            .saturating_add(1);
        sqlx::query(
            "UPDATE materials SET library_state = 'deleted', object_revision = $3, deleted_at = COALESCE(deleted_at, $4), updated_at = $4 WHERE material_id = $1 AND owner_user_id = $2",
        )
        .bind(material_id)
        .bind(session.user_id)
        .bind(object_revision)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        append_library_change(
            &mut tx,
            LibraryChange {
                space_id,
                material_id,
                object_revision,
                device_id: session.device_id,
                idempotency_key,
                change_kind: "delete",
                payload,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)
    }

    pub(crate) async fn job(
        &self,
        user_id: Uuid,
        job_id: JobId,
    ) -> Result<Job, ImportServiceError> {
        let row = sqlx::query(
            "SELECT job_id, user_id, status, stage, result_material_id, revision_id, created_at, updated_at FROM import_jobs WHERE job_id = $1 AND user_id = $2",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let diagnostics = self.diagnostics(user_id, job_id).await?;
        job_from_row(&row, diagnostics)
    }

    pub(crate) async fn diagnostics(
        &self,
        user_id: Uuid,
        job_id: JobId,
    ) -> Result<Vec<ImportDiagnostic>, ImportServiceError> {
        let rows = sqlx::query(
            "SELECT d.severity, d.code, d.message, d.source_path FROM import_diagnostics d JOIN import_jobs j ON j.job_id = d.job_id WHERE d.job_id = $1 AND j.user_id = $2 ORDER BY d.attempt DESC, d.diagnostic_id",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        rows.into_iter().map(diagnostic_from_row).collect()
    }

    pub(crate) async fn cancel(
        &self,
        user_id: Uuid,
        job_id: JobId,
    ) -> Result<Job, ImportServiceError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let row = sqlx::query(
            "UPDATE import_jobs SET cancellation_requested = true, status = CASE WHEN status IN ('reserving_source', 'queued') THEN 'cancelled' ELSE status END, worker_claim_id = CASE WHEN status IN ('reserving_source', 'queued') THEN NULL ELSE worker_claim_id END, lease_expires_at = CASE WHEN status IN ('reserving_source', 'queued') THEN NULL ELSE lease_expires_at END, finished_at = CASE WHEN status IN ('reserving_source', 'queued') THEN now() ELSE finished_at END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('reserving_source', 'queued', 'running') RETURNING result_material_id, status",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::Conflict)?;
        let status: String = row.try_get("status").map_err(log_storage_error)?;
        if status == "cancelled" {
            let material_id: Option<Uuid> = row
                .try_get("result_material_id")
                .map_err(log_storage_error)?;
            if let Some(material_id) = material_id {
                sqlx::query("UPDATE materials SET import_status = 'cancelled', updated_at = now() WHERE material_id = $1")
                    .bind(material_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(log_storage_error)?;
            }
        }
        tx.commit().await.map_err(log_storage_error)?;
        if let Ok(flags) = self.cancellations.lock() {
            if let Some(flag) = flags.get(&job_id) {
                flag.store(true, Ordering::Release);
            }
        }
        self.job(user_id, job_id).await
    }

    pub(crate) async fn retry(
        self: &Arc<Self>,
        user_id: Uuid,
        job_id: JobId,
    ) -> Result<Job, ImportServiceError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let updated = sqlx::query(
            "UPDATE import_jobs SET status = 'queued', stage = 'source_accepted', cancellation_requested = false, error_code = NULL, started_at = NULL, finished_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('failed', 'cancelled') AND attempt < max_attempts RETURNING result_material_id",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::Conflict)?;
        let material_id: Option<Uuid> = updated
            .try_get("result_material_id")
            .map_err(log_storage_error)?;
        if let Some(material_id) = material_id {
            sqlx::query("UPDATE materials SET import_status = 'queued', updated_at = now() WHERE material_id = $1")
                .bind(material_id)
                .execute(&mut *tx)
                .await
                .map_err(log_storage_error)?;
        }
        tx.commit().await.map_err(log_storage_error)?;
        self.spawn(job_id);
        self.job(user_id, job_id).await
    }

    pub(crate) async fn source(
        &self,
        user_id: Uuid,
        material_id: MaterialId,
    ) -> Result<(String, String, Vec<u8>), ImportServiceError> {
        let value: serde_json::Value = sqlx::query_scalar(
            "SELECT j.source_ref FROM materials m JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.material_id = $1 AND m.owner_user_id = $2 AND m.deleted_at IS NULL",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let source_ref = decode_source_ref(value)?;
        let blob_hash = source_ref
            .source_blob_hash()
            .ok_or(ImportServiceError::Conflict)?;
        let bytes = self.blobs.get(blob_hash).await.map_err(map_blob_error)?;
        let (name, media_type) = match &source_ref {
            SourceRef::Epub {
                file_name,
                media_type,
                ..
            } => (file_name.clone(), media_type.clone()),
            SourceRef::Pdf {
                file_name,
                media_type,
                ..
            } => (file_name.clone(), media_type.clone()),
            SourceRef::Markdown {
                file_name,
                media_type,
                ..
            } => (file_name.clone(), media_type.clone()),
            SourceRef::Lum {
                file_name,
                media_type,
                ..
            } => (file_name.clone(), media_type.clone()),
            SourceRef::WebPage { .. } => (
                "snapshot.json".to_owned(),
                "application/vnd.lumi.web-snapshot+json".to_owned(),
            ),
            SourceRef::TelegramText { .. } => (
                "message.json".to_owned(),
                "application/vnd.lumi.telegram-message+json".to_owned(),
            ),
            SourceRef::TelegramComposite { .. } => (
                "envelope.json".to_owned(),
                "application/vnd.lumi.telegram-envelope+json".to_owned(),
            ),
        };
        Ok((name, media_type, bytes))
    }

    pub(crate) async fn revision(
        &self,
        user_id: Uuid,
        revision_id: DocumentRevisionId,
    ) -> Result<DocumentRevision, ImportServiceError> {
        let row = sqlx::query(
            "SELECT r.revision_id, r.material_id, r.source_hash, r.normalized_hash, r.importer_id, r.importer_version, r.package_format_version, r.supersedes_revision_id, r.created_at, p.payload FROM document_revisions r JOIN materials m ON m.material_id = r.material_id JOIN normalized_packages p ON p.revision_id = r.revision_id WHERE r.revision_id = $1 AND m.owner_user_id = $2",
        )
        .bind(revision_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let payload: serde_json::Value = row.try_get("payload").map_err(log_storage_error)?;
        let diagnostics = payload
            .get("diagnostics")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ImportServiceError::Unavailable)?
            .unwrap_or_default();
        Ok(DocumentRevision {
            id: row.try_get("revision_id").map_err(log_storage_error)?,
            material_id: row.try_get("material_id").map_err(log_storage_error)?,
            source_hash: row.try_get("source_hash").map_err(log_storage_error)?,
            normalized_hash: row
                .try_get::<Option<String>, _>("normalized_hash")
                .map_err(log_storage_error)?
                .ok_or(ImportServiceError::Unavailable)?,
            importer_id: row.try_get("importer_id").map_err(log_storage_error)?,
            importer_version: row.try_get("importer_version").map_err(log_storage_error)?,
            package_format_version: row
                .try_get::<Option<String>, _>("package_format_version")
                .map_err(log_storage_error)?
                .ok_or(ImportServiceError::Unavailable)?,
            supersedes_revision_id: row
                .try_get("supersedes_revision_id")
                .map_err(log_storage_error)?,
            created_at: timestamp_ms(row.try_get("created_at").map_err(log_storage_error)?),
            diagnostics,
        })
    }

    pub(crate) async fn package(
        &self,
        user_id: Uuid,
        revision_id: DocumentRevisionId,
    ) -> Result<NormalizedContentPackage, ImportServiceError> {
        let payload: serde_json::Value = sqlx::query_scalar(
            "SELECT p.payload FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id JOIN materials m ON m.material_id = r.material_id WHERE p.revision_id = $1 AND m.owner_user_id = $2",
        )
        .bind(revision_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        serde_json::from_value(payload).map_err(|_| ImportServiceError::Unavailable)
    }

    pub(crate) async fn reading_document(
        &self,
        user_id: Uuid,
        revision_id: DocumentRevisionId,
    ) -> Result<ReadingDocument, ImportServiceError> {
        let package = self.package(user_id, revision_id).await?;
        let revision = self.revision(user_id, revision_id).await?;
        Ok(reading_document_from_package(
            &package,
            revision.material_id,
        ))
    }

    pub(crate) async fn fixed_layout_package(
        &self,
        user_id: Uuid,
        revision_id: DocumentRevisionId,
    ) -> Result<FixedLayoutContentPackage, ImportServiceError> {
        let payload: serde_json::Value = sqlx::query_scalar(
            "SELECT p.payload FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id JOIN materials m ON m.material_id = r.material_id WHERE p.revision_id = $1 AND r.source_format = 'pdf' AND m.owner_user_id = $2",
        )
        .bind(revision_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        serde_json::from_value(payload).map_err(|_| ImportServiceError::Unavailable)
    }

    pub(crate) async fn page_fidelity_document(
        &self,
        user_id: Uuid,
        revision_id: DocumentRevisionId,
    ) -> Result<PageFidelityDocument, ImportServiceError> {
        let package = self.fixed_layout_package(user_id, revision_id).await?;
        let revision = self.revision(user_id, revision_id).await?;
        Ok(package.page_fidelity_document(revision.material_id))
    }

    pub(crate) async fn reader_settings(
        &self,
        user_id: Uuid,
    ) -> Result<ReaderSettings, ImportServiceError> {
        let value: Option<serde_json::Value> = sqlx::query_scalar(
            "SELECT rs.settings FROM reader_settings rs JOIN sync_spaces s ON s.space_id = rs.space_id WHERE rs.user_id = $1 AND s.owner_user_id = $1 AND s.deleted_at IS NULL",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?;
        value
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| ImportServiceError::Unavailable)
            .map(|settings| settings.unwrap_or_default())
    }

    pub(crate) async fn update_reader_settings(
        &self,
        session: &AuthenticatedSession,
        settings: ReaderSettings,
        idempotency_key: &str,
    ) -> Result<ReaderSettings, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let settings = settings.normalized();
        let payload =
            serde_json::to_value(settings).map_err(|_| ImportServiceError::Unavailable)?;
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        if reader_change_exists(
            &mut tx,
            space_id,
            session.user_id,
            "reader_settings",
            idempotency_key,
            &payload,
        )
        .await?
        {
            tx.commit().await.map_err(log_storage_error)?;
            return self.reader_settings(session.user_id).await;
        }
        let revision: i64 = sqlx::query_scalar(
            "INSERT INTO reader_settings (space_id, user_id, settings, object_revision, updated_at) VALUES ($1, $2, $3, 1, $4) ON CONFLICT (space_id, user_id) DO UPDATE SET settings = EXCLUDED.settings, object_revision = reader_settings.object_revision + 1, updated_at = EXCLUDED.updated_at RETURNING object_revision",
        )
        .bind(space_id)
        .bind(session.user_id)
        .bind(&payload)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        append_reader_change(
            &mut tx,
            ReaderChange {
                space_id,
                object_type: "reader_settings",
                object_id: session.user_id,
                object_revision: revision,
                device_id: session.device_id,
                idempotency_key,
                payload,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(settings)
    }

    pub(crate) async fn reading_progress(
        &self,
        user_id: Uuid,
        material_id: MaterialId,
    ) -> Result<Option<ReadingProgress>, ImportServiceError> {
        let row = sqlx::query(
            "SELECT rp.revision_id, rp.locator, rp.progress_fraction, rp.updated_at FROM reading_progress rp JOIN materials m ON m.material_id = rp.material_id AND m.space_id = rp.space_id WHERE rp.material_id = $1 AND m.owner_user_id = $2 AND m.deleted_at IS NULL AND rp.deleted_at IS NULL",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?;
        row.map(|row| {
            let locator = row
                .try_get::<serde_json::Value, _>("locator")
                .map_err(log_storage_error)?;
            Ok(ReadingProgress {
                material_id,
                revision_id: row.try_get("revision_id").map_err(log_storage_error)?,
                locator: serde_json::from_value(locator)
                    .map_err(|_| ImportServiceError::Unavailable)?,
                progress_fraction: row
                    .try_get::<f32, _>("progress_fraction")
                    .map_err(log_storage_error)?,
                updated_at: timestamp_ms(row.try_get("updated_at").map_err(log_storage_error)?),
            })
        })
        .transpose()
    }

    pub(crate) async fn move_reading_position(
        &self,
        session: &AuthenticatedSession,
        command: MoveReadingPositionCommand,
        idempotency_key: &str,
    ) -> Result<ReadingProgress, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let progress_fraction = if command.progress_fraction.is_finite() {
            command.progress_fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let payload = serde_json::json!({
            "revision_id": command.revision_id,
            "locator": command.locator,
            "progress_fraction": progress_fraction,
        });
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let row = sqlx::query(
            "SELECT active_revision_id FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND space_id = $3 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(command.material_id)
        .bind(session.user_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let active_revision_id: Option<Uuid> = row
            .try_get("active_revision_id")
            .map_err(log_storage_error)?;
        if active_revision_id != Some(command.revision_id)
            || command.locator.revision_id != command.revision_id
        {
            return Err(ImportServiceError::Conflict);
        }
        let package_row = sqlx::query(
            "SELECT p.payload, r.source_format FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id WHERE p.revision_id = $1 AND r.material_id = $2 AND r.space_id = $3",
        )
        .bind(command.revision_id)
        .bind(command.material_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let package_value: serde_json::Value =
            package_row.try_get("payload").map_err(log_storage_error)?;
        let source_format: String = package_row
            .try_get("source_format")
            .map_err(log_storage_error)?;
        if source_format == "pdf" {
            let package: FixedLayoutContentPackage = serde_json::from_value(package_value)
                .map_err(|_| ImportServiceError::Unavailable)?;
            validate_pdf_anchor(&package, &command.locator, false)?;
        } else {
            let package: NormalizedContentPackage = serde_json::from_value(package_value)
                .map_err(|_| ImportServiceError::Unavailable)?;
            let document = reading_document_from_package(&package, command.material_id);
            validate_progress_locator(&RenderPlan::from_document(&document), &command.locator)?;
        }
        if reader_change_exists(
            &mut tx,
            space_id,
            command.material_id,
            "reading_progress",
            idempotency_key,
            &payload,
        )
        .await?
        {
            tx.commit().await.map_err(log_storage_error)?;
            return self
                .reading_progress(session.user_id, command.material_id)
                .await?
                .ok_or(ImportServiceError::Unavailable);
        }
        let locator =
            serde_json::to_value(&command.locator).map_err(|_| ImportServiceError::Unavailable)?;
        let revision: i64 = sqlx::query_scalar(
            "INSERT INTO reading_progress (space_id, material_id, revision_id, locator, progress_fraction, object_revision, updated_at) VALUES ($1, $2, $3, $4, $5, 1, $6) ON CONFLICT (space_id, material_id) DO UPDATE SET revision_id = EXCLUDED.revision_id, locator = EXCLUDED.locator, progress_fraction = EXCLUDED.progress_fraction, object_revision = reading_progress.object_revision + 1, updated_at = EXCLUDED.updated_at, deleted_at = NULL RETURNING object_revision",
        )
        .bind(space_id)
        .bind(command.material_id)
        .bind(command.revision_id)
        .bind(locator)
        .bind(progress_fraction)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        append_reader_change(
            &mut tx,
            ReaderChange {
                space_id,
                object_type: "reading_progress",
                object_id: command.material_id,
                object_revision: revision,
                device_id: session.device_id,
                idempotency_key,
                payload,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(ReadingProgress {
            material_id: command.material_id,
            revision_id: command.revision_id,
            locator: command.locator,
            progress_fraction,
            updated_at: timestamp_ms(now),
        })
    }

    pub(crate) async fn annotations(
        &self,
        user_id: Uuid,
        material_id: MaterialId,
    ) -> Result<Vec<Annotation>, ImportServiceError> {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL)",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .map_err(log_storage_error)?;
        if !exists {
            return Err(ImportServiceError::NotFound);
        }
        let rows = sqlx::query(
            "SELECT a.annotation_id, a.material_id, a.revision_id, a.anchor, a.kind,
                    a.annotation_type, a.target_kind, a.status, a.title,
                    a.related_annotation_id, a.object_revision, a.created_at, a.updated_at,
                    COALESCE(
                        (SELECT jsonb_agg(t.tag ORDER BY t.ordinal)
                           FROM annotation_tags t
                          WHERE t.annotation_id = a.annotation_id),
                        '[]'::jsonb
                    ) AS tags
               FROM annotations a
               JOIN materials m
                 ON m.material_id = a.material_id
                AND m.space_id = a.space_id
              WHERE a.material_id = $1
                AND m.owner_user_id = $2
                AND m.deleted_at IS NULL
                AND a.deleted_at IS NULL
              ORDER BY a.created_at, a.annotation_id",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        rows.iter().map(annotation_from_row).collect()
    }

    pub(crate) async fn create_annotation(
        &self,
        session: &AuthenticatedSession,
        mut command: CreateAnnotationCommand,
        idempotency_key: &str,
    ) -> Result<Annotation, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        canonicalize_anchor(&mut command.anchor);
        command.normalize();
        command
            .validate()
            .map_err(|_| ImportServiceError::BadRequest("invalid Annotation v2 command"))?;
        validate_anchor_shape(command.revision_id, &command.anchor)?;
        let command_value =
            serde_json::to_value(&command).map_err(|_| ImportServiceError::Unavailable)?;
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        lock_idempotency_key(&mut tx, space_id, idempotency_key).await?;
        if let Some(annotation) =
            annotation_retry(&mut tx, space_id, idempotency_key, &command_value).await?
        {
            tx.commit().await.map_err(log_storage_error)?;
            return Ok(annotation);
        }
        let package_row = sqlx::query(
            "SELECT p.payload, r.source_format FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id JOIN materials m ON m.material_id = r.material_id AND m.space_id = r.space_id WHERE p.revision_id = $1 AND r.material_id = $2 AND m.owner_user_id = $3 AND m.active_revision_id = $1 AND m.deleted_at IS NULL",
        )
        .bind(command.revision_id)
        .bind(command.material_id)
        .bind(session.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let package_value: serde_json::Value =
            package_row.try_get("payload").map_err(log_storage_error)?;
        let source_format: String = package_row
            .try_get("source_format")
            .map_err(log_storage_error)?;
        if source_format == "pdf" {
            let package: FixedLayoutContentPackage = serde_json::from_value(package_value)
                .map_err(|_| ImportServiceError::Unavailable)?;
            validate_pdf_anchor(
                &package,
                &command.anchor,
                command.target.is_exact_selection(),
            )?;
        } else {
            let package: NormalizedContentPackage = serde_json::from_value(package_value)
                .map_err(|_| ImportServiceError::Unavailable)?;
            let document = reading_document_from_package(&package, command.material_id);
            validate_anchor_exact(&RenderPlan::from_document(&document), &command.anchor)?;
        }
        let row = sqlx::query(
            "SELECT active_revision_id FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND space_id = $3 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(command.material_id)
        .bind(session.user_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let active_revision_id: Option<Uuid> = row
            .try_get("active_revision_id")
            .map_err(log_storage_error)?;
        if active_revision_id != Some(command.revision_id) {
            return Err(ImportServiceError::Conflict);
        }
        validate_related_annotation(
            &mut tx,
            space_id,
            command.material_id,
            command.related_annotation_id,
        )
        .await?;
        validate_voice_attachment(&mut tx, session.user_id, &command.kind).await?;
        let annotation = Annotation::create(command, timestamp_ms(now));
        let anchor = serde_json::to_value(&annotation.anchor)
            .map_err(|_| ImportServiceError::Unavailable)?;
        let kind =
            serde_json::to_value(&annotation.kind).map_err(|_| ImportServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO annotations
                (annotation_id, space_id, material_id, revision_id, kind, anchor,
                 annotation_type, target_kind, status, title, related_annotation_id,
                 audio_attachment_id, payload_schema, object_revision, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12,
                     'lumi.annotation-payload.v2', 1, $13, $13)",
        )
        .bind(annotation.id)
        .bind(space_id)
        .bind(annotation.material_id)
        .bind(annotation.revision_id)
        .bind(kind)
        .bind(anchor)
        .bind(annotation.annotation_type.as_str())
        .bind(annotation.target.kind_str())
        .bind(annotation.status.as_str())
        .bind(annotation.title.as_deref())
        .bind(annotation.related_annotation_id)
        .bind(annotation.kind.audio_attachment_id())
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        replace_annotation_tags(&mut tx, annotation.id, &annotation.tags, now).await?;
        append_annotation_change(
            &mut tx,
            AnnotationChange {
                space_id,
                annotation: &annotation,
                base_revision: None,
                device_id: session.device_id,
                idempotency_key,
                change_kind: "create",
                command: command_value,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(annotation)
    }

    pub(crate) async fn update_annotation(
        &self,
        session: &AuthenticatedSession,
        mut command: UpdateAnnotationCommand,
        idempotency_key: &str,
    ) -> Result<Annotation, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        command.normalize();
        let command_value =
            serde_json::to_value(&command).map_err(|_| ImportServiceError::Unavailable)?;
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        lock_idempotency_key(&mut tx, space_id, idempotency_key).await?;
        if let Some(annotation) =
            annotation_retry(&mut tx, space_id, idempotency_key, &command_value).await?
        {
            tx.commit().await.map_err(log_storage_error)?;
            return Ok(annotation);
        }
        let owned: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND space_id = $3 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(command.material_id)
        .bind(session.user_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        if owned.is_none() {
            return Err(ImportServiceError::NotFound);
        }
        let current = sqlx::query(
            "SELECT anchor
               FROM annotations
              WHERE annotation_id = $1
                AND material_id = $2
                AND space_id = $3
                AND deleted_at IS NULL
              FOR UPDATE",
        )
        .bind(command.annotation_id)
        .bind(command.material_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let current_anchor: serde_json::Value =
            current.try_get("anchor").map_err(log_storage_error)?;
        let current_anchor: lumi_core::Anchor =
            serde_json::from_value(current_anchor).map_err(|_| ImportServiceError::Unavailable)?;
        command
            .validate(&current_anchor)
            .map_err(|_| ImportServiceError::BadRequest("invalid Annotation v2 command"))?;
        validate_related_annotation(
            &mut tx,
            space_id,
            command.material_id,
            command.related_annotation_id,
        )
        .await?;
        validate_voice_attachment(&mut tx, session.user_id, &command.kind).await?;
        let annotation_type = command.kind.annotation_type(&command.target);
        let row = sqlx::query(
            "UPDATE annotations
                SET kind = $1,
                    annotation_type = $2,
                    target_kind = $3,
                    status = $4,
                    title = $5,
                    related_annotation_id = $6,
                    audio_attachment_id = $7,
                    payload_schema = 'lumi.annotation-payload.v2',
                    object_revision = object_revision + 1,
                    updated_at = $8
              WHERE annotation_id = $9
                AND material_id = $10
                AND space_id = $11
                AND object_revision = $12
                AND deleted_at IS NULL
          RETURNING annotation_id, material_id, revision_id, anchor, kind,
                    annotation_type, target_kind, status, title,
                    related_annotation_id, object_revision, created_at, updated_at,
                    '[]'::jsonb AS tags",
        )
        .bind(serde_json::to_value(&command.kind).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(annotation_type.as_str())
        .bind(command.target.kind_str())
        .bind(command.status.as_str())
        .bind(command.title.as_deref())
        .bind(command.related_annotation_id)
        .bind(command.kind.audio_attachment_id())
        .bind(now)
        .bind(command.annotation_id)
        .bind(command.material_id)
        .bind(space_id)
        .bind(i64::try_from(command.expected_revision).map_err(|_| ImportServiceError::Conflict)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let mut annotation = match row {
            Some(row) => annotation_from_row(&row)?,
            None => {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM annotations WHERE annotation_id = $1 AND material_id = $2 AND space_id = $3 AND deleted_at IS NULL)",
                )
                .bind(command.annotation_id)
                .bind(command.material_id)
                .bind(space_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(log_storage_error)?;
                return Err(if exists {
                    ImportServiceError::Conflict
                } else {
                    ImportServiceError::NotFound
                });
            }
        };
        replace_annotation_tags(&mut tx, annotation.id, &command.tags, now).await?;
        annotation.tags = command.tags.clone();
        append_annotation_change(
            &mut tx,
            AnnotationChange {
                space_id,
                annotation: &annotation,
                base_revision: Some(command.expected_revision),
                device_id: session.device_id,
                idempotency_key,
                change_kind: "update",
                command: command_value,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(annotation)
    }

    pub(crate) async fn delete_annotation(
        &self,
        session: &AuthenticatedSession,
        command: DeleteAnnotationCommand,
        idempotency_key: &str,
    ) -> Result<Annotation, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        let command_value =
            serde_json::to_value(command).map_err(|_| ImportServiceError::Unavailable)?;
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let space_id: Uuid = sqlx::query_scalar(
            "SELECT space_id FROM sync_spaces WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(session.user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        lock_idempotency_key(&mut tx, space_id, idempotency_key).await?;
        if let Some(annotation) =
            annotation_retry(&mut tx, space_id, idempotency_key, &command_value).await?
        {
            tx.commit().await.map_err(log_storage_error)?;
            return Ok(annotation);
        }
        let owned: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND space_id = $3 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(command.material_id)
        .bind(session.user_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        if owned.is_none() {
            return Err(ImportServiceError::NotFound);
        }
        let row = sqlx::query(
            "UPDATE annotations
                SET object_revision = object_revision + 1,
                    updated_at = $1,
                    deleted_at = $1
              WHERE annotation_id = $2
                AND material_id = $3
                AND space_id = $4
                AND object_revision = $5
                AND deleted_at IS NULL
          RETURNING annotation_id, material_id, revision_id, anchor, kind,
                    annotation_type, target_kind, status, title,
                    related_annotation_id, object_revision, created_at, updated_at,
                    '[]'::jsonb AS tags",
        )
        .bind(now)
        .bind(command.annotation_id)
        .bind(command.material_id)
        .bind(space_id)
        .bind(i64::try_from(command.expected_revision).map_err(|_| ImportServiceError::Conflict)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let mut annotation = match row {
            Some(row) => annotation_from_row(&row)?,
            None => {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM annotations WHERE annotation_id = $1 AND material_id = $2 AND space_id = $3 AND deleted_at IS NULL)",
                )
                .bind(command.annotation_id)
                .bind(command.material_id)
                .bind(space_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(log_storage_error)?;
                return Err(if exists {
                    ImportServiceError::Conflict
                } else {
                    ImportServiceError::NotFound
                });
            }
        };
        annotation.tags = annotation_tags(&mut tx, annotation.id).await?;
        append_annotation_change(
            &mut tx,
            AnnotationChange {
                space_id,
                annotation: &annotation,
                base_revision: Some(command.expected_revision),
                device_id: session.device_id,
                idempotency_key,
                change_kind: "delete",
                command: command_value,
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)?;
        Ok(annotation)
    }

    pub(crate) async fn export_annotations(
        &self,
        user_id: Uuid,
        material_id: MaterialId,
    ) -> Result<AnnotationExport, ImportServiceError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
        let row = sqlx::query(
            "SELECT material_id, owner_user_id, kind, canonical_title, title_override, active_revision_id, library_state, source_identity, created_at FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let revision_id: Option<Uuid> = row
            .try_get("active_revision_id")
            .map_err(log_storage_error)?;
        let kind: String = row.try_get("kind").map_err(log_storage_error)?;
        let library_state: String = row.try_get("library_state").map_err(log_storage_error)?;
        let source_identity: serde_json::Value =
            row.try_get("source_identity").map_err(log_storage_error)?;
        let material = Material {
            id: row.try_get("material_id").map_err(log_storage_error)?,
            owner_id: row.try_get("owner_user_id").map_err(log_storage_error)?,
            kind: match kind.as_str() {
                "epub" => MaterialKind::Epub,
                "pdf" => MaterialKind::Pdf,
                "web_page" => MaterialKind::WebPage,
                "telegram" => MaterialKind::Telegram,
                "markdown" => MaterialKind::Markdown,
                "lum" => MaterialKind::Lum,
                _ => return Err(ImportServiceError::Unavailable),
            },
            canonical_title: row.try_get("canonical_title").map_err(log_storage_error)?,
            title_override: row.try_get("title_override").map_err(log_storage_error)?,
            active_revision_id: revision_id.ok_or(ImportServiceError::Conflict)?,
            library_state: match library_state.as_str() {
                "active" => LibraryState::Active,
                "archived" => LibraryState::Archived,
                _ => return Err(ImportServiceError::Unavailable),
            },
            source_identity: serde_json::from_value(source_identity)
                .map_err(|_| ImportServiceError::Unavailable)?,
            created_at: timestamp_ms(row.try_get("created_at").map_err(log_storage_error)?),
        };
        let rows = sqlx::query(
            "SELECT a.annotation_id, a.material_id, a.revision_id, a.anchor, a.kind,
                    a.annotation_type, a.target_kind, a.status, a.title,
                    a.related_annotation_id, a.object_revision, a.created_at, a.updated_at,
                    COALESCE(
                        (SELECT jsonb_agg(t.tag ORDER BY t.ordinal)
                           FROM annotation_tags t
                          WHERE t.annotation_id = a.annotation_id),
                        '[]'::jsonb
                    ) AS tags
               FROM annotations a
              WHERE a.material_id = $1
                AND a.space_id = (
                    SELECT space_id
                      FROM materials
                     WHERE material_id = $1 AND owner_user_id = $2
                )
                AND a.deleted_at IS NULL
              ORDER BY a.created_at, a.annotation_id",
        )
        .bind(material_id)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let annotations = rows
            .iter()
            .map(annotation_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let export = AnnotationExport::for_material(&material, &annotations);
        tx.commit().await.map_err(log_storage_error)?;
        Ok(export)
    }

    pub(crate) async fn resource(
        &self,
        user_id: Uuid,
        revision_id: DocumentRevisionId,
        content_hash: &str,
    ) -> Result<(String, Vec<u8>), ImportServiceError> {
        let media_type: String = sqlx::query_scalar(
            "SELECT b.media_type FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id JOIN materials m ON m.material_id = r.material_id JOIN blob_manifest_entries e ON e.manifest_id = p.manifest_id JOIN blobs b ON b.content_hash = e.content_hash WHERE p.revision_id = $1 AND m.owner_user_id = $2 AND e.content_hash = $3 AND e.role = 'resource'",
        )
        .bind(revision_id)
        .bind(user_id)
        .bind(content_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let bytes = self.blobs.get(content_hash).await.map_err(map_blob_error)?;
        Ok((media_type, bytes))
    }

    pub(crate) async fn manifest(
        &self,
        user_id: Uuid,
        manifest_id: Uuid,
    ) -> Result<BlobManifest, ImportServiceError> {
        let rows = sqlx::query(
            "SELECT b.content_hash, b.byte_length, b.media_type, e.logical_path, e.role FROM blob_manifests m JOIN normalized_packages p ON p.manifest_id = m.manifest_id JOIN document_revisions r ON r.revision_id = p.revision_id JOIN materials mt ON mt.material_id = r.material_id JOIN blob_manifest_entries e ON e.manifest_id = m.manifest_id JOIN blobs b ON b.content_hash = e.content_hash WHERE m.manifest_id = $1 AND mt.owner_user_id = $2 ORDER BY e.logical_path",
        )
        .bind(manifest_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        if rows.is_empty() {
            return Err(ImportServiceError::NotFound);
        }
        let blobs = rows
            .into_iter()
            .map(|row| {
                let role: String = row.try_get("role").map_err(log_storage_error)?;
                Ok(BlobRef {
                    hash: row.try_get("content_hash").map_err(log_storage_error)?,
                    name: row.try_get("logical_path").map_err(log_storage_error)?,
                    media_type: row.try_get("media_type").map_err(log_storage_error)?,
                    byte_len: u64::try_from(
                        row.try_get::<i64, _>("byte_length")
                            .map_err(log_storage_error)?,
                    )
                    .map_err(|_| ImportServiceError::Unavailable)?,
                    role: match role.as_str() {
                        "source" => BlobRole::Source,
                        "resource" => BlobRole::Resource,
                        "normalized_package" => BlobRole::NormalizedPackage,
                        _ => return Err(ImportServiceError::Unavailable),
                    },
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BlobManifest {
            id: manifest_id,
            schema_version: lumi_core::DOMAIN_SCHEMA_VERSION.to_owned(),
            blobs,
        })
    }

    fn spawn(self: &Arc<Self>, job_id: JobId) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = service.run(job_id).await {
                tracing::error!(%job_id, error = %error, "durable import worker failed");
            }
        });
    }

    fn spawn_heartbeat(
        self: &Arc<Self>,
        job_id: JobId,
        claim_id: Uuid,
        fence: u64,
        cancellation: Arc<AtomicBool>,
    ) -> tokio::task::JoinHandle<()> {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let claim = crate::jobs::JobFence {
                job_id,
                claim_id,
                fence,
            };
            loop {
                match service
                    .job_runtime
                    .heartbeat(None, &claim, std::time::Duration::from_secs(30 * 60))
                    .await
                {
                    Ok(cancelled) => {
                        if cancelled {
                            cancellation.store(true, Ordering::Release);
                        }
                    }
                    Err(JobRuntimeError::StaleClaim | JobRuntimeError::Conflict) => {
                        cancellation.store(true, Ordering::Release);
                        break;
                    }
                    Err(error) => {
                        tracing::warn!(%job_id, %claim_id, %error, "import heartbeat failed");
                    }
                }
                tokio::time::sleep(HEARTBEAT_INTERVAL).await;
            }
        })
    }

    fn spawn_reservation_heartbeat(
        self: &Arc<Self>,
        job_id: JobId,
        claim_id: Uuid,
    ) -> tokio::task::JoinHandle<()> {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let renewed = sqlx::query(
                    "UPDATE import_jobs SET lease_expires_at = now() + $3::interval, updated_at = now() WHERE job_id = $1 AND worker_claim_id = $2 AND status = 'reserving_source' AND cancellation_requested = false",
                )
                .bind(job_id)
                .bind(claim_id)
                .bind(SOURCE_RESERVATION_LEASE_SQL)
                .execute(&service.pool)
                .await;
                match renewed {
                    Ok(result) if result.rows_affected() == 1 => {}
                    Ok(_) => break,
                    Err(error) => {
                        tracing::warn!(%job_id, %claim_id, %error, "source reservation heartbeat failed");
                    }
                }
                tokio::time::sleep(SOURCE_RESERVATION_HEARTBEAT_INTERVAL).await;
            }
        })
    }

    async fn run(self: Arc<Self>, job_id: JobId) -> Result<(), ImportServiceError> {
        let _worker_slot = Arc::clone(&self.worker_slots)
            .acquire_owned()
            .await
            .map_err(|_| ImportServiceError::Unavailable)?;
        let claim = match self
            .job_runtime
            .claim(None, job_id, std::time::Duration::from_secs(30 * 60))
            .await
        {
            Ok(claim) => claim,
            Err(JobRuntimeError::Conflict | JobRuntimeError::NotFound) => return Ok(()),
            Err(_) => return Err(ImportServiceError::Unavailable),
        };
        let worker_claim = ImportWorkerClaim {
            job_id,
            claim_id: claim.claim.claim_id,
            fence: i64::try_from(claim.claim.fence).map_err(|_| ImportServiceError::Unavailable)?,
        };
        let user_id = claim.job.owner_id;
        let space_id = claim.job.space_id;
        let material_id: Uuid = claim
            .job
            .payload_ref
            .get("result_material_id")
            .and_then(Value::as_str)
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or(ImportServiceError::Unavailable)?;
        let attempt =
            i32::try_from(claim.job.attempt).map_err(|_| ImportServiceError::Unavailable)?;
        let source_ref = decode_source_ref(
            claim
                .job
                .payload_ref
                .get("source_ref")
                .cloned()
                .ok_or(ImportServiceError::Unavailable)?,
        )?;
        sqlx::query("UPDATE materials SET import_status = 'running', updated_at = now() WHERE material_id = $1")
            .bind(material_id)
            .execute(&self.pool)
            .await
            .map_err(log_storage_error)?;
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellations
            .lock()
            .map_err(|_| ImportServiceError::Unavailable)?
            .insert(job_id, Arc::clone(&cancellation));
        let heartbeat = self.spawn_heartbeat(
            job_id,
            worker_claim.claim_id,
            claim.claim.fence,
            Arc::clone(&cancellation),
        );

        let result = match source_ref {
            source_ref @ SourceRef::Epub { .. } => {
                self.run_epub(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
            source_ref @ SourceRef::Pdf { .. } => {
                self.run_pdf(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
            source_ref @ SourceRef::Markdown { .. } => {
                self.run_markdown(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
            source_ref @ SourceRef::Lum { .. } => {
                self.run_lum(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
            source_ref @ SourceRef::WebPage { .. } => {
                self.run_web(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
            source_ref @ SourceRef::TelegramText { .. } => {
                self.run_telegram(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
            source_ref @ SourceRef::TelegramComposite { .. } => {
                self.run_telegram_composite(
                    worker_claim,
                    user_id,
                    space_id,
                    material_id,
                    attempt,
                    source_ref,
                    Arc::clone(&cancellation),
                )
                .await
            }
        };
        heartbeat.abort();
        self.remove_cancellation(job_id);
        result
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the source adapter boundary"
    )]
    async fn run_epub(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let SourceRef::Epub {
            blob_hash,
            file_name,
            ..
        } = &source_ref
        else {
            return Err(ImportServiceError::Unavailable);
        };
        self.set_stage(claim, "validating_container").await?;
        let source = match self.blobs.get(blob_hash).await {
            Ok(source) => source,
            Err(_) => {
                return self
                    .fail(
                        claim,
                        material_id,
                        attempt,
                        source_unavailable_diagnostic("epub"),
                        false,
                    )
                    .await;
            }
        };
        self.persist_blob_parts(blob_hash, EPUB_SOURCE_MEDIA_TYPE, &source)
            .await?;
        self.set_stage(claim, "normalizing").await?;
        let revision_id = Uuid::now_v7();
        let source_name = file_name.clone();
        let worker_cancellation = Arc::clone(&cancellation);
        let imported = tokio::task::spawn_blocking(move || {
            import_epub(
                EpubImportRequest {
                    owner_id: user_id,
                    material_id,
                    revision_id,
                    source_name: &source_name,
                    source: &source,
                },
                EpubLimits::s1(),
                || worker_cancellation.load(Ordering::Acquire),
            )
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(imported) if !cancellation.load(Ordering::Acquire) => {
                self.persist_success(
                    claim,
                    space_id,
                    &source_ref,
                    attempt,
                    publication_from_epub(imported),
                )
                .await
            }
            Ok(_) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    EpubImportError::Cancelled.diagnostic(),
                    true,
                )
                .await
            }
            Err(error) => {
                let cancelled = matches!(error, EpubImportError::Cancelled);
                self.fail(claim, material_id, attempt, error.diagnostic(), cancelled)
                    .await
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the source adapter boundary"
    )]
    async fn run_markdown(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let SourceRef::Markdown {
            blob_hash,
            file_name,
            ..
        } = &source_ref
        else {
            return Err(ImportServiceError::Unavailable);
        };
        self.set_stage(claim, "normalizing").await?;
        let source = match self.blobs.get(blob_hash).await {
            Ok(source) => source,
            Err(_) => {
                return self
                    .fail(
                        claim,
                        material_id,
                        attempt,
                        source_unavailable_diagnostic("markdown"),
                        false,
                    )
                    .await;
            }
        };
        self.persist_blob_parts(blob_hash, MARKDOWN_SOURCE_MEDIA_TYPE, &source)
            .await?;
        let revision_id = Uuid::now_v7();
        let source_name = file_name.clone();
        let worker_cancellation = Arc::clone(&cancellation);
        let imported = tokio::task::spawn_blocking(move || {
            import_markdown(
                MarkdownImportRequest {
                    owner_id: user_id,
                    material_id,
                    revision_id,
                    source_name: &source_name,
                    source: &source,
                },
                MarkdownLimits::web_v1(),
                || worker_cancellation.load(Ordering::Acquire),
            )
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(imported) if !cancellation.load(Ordering::Acquire) => {
                self.persist_success(claim, space_id, &source_ref, attempt, imported)
                    .await
            }
            Ok(_) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    MarkdownImportError::Cancelled.diagnostic(),
                    true,
                )
                .await
            }
            Err(error) => {
                let cancelled = matches!(error, MarkdownImportError::Cancelled);
                self.fail(claim, material_id, attempt, error.diagnostic(), cancelled)
                    .await
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the source adapter boundary"
    )]
    async fn run_lum(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let SourceRef::Lum {
            blob_hash,
            file_name,
            ..
        } = &source_ref
        else {
            return Err(ImportServiceError::Unavailable);
        };
        self.set_stage(claim, "validating_container").await?;
        let source = match self.blobs.get(blob_hash).await {
            Ok(source) => source,
            Err(_) => {
                return self
                    .fail(
                        claim,
                        material_id,
                        attempt,
                        source_unavailable_diagnostic("lum"),
                        false,
                    )
                    .await;
            }
        };
        self.persist_blob_parts(blob_hash, LUM_SOURCE_MEDIA_TYPE, &source)
            .await?;
        self.set_stage(claim, "normalizing").await?;
        let revision_id = Uuid::now_v7();
        let source_name = file_name.clone();
        let worker_cancellation = Arc::clone(&cancellation);
        let imported = tokio::task::spawn_blocking(move || {
            import_lum(
                LumImportRequest {
                    owner_id: user_id,
                    material_id,
                    revision_id,
                    source_name: &source_name,
                    source: &source,
                },
                LumLimits::web_v1(),
                || worker_cancellation.load(Ordering::Acquire),
            )
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(imported) if !cancellation.load(Ordering::Acquire) => {
                self.persist_success(claim, space_id, &source_ref, attempt, imported)
                    .await
            }
            Ok(_) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    LumImportError::Cancelled.diagnostic(),
                    true,
                )
                .await
            }
            Err(error) => {
                let cancelled = matches!(error, LumImportError::Cancelled);
                self.fail(claim, material_id, attempt, error.diagnostic(), cancelled)
                    .await
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the source adapter boundary"
    )]
    async fn run_pdf(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let SourceRef::Pdf {
            blob_hash,
            file_name,
            ..
        } = &source_ref
        else {
            return Err(ImportServiceError::Unavailable);
        };
        self.set_stage(claim, "inspecting_document").await?;
        let source = match self.blobs.get(blob_hash).await {
            Ok(source) => source,
            Err(_) => {
                return self
                    .fail(
                        claim,
                        material_id,
                        attempt,
                        source_unavailable_diagnostic("pdf"),
                        false,
                    )
                    .await;
            }
        };
        self.persist_blob_parts(blob_hash, PDF_SOURCE_MEDIA_TYPE, &source)
            .await?;
        let revision_id = Uuid::now_v7();
        let source_name = file_name.clone();
        let engine = Arc::clone(&self.pdf_engine);
        let worker_cancellation = Arc::clone(&cancellation);
        let imported = tokio::task::spawn_blocking(move || {
            let inspected = engine.inspect(&source)?;
            normalize_pdf(
                PdfImportRequest {
                    owner_id: user_id,
                    material_id,
                    revision_id,
                    source_name: &source_name,
                    source: &source,
                },
                PdfLimits::web_v1(),
                inspected,
                || worker_cancellation.load(Ordering::Acquire),
            )
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(imported) if !cancellation.load(Ordering::Acquire) => {
                self.persist_pdf_success(claim, space_id, &source_ref, attempt, imported)
                    .await
            }
            Ok(_) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    lumi_core::PdfImportError::Cancelled.diagnostic(),
                    true,
                )
                .await
            }
            Err(error) => {
                let cancelled = matches!(error, lumi_core::PdfImportError::Cancelled);
                self.fail(claim, material_id, attempt, error.diagnostic(), cancelled)
                    .await
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the source adapter boundary"
    )]
    async fn run_web(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        mut source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let snapshot = if let SourceRef::WebPage {
            snapshot_blob_hash: Some(hash),
            ..
        } = &source_ref
        {
            let bytes = match self.blobs.get(hash).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    return self
                        .fail(
                            claim,
                            material_id,
                            attempt,
                            source_unavailable_diagnostic("web"),
                            false,
                        )
                        .await;
                }
            };
            self.persist_blob_parts(hash, "application/vnd.lumi.web-snapshot+json", &bytes)
                .await?;
            match serde_json::from_slice(&bytes) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    return self
                        .fail(
                            claim,
                            material_id,
                            attempt,
                            source_import_diagnostic("web", "stored snapshot is invalid"),
                            false,
                        )
                        .await;
                }
            }
        } else {
            let SourceRef::WebPage { url, .. } = &source_ref else {
                return Err(ImportServiceError::Unavailable);
            };
            self.set_stage(claim, "fetching_source").await?;
            let snapshot = match self.web_capture.capture(url).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    return self
                        .fail(claim, material_id, attempt, error.diagnostic(), false)
                        .await;
                }
            };
            self.set_stage(claim, "capturing_snapshot").await?;
            let bytes =
                serde_json::to_vec(&snapshot).map_err(|_| ImportServiceError::Unavailable)?;
            let hash = content_hash(&bytes);
            let stored = self
                .blobs
                .put(&hash, &bytes)
                .await
                .map_err(map_blob_error)?;
            let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
            insert_blob_record(
                &mut tx,
                &hash,
                "application/vnd.lumi.web-snapshot+json",
                &stored,
            )
            .await?;
            if let SourceRef::WebPage {
                snapshot_blob_hash, ..
            } = &mut source_ref
            {
                *snapshot_blob_hash = Some(hash);
            }
            let updated = sqlx::query(
                "UPDATE import_jobs SET source_ref = $4, lease_expires_at = now() + $5::interval, updated_at = now() WHERE job_id = $1 AND worker_claim_id = $2 AND worker_fence = $3 AND status = 'running' AND lease_expires_at > now()",
            )
            .bind(claim.job_id)
            .bind(claim.claim_id)
            .bind(claim.fence)
            .bind(serde_json::to_value(&source_ref).map_err(|_| ImportServiceError::Unavailable)?)
            .bind(WORKER_LEASE_SQL)
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
            if updated.rows_affected() != 1 {
                tx.rollback().await.map_err(log_storage_error)?;
                return Err(ImportServiceError::Conflict);
            }
            tx.commit().await.map_err(log_storage_error)?;
            snapshot
        };
        if cancellation.load(Ordering::Acquire) {
            return self
                .fail(claim, material_id, attempt, cancelled_diagnostic(), true)
                .await;
        }
        self.set_stage(claim, "extracting_content").await?;
        let imported = tokio::task::spawn_blocking(move || {
            import_web_snapshot(user_id, material_id, Uuid::now_v7(), &snapshot)
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(publication) if !cancellation.load(Ordering::Acquire) => {
                self.persist_success(claim, space_id, &source_ref, attempt, publication)
                    .await
            }
            Ok(_) => {
                self.fail(claim, material_id, attempt, cancelled_diagnostic(), true)
                    .await
            }
            Err(error) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    source_import_diagnostic("web", &error.to_string()),
                    false,
                )
                .await
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the source adapter boundary"
    )]
    async fn run_telegram(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let Some(hash) = source_ref.source_blob_hash() else {
            return Err(ImportServiceError::Unavailable);
        };
        let bytes = match self.blobs.get(hash).await {
            Ok(bytes) => bytes,
            Err(_) => {
                return self
                    .fail(
                        claim,
                        material_id,
                        attempt,
                        source_unavailable_diagnostic("telegram"),
                        false,
                    )
                    .await;
            }
        };
        self.persist_blob_parts(hash, "application/vnd.lumi.telegram-message+json", &bytes)
            .await?;
        let snapshot: TelegramMessageSnapshot = match serde_json::from_slice(&bytes) {
            Ok(snapshot) => snapshot,
            Err(_) => {
                return self
                    .fail(
                        claim,
                        material_id,
                        attempt,
                        source_import_diagnostic("telegram", "stored message snapshot is invalid"),
                        false,
                    )
                    .await;
            }
        };
        if cancellation.load(Ordering::Acquire) {
            return self
                .fail(claim, material_id, attempt, cancelled_diagnostic(), true)
                .await;
        }
        self.set_stage(claim, "normalizing").await?;
        let imported = tokio::task::spawn_blocking(move || {
            import_telegram_text(user_id, material_id, Uuid::now_v7(), &snapshot)
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(publication) if !cancellation.load(Ordering::Acquire) => {
                self.persist_success(claim, space_id, &source_ref, attempt, publication)
                    .await
            }
            Ok(_) => {
                self.fail(claim, material_id, attempt, cancelled_diagnostic(), true)
                    .await
            }
            Err(error) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    source_import_diagnostic("telegram", &error.to_string()),
                    false,
                )
                .await
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "worker context is explicit across the composite source adapter boundary"
    )]
    async fn run_telegram_composite(
        &self,
        claim: ImportWorkerClaim,
        user_id: Uuid,
        space_id: Uuid,
        material_id: Uuid,
        attempt: i32,
        mut source_ref: SourceRef,
        cancellation: Arc<AtomicBool>,
    ) -> Result<(), ImportServiceError> {
        let SourceRef::TelegramComposite {
            message_blob_hash,
            bot_id,
            ..
        } = &source_ref
        else {
            return Err(ImportServiceError::Unavailable);
        };
        let envelope_bytes = self
            .blobs
            .get(message_blob_hash)
            .await
            .map_err(map_blob_error)?;
        self.persist_blob_parts(
            message_blob_hash,
            "application/vnd.lumi.telegram-envelope+json",
            &envelope_bytes,
        )
        .await?;
        let envelope: TelegramUpdate =
            serde_json::from_slice(&envelope_bytes).map_err(|_| ImportServiceError::Unavailable)?;
        let bot_id = *bot_id;
        let mut diagnostics = Vec::new();

        self.set_stage(claim, "capturing_telegram_media").await?;
        let mut total_image_bytes = 0_usize;
        let image_count = match &source_ref {
            SourceRef::TelegramComposite { image_blobs, .. } => image_blobs.len(),
            _ => 0,
        };
        for index in 0..image_count {
            if cancellation.load(Ordering::Acquire) {
                return self
                    .fail(claim, material_id, attempt, cancelled_diagnostic(), true)
                    .await;
            }
            if index >= MAX_TELEGRAM_IMAGES {
                let diagnostic = telegram_part_diagnostic(
                    "telegram_image_count_limit",
                    "Telegram image was skipped because the material image limit was reached.",
                    format!("photo[{index}]"),
                );
                set_image_diagnostic(&mut source_ref, index, diagnostic.clone())?;
                diagnostics.push(diagnostic);
                self.persist_running_source_ref(claim, &source_ref).await?;
                continue;
            }
            let existing = match &source_ref {
                SourceRef::TelegramComposite { image_blobs, .. } => image_blobs[index]
                    .blob_hash
                    .as_ref()
                    .zip(image_blobs[index].media_type.as_ref())
                    .map(|(hash, media_type)| (hash.clone(), media_type.clone())),
                _ => None,
            };
            if let Some((hash, _)) = existing {
                let bytes = self.blobs.get(&hash).await.map_err(map_blob_error)?;
                if total_image_bytes.saturating_add(bytes.len()) > MAX_TELEGRAM_TOTAL_IMAGE_BYTES {
                    let diagnostic = telegram_part_diagnostic(
                        "telegram_image_total_limit",
                        "Telegram image was skipped because the material byte limit was reached.",
                        format!("photo[{index}]"),
                    );
                    set_image_diagnostic(&mut source_ref, index, diagnostic.clone())?;
                    diagnostics.push(diagnostic);
                } else {
                    total_image_bytes += bytes.len();
                }
                self.persist_running_source_ref(claim, &source_ref).await?;
                continue;
            }
            let descriptor = match &source_ref {
                SourceRef::TelegramComposite { image_blobs, .. } => {
                    image_blobs[index].descriptor.clone()
                }
                _ => return Err(ImportServiceError::Unavailable),
            };
            let capture = self.capture_telegram_photo(bot_id, &descriptor).await;
            match capture {
                Ok(captured)
                    if captured.bytes.len() <= MAX_TELEGRAM_IMAGE_BYTES
                        && total_image_bytes.saturating_add(captured.bytes.len())
                            <= MAX_TELEGRAM_TOTAL_IMAGE_BYTES =>
                {
                    let hash = content_hash(&captured.bytes);
                    self.persist_blob_parts(&hash, &captured.media_type, &captured.bytes)
                        .await?;
                    total_image_bytes += captured.bytes.len();
                    if let SourceRef::TelegramComposite { image_blobs, .. } = &mut source_ref {
                        image_blobs[index].blob_hash = Some(hash);
                        image_blobs[index].media_type = Some(captured.media_type);
                        image_blobs[index].diagnostic = None;
                    }
                }
                Ok(_) => {
                    let diagnostic = telegram_part_diagnostic(
                        "telegram_image_total_limit",
                        "Telegram image was skipped because the material byte limit was reached.",
                        format!("photo[{index}]"),
                    );
                    set_image_diagnostic(&mut source_ref, index, diagnostic.clone())?;
                    diagnostics.push(diagnostic);
                }
                Err(error) => {
                    let diagnostic = telegram_part_diagnostic(
                        "telegram_image_capture_failed",
                        &error.to_string(),
                        format!("photo[{index}]"),
                    );
                    set_image_diagnostic(&mut source_ref, index, diagnostic.clone())?;
                    diagnostics.push(diagnostic);
                }
            }
            self.persist_running_source_ref(claim, &source_ref).await?;
        }

        self.set_stage(claim, "fetching_linked_sources").await?;
        let web_jobs = match &source_ref {
            SourceRef::TelegramComposite { web_snapshots, .. } => web_snapshots
                .iter()
                .take(MAX_TELEGRAM_LINKS)
                .enumerate()
                .filter(|(_, artifact)| {
                    artifact.snapshot_blob_hash.is_none() && artifact.diagnostic.is_none()
                })
                .map(|(index, artifact)| (index, artifact.url.clone()))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        let semaphore = Arc::new(Semaphore::new(MAX_TELEGRAM_WEB_FETCHES));
        let mut captures = tokio::task::JoinSet::new();
        for (index, url) in web_jobs {
            let web_capture = Arc::clone(&self.web_capture);
            let semaphore = Arc::clone(&semaphore);
            captures.spawn(async move {
                let permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| ImportServiceError::Unavailable)?;
                let result = web_capture.capture(&url).await;
                drop(permit);
                Ok::<_, ImportServiceError>((index, result))
            });
        }
        while let Some(result) = captures.join_next().await {
            let (index, capture) = result.map_err(|_| ImportServiceError::Unavailable)??;
            match capture {
                Ok(snapshot) => {
                    let bytes = serde_json::to_vec(&snapshot)
                        .map_err(|_| ImportServiceError::Unavailable)?;
                    let hash = content_hash(&bytes);
                    self.persist_blob_parts(
                        &hash,
                        "application/vnd.lumi.web-snapshot+json",
                        &bytes,
                    )
                    .await?;
                    if let SourceRef::TelegramComposite { web_snapshots, .. } = &mut source_ref {
                        web_snapshots[index].snapshot_blob_hash = Some(hash);
                        web_snapshots[index].diagnostic = None;
                    }
                }
                Err(error) => {
                    let mut diagnostic = error.diagnostic();
                    diagnostic.source_path = Some(format!("links[{index}]"));
                    if let SourceRef::TelegramComposite { web_snapshots, .. } = &mut source_ref {
                        web_snapshots[index].diagnostic = Some(diagnostic.clone());
                    }
                    diagnostics.push(diagnostic);
                }
            }
            self.persist_running_source_ref(claim, &source_ref).await?;
        }
        if let SourceRef::TelegramComposite { web_snapshots, .. } = &mut source_ref {
            for (index, artifact) in web_snapshots
                .iter_mut()
                .enumerate()
                .skip(MAX_TELEGRAM_LINKS)
            {
                let diagnostic = telegram_part_diagnostic(
                    "telegram_link_count_limit",
                    "Telegram link was not expanded because the material link limit was reached.",
                    format!("links[{index}]"),
                );
                artifact.diagnostic = Some(diagnostic.clone());
                diagnostics.push(diagnostic);
            }
        }
        self.persist_running_source_ref(claim, &source_ref).await?;

        let (images, web_sections) = self
            .load_telegram_composite_artifacts(&source_ref, &mut diagnostics)
            .await?;
        self.set_stage(claim, "normalizing").await?;
        let imported = tokio::task::spawn_blocking(move || {
            import_telegram_composite(
                user_id,
                material_id,
                Uuid::now_v7(),
                &envelope,
                &images,
                &web_sections,
                &diagnostics,
            )
        })
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
        match imported {
            Ok(publication) if !cancellation.load(Ordering::Acquire) => {
                self.persist_success(claim, space_id, &source_ref, attempt, publication)
                    .await
            }
            Ok(_) => {
                self.fail(claim, material_id, attempt, cancelled_diagnostic(), true)
                    .await
            }
            Err(error) => {
                self.fail(
                    claim,
                    material_id,
                    attempt,
                    source_import_diagnostic("telegram", &error.to_string()),
                    false,
                )
                .await
            }
        }
    }

    async fn capture_telegram_photo(
        &self,
        bot_id: u64,
        descriptor: &TelegramPhotoDescriptor,
    ) -> Result<
        crate::telegram_media::CapturedTelegramMedia,
        crate::telegram_media::TelegramMediaError,
    > {
        let mut last_error = None;
        for retry in 0..3 {
            match self.telegram_media.capture(bot_id, descriptor).await {
                Ok(captured) => return Ok(captured),
                Err(error) if error.is_retryable() && retry < 2 => {
                    last_error = Some(error);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or(crate::telegram_media::TelegramMediaError::CredentialUnavailable))
    }

    async fn persist_running_source_ref(
        &self,
        claim: ImportWorkerClaim,
        source_ref: &SourceRef,
    ) -> Result<(), ImportServiceError> {
        let updated = sqlx::query(
            "UPDATE import_jobs SET source_ref = $4, lease_expires_at = now() + $5::interval, updated_at = now() WHERE job_id = $1 AND worker_claim_id = $2 AND worker_fence = $3 AND status = 'running' AND lease_expires_at > now()",
        )
        .bind(claim.job_id)
        .bind(claim.claim_id)
        .bind(claim.fence)
        .bind(serde_json::to_value(source_ref).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(WORKER_LEASE_SQL)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
        (updated.rows_affected() == 1)
            .then_some(())
            .ok_or(ImportServiceError::Conflict)
    }

    async fn load_telegram_composite_artifacts(
        &self,
        source_ref: &SourceRef,
        diagnostics: &mut Vec<ImportDiagnostic>,
    ) -> Result<(Vec<TelegramCapturedImage>, Vec<TelegramWebSection>), ImportServiceError> {
        let SourceRef::TelegramComposite {
            image_blobs,
            web_snapshots,
            ..
        } = source_ref
        else {
            return Err(ImportServiceError::Unavailable);
        };
        let mut images = Vec::new();
        let mut total_bytes = 0_usize;
        for artifact in image_blobs.iter().take(MAX_TELEGRAM_IMAGES) {
            if let Some(diagnostic) = artifact.diagnostic.as_ref() {
                diagnostics.push(diagnostic.clone());
            }
            let Some((hash, media_type)) = artifact
                .blob_hash
                .as_ref()
                .zip(artifact.media_type.as_ref())
            else {
                continue;
            };
            let bytes = self.blobs.get(hash).await.map_err(map_blob_error)?;
            if total_bytes.saturating_add(bytes.len()) > MAX_TELEGRAM_TOTAL_IMAGE_BYTES {
                continue;
            }
            total_bytes += bytes.len();
            images.push(TelegramCapturedImage {
                descriptor: artifact.descriptor.clone(),
                media_type: media_type.clone(),
                content_hash: hash.clone(),
                bytes,
            });
        }
        let mut sections = Vec::with_capacity(web_snapshots.len());
        for artifact in web_snapshots {
            let snapshot = if let Some(hash) = artifact.snapshot_blob_hash.as_ref() {
                let bytes = self.blobs.get(hash).await.map_err(map_blob_error)?;
                Some(serde_json::from_slice(&bytes).map_err(|_| ImportServiceError::Unavailable)?)
            } else {
                None
            };
            sections.push(TelegramWebSection {
                url: artifact.url.clone(),
                snapshot,
                diagnostics: artifact.diagnostic.iter().cloned().collect(),
            });
        }
        Ok((images, sections))
    }

    async fn set_stage(
        &self,
        claim: ImportWorkerClaim,
        stage: &str,
    ) -> Result<(), ImportServiceError> {
        let result = sqlx::query("UPDATE import_jobs SET stage = $4, lease_expires_at = now() + $5::interval, updated_at = now() WHERE job_id = $1 AND worker_claim_id = $2 AND worker_fence = $3 AND status = 'running' AND lease_expires_at > now()")
            .bind(claim.job_id)
            .bind(claim.claim_id)
            .bind(claim.fence)
            .bind(stage)
            .bind(WORKER_LEASE_SQL)
            .execute(&self.pool)
            .await
            .map_err(log_storage_error)?;
        (result.rows_affected() == 1)
            .then_some(())
            .ok_or(ImportServiceError::Conflict)
    }

    async fn persist_reserved_blob(&self, blob: &PendingBlob) -> Result<(), ImportServiceError> {
        self.persist_blob_parts(&blob.hash, blob.media_type, &blob.bytes)
            .await
    }

    async fn persist_blob_parts(
        &self,
        hash: &str,
        media_type: &str,
        bytes: &[u8],
    ) -> Result<(), ImportServiceError> {
        let stored = self.blobs.put(hash, bytes).await.map_err(map_blob_error)?;
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        insert_blob_record(&mut tx, hash, media_type, &stored).await?;
        tx.commit().await.map_err(log_storage_error)
    }

    async fn activate_reserved_source(
        &self,
        job_id: JobId,
        claim_id: Uuid,
    ) -> Result<(), ImportServiceError> {
        let changed = sqlx::query(
            "UPDATE import_jobs SET status = 'queued', worker_claim_id = NULL, lease_expires_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND worker_claim_id = $2 AND status = 'reserving_source' AND cancellation_requested = false",
        )
        .bind(job_id)
        .bind(claim_id)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
        if changed.rows_affected() == 1 {
            return Ok(());
        }
        self.reservation_terminal_or_activated(job_id).await
    }

    async fn reservation_terminal_or_activated(
        &self,
        job_id: JobId,
    ) -> Result<(), ImportServiceError> {
        let status: Option<String> =
            sqlx::query_scalar("SELECT status FROM import_jobs WHERE job_id = $1")
                .bind(job_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(log_storage_error)?;
        match status.as_deref() {
            Some("queued" | "running" | "succeeded" | "failed" | "cancelled") => Ok(()),
            Some("reserving_source") => Err(ImportServiceError::Conflict),
            _ => Err(ImportServiceError::NotFound),
        }
    }

    async fn fail_reserved_source(
        &self,
        job_id: JobId,
        claim_id: Uuid,
        material_id: MaterialId,
        diagnostic: ImportDiagnostic,
    ) -> Result<(), ImportServiceError> {
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let changed = sqlx::query(
            "UPDATE import_jobs SET status = 'failed', error_code = $3, worker_claim_id = NULL, lease_expires_at = NULL, finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND worker_claim_id = $2 AND status = 'reserving_source'",
        )
        .bind(job_id)
        .bind(claim_id)
        .bind(&diagnostic.code)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        if changed.rows_affected() == 1 {
            insert_diagnostic(&mut tx, job_id, 1, &diagnostic).await?;
            sqlx::query(
                "UPDATE materials SET import_status = 'failed', updated_at = now() WHERE material_id = $1",
            )
            .bind(material_id)
            .execute(&mut *tx)
            .await
            .map_err(log_storage_error)?;
        }
        tx.commit().await.map_err(log_storage_error)
    }

    async fn persist_success(
        &self,
        claim: ImportWorkerClaim,
        space_id: Uuid,
        source_ref: &SourceRef,
        attempt: i32,
        imported: ImportedPublication,
    ) -> Result<(), ImportServiceError> {
        let publication = PersistablePublication::from_reflowable(imported)?;
        self.persist_publication(claim, space_id, source_ref, attempt, publication)
            .await
    }

    async fn persist_pdf_success(
        &self,
        claim: ImportWorkerClaim,
        space_id: Uuid,
        source_ref: &SourceRef,
        attempt: i32,
        imported: ImportedPdf,
    ) -> Result<(), ImportServiceError> {
        let publication = PersistablePublication::from_pdf(imported)?;
        self.persist_publication(claim, space_id, source_ref, attempt, publication)
            .await
    }

    async fn persist_publication(
        &self,
        claim: ImportWorkerClaim,
        space_id: Uuid,
        source_ref: &SourceRef,
        attempt: i32,
        mut imported: PersistablePublication,
    ) -> Result<(), ImportServiceError> {
        self.set_stage(claim, "persisting").await?;
        let mut stored_resources = Vec::with_capacity(imported.resources.len());
        for resource in &imported.resources {
            let stored = self
                .blobs
                .put(&resource.content_hash, &resource.bytes)
                .await
                .map_err(map_blob_error)?;
            stored_resources.push((resource.clone(), stored));
        }
        imported.revision.created_at = timestamp_ms(OffsetDateTime::now_utc());
        let package_bytes = serde_json::to_vec(&imported.package_payload)
            .map_err(|_| ImportServiceError::Unavailable)?;
        let package_blob_hash = content_hash(&package_bytes);
        let stored_package = self
            .blobs
            .put(&package_blob_hash, &package_bytes)
            .await
            .map_err(map_blob_error)?;
        let source_map = imported.source_map;
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let may_publish: bool = sqlx::query_scalar(
            "SELECT status = 'running' AND cancellation_requested = false AND worker_claim_id = $2 AND worker_fence = $3 AND lease_expires_at > now() FROM import_jobs WHERE job_id = $1 FOR UPDATE",
        )
        .bind(claim.job_id)
        .bind(claim.claim_id)
        .bind(claim.fence)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .unwrap_or(false);
        if !may_publish {
            tx.rollback().await.map_err(log_storage_error)?;
            return self
                .fail(
                    claim,
                    imported.revision.material_id,
                    attempt,
                    cancelled_diagnostic(),
                    true,
                )
                .await;
        }
        for (resource, stored) in &stored_resources {
            insert_blob_record(
                &mut tx,
                &resource.content_hash,
                &resource.media_type,
                stored,
            )
            .await?;
        }
        insert_blob_record(
            &mut tx,
            &package_blob_hash,
            "application/vnd.lumi.normalized+json",
            &stored_package,
        )
        .await?;
        let source_blob_hash = source_ref
            .source_blob_hash()
            .ok_or(ImportServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO document_revisions (revision_id, material_id, space_id, source_format, source_hash, importer_id, importer_version, created_at, normalized_hash, package_format_version, source_blob_hash, supersedes_revision_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(imported.revision.id)
        .bind(imported.revision.material_id)
        .bind(space_id)
        .bind(source_ref.source_format())
        .bind(&imported.revision.source_hash)
        .bind(&imported.revision.importer_id)
        .bind(&imported.revision.importer_version)
        .bind(now)
        .bind(&imported.revision.normalized_hash)
        .bind(&imported.revision.package_format_version)
        .bind(source_blob_hash)
        .bind(imported.revision.supersedes_revision_id)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let manifest_id = imported.resources_manifest.id;
        sqlx::query(
            "INSERT INTO blob_manifests (manifest_id, space_id, schema_version, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(manifest_id)
        .bind(space_id)
        .bind(&imported.resources_manifest.schema_version)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        insert_manifest_entry(
            &mut tx,
            manifest_id,
            source_blob_hash,
            &format!("source/{}", source_ref.logical_source_name()),
            "source",
        )
        .await?;
        for (resource, _) in &stored_resources {
            insert_manifest_entry(
                &mut tx,
                manifest_id,
                &resource.content_hash,
                &resource.path,
                "resource",
            )
            .await?;
        }
        insert_manifest_entry(
            &mut tx,
            manifest_id,
            &package_blob_hash,
            "normalized/package.json",
            "normalized_package",
        )
        .await?;
        sqlx::query(
            "INSERT INTO normalized_packages (package_id, revision_id, schema_version, payload, source_map, manifest_id, package_blob_hash, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(imported.package_id)
        .bind(imported.revision.id)
        .bind(&imported.revision.package_format_version)
        .bind(imported.package_payload)
        .bind(source_map)
        .bind(manifest_id)
        .bind(&package_blob_hash)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "UPDATE materials SET canonical_title = $2, active_revision_id = $3, source_identity = $4, import_status = 'ready', object_revision = object_revision + 1, updated_at = $5 WHERE material_id = $1",
        )
        .bind(imported.revision.material_id)
        .bind(&imported.title)
        .bind(imported.revision.id)
        .bind(
            serde_json::to_value(&imported.source_identity)
                .map_err(|_| ImportServiceError::Unavailable)?,
        )
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        for diagnostic in &imported.revision.diagnostics {
            insert_diagnostic(&mut tx, claim.job_id, attempt, diagnostic).await?;
        }
        let completed = sqlx::query(
            "UPDATE import_jobs SET status = 'succeeded', stage = 'committed', revision_id = $4, error_code = NULL, worker_claim_id = NULL, lease_expires_at = NULL, finished_at = $5, updated_at = $5, object_revision = object_revision + 1 WHERE job_id = $1 AND worker_claim_id = $2 AND worker_fence = $3 AND status = 'running' AND cancellation_requested = false AND lease_expires_at > now()",
        )
        .bind(claim.job_id)
        .bind(claim.claim_id)
        .bind(claim.fence)
        .bind(imported.revision.id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        if completed.rows_affected() != 1 {
            tx.rollback().await.map_err(log_storage_error)?;
            return Err(ImportServiceError::Conflict);
        }
        append_import_change(
            &mut tx,
            ImportChange {
                space_id,
                object_id: imported.revision.material_id,
                device_id: source_ref.device_id(),
                idempotency_key: &format!("{}:complete", claim.job_id),
                change_kind: "blob_ref",
                payload: serde_json::json!({
                    "revision_id": imported.revision.id,
                    "manifest_id": manifest_id,
                    "import_status": "ready",
                }),
                now,
            },
        )
        .await?;
        tx.commit().await.map_err(log_storage_error)
    }

    async fn fail(
        &self,
        claim: ImportWorkerClaim,
        material_id: MaterialId,
        attempt: i32,
        diagnostic: ImportDiagnostic,
        cancelled: bool,
    ) -> Result<(), ImportServiceError> {
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        let cancellation_requested: bool = sqlx::query_scalar(
            "SELECT cancellation_requested FROM import_jobs WHERE job_id = $1 AND worker_claim_id = $2 AND worker_fence = $3 AND status = 'running' AND lease_expires_at > now() FOR UPDATE",
        )
        .bind(claim.job_id)
        .bind(claim.claim_id)
        .bind(claim.fence)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::Conflict)?;
        let cancelled = cancelled || cancellation_requested;
        let status = if cancelled { "cancelled" } else { "failed" };
        let result = sqlx::query(
            "UPDATE import_jobs SET status = $4, error_code = $5, worker_claim_id = NULL, lease_expires_at = NULL, finished_at = $6, updated_at = $6, object_revision = object_revision + 1 WHERE job_id = $1 AND worker_claim_id = $2 AND worker_fence = $3 AND status = 'running' AND lease_expires_at > now()",
        )
        .bind(claim.job_id)
        .bind(claim.claim_id)
        .bind(claim.fence)
        .bind(status)
        .bind(&diagnostic.code)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        if result.rows_affected() != 1 {
            tx.rollback().await.map_err(log_storage_error)?;
            return Err(ImportServiceError::Conflict);
        }
        insert_diagnostic(&mut tx, claim.job_id, attempt, &diagnostic).await?;
        sqlx::query(
            "UPDATE materials SET import_status = $2, updated_at = $3 WHERE material_id = $1",
        )
        .bind(material_id)
        .bind(status)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        tx.commit().await.map_err(log_storage_error)
    }

    fn remove_cancellation(&self, job_id: JobId) {
        if let Ok(mut flags) = self.cancellations.lock() {
            flags.remove(&job_id);
        }
    }
}

fn publication_from_epub(imported: ImportedEpub) -> ImportedPublication {
    ImportedPublication {
        title: imported.title,
        revision: imported.revision,
        package: imported.package,
        resources: imported
            .resources
            .into_iter()
            .map(|resource| ImportedPublicationResource {
                path: resource.path,
                media_type: resource.media_type,
                content_hash: resource.content_hash,
                bytes: resource.bytes,
            })
            .collect(),
    }
}

fn source_unavailable_diagnostic(kind: &str) -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Error,
        code: format!("{kind}_source_blob_unavailable"),
        message: "The accepted immutable source blob is unavailable.".to_owned(),
        source_path: None,
    }
}

fn source_import_diagnostic(kind: &str, message: &str) -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Error,
        code: format!("{kind}_normalization_failed"),
        message: message.to_owned(),
        source_path: None,
    }
}

fn telegram_part_diagnostic(code: &str, message: &str, source_path: String) -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Warning,
        code: code.to_owned(),
        message: message.to_owned(),
        source_path: Some(source_path),
    }
}

fn set_image_diagnostic(
    source_ref: &mut SourceRef,
    index: usize,
    diagnostic: ImportDiagnostic,
) -> Result<(), ImportServiceError> {
    let SourceRef::TelegramComposite { image_blobs, .. } = source_ref else {
        return Err(ImportServiceError::Unavailable);
    };
    let artifact = image_blobs
        .get_mut(index)
        .ok_or(ImportServiceError::Unavailable)?;
    artifact.blob_hash = None;
    artifact.media_type = None;
    artifact.diagnostic = Some(diagnostic);
    Ok(())
}

fn cancelled_diagnostic() -> ImportDiagnostic {
    ImportDiagnostic {
        severity: DiagnosticSeverity::Info,
        code: "import_cancelled".to_owned(),
        message: "Import was cancelled before publication.".to_owned(),
        source_path: None,
    }
}

struct PendingBlob {
    hash: String,
    media_type: &'static str,
    bytes: Vec<u8>,
}

struct PersistablePublication {
    title: String,
    revision: DocumentRevision,
    package_id: Uuid,
    package_payload: serde_json::Value,
    resources_manifest: BlobManifest,
    source_map: serde_json::Value,
    source_identity: SourceIdentity,
    resources: Vec<ImportedPublicationResource>,
}

impl PersistablePublication {
    fn from_reflowable(imported: ImportedPublication) -> Result<Self, ImportServiceError> {
        let source_map = source_map(&imported.package)?;
        let source_identity = imported.package.manifest.source.clone();
        let package_id = imported.package.id;
        let resources_manifest = imported.package.resources.clone();
        let package_payload =
            serde_json::to_value(imported.package).map_err(|_| ImportServiceError::Unavailable)?;
        Ok(Self {
            title: imported.title,
            revision: imported.revision,
            package_id,
            package_payload,
            resources_manifest,
            source_map,
            source_identity,
            resources: imported.resources,
        })
    }

    fn from_pdf(imported: ImportedPdf) -> Result<Self, ImportServiceError> {
        let source_map = pdf_source_map(&imported.package)?;
        let source_identity = imported.package.manifest.source.clone();
        let package_id = imported.package.id;
        let resources_manifest = imported.package.resources.clone();
        let package_payload =
            serde_json::to_value(imported.package).map_err(|_| ImportServiceError::Unavailable)?;
        Ok(Self {
            title: imported.title,
            revision: imported.revision,
            package_id,
            package_payload,
            resources_manifest,
            source_map,
            source_identity,
            resources: imported
                .resources
                .into_iter()
                .map(|resource| ImportedPublicationResource {
                    path: resource.path,
                    media_type: resource.media_type,
                    content_hash: resource.content_hash,
                    bytes: resource.bytes,
                })
                .collect(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SourceRef {
    Epub {
        blob_hash: String,
        file_name: String,
        media_type: String,
        device_id: Uuid,
    },
    Pdf {
        blob_hash: String,
        file_name: String,
        media_type: String,
        device_id: Uuid,
    },
    Markdown {
        blob_hash: String,
        file_name: String,
        media_type: String,
        device_id: Uuid,
    },
    Lum {
        blob_hash: String,
        file_name: String,
        media_type: String,
        device_id: Uuid,
    },
    WebPage {
        url: String,
        snapshot_blob_hash: Option<String>,
        device_id: Uuid,
    },
    TelegramText {
        snapshot_blob_hash: String,
        device_id: Uuid,
    },
    TelegramComposite {
        message_blob_hash: String,
        image_blobs: Vec<TelegramImageArtifact>,
        web_snapshots: Vec<TelegramWebArtifact>,
        bot_id: u64,
        device_id: Uuid,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TelegramImageArtifact {
    descriptor: TelegramPhotoDescriptor,
    blob_hash: Option<String>,
    media_type: Option<String>,
    diagnostic: Option<ImportDiagnostic>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TelegramWebArtifact {
    url: String,
    snapshot_blob_hash: Option<String>,
    diagnostic: Option<ImportDiagnostic>,
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyEpubSourceRef {
    blob_hash: String,
    file_name: String,
    media_type: String,
    device_id: Uuid,
}

impl SourceRef {
    fn device_id(&self) -> Uuid {
        match self {
            Self::Epub { device_id, .. }
            | Self::Pdf { device_id, .. }
            | Self::Markdown { device_id, .. }
            | Self::Lum { device_id, .. }
            | Self::WebPage { device_id, .. }
            | Self::TelegramText { device_id, .. }
            | Self::TelegramComposite { device_id, .. } => *device_id,
        }
    }

    fn source_blob_hash(&self) -> Option<&str> {
        match self {
            Self::Epub { blob_hash, .. }
            | Self::Pdf { blob_hash, .. }
            | Self::Markdown { blob_hash, .. }
            | Self::Lum { blob_hash, .. } => Some(blob_hash),
            Self::WebPage {
                snapshot_blob_hash, ..
            } => snapshot_blob_hash.as_deref(),
            Self::TelegramText {
                snapshot_blob_hash, ..
            } => Some(snapshot_blob_hash),
            Self::TelegramComposite {
                message_blob_hash, ..
            } => Some(message_blob_hash),
        }
    }

    fn source_media_type(&self) -> &'static str {
        match self {
            Self::Epub { .. } => EPUB_SOURCE_MEDIA_TYPE,
            Self::Pdf { .. } => PDF_SOURCE_MEDIA_TYPE,
            Self::Markdown { .. } => MARKDOWN_SOURCE_MEDIA_TYPE,
            Self::Lum { .. } => LUM_SOURCE_MEDIA_TYPE,
            Self::WebPage { .. } => "application/vnd.lumi.web-snapshot+json",
            Self::TelegramText { .. } => "application/vnd.lumi.telegram-message+json",
            Self::TelegramComposite { .. } => "application/vnd.lumi.telegram-envelope+json",
        }
    }

    fn source_format(&self) -> &'static str {
        match self {
            Self::Epub { .. } => "epub",
            Self::Pdf { .. } => "pdf",
            Self::Markdown { .. } => "markdown",
            Self::Lum { .. } => "lum",
            Self::WebPage { .. } => "web_page",
            Self::TelegramText { .. } | Self::TelegramComposite { .. } => "telegram",
        }
    }

    fn logical_source_name(&self) -> String {
        match self {
            Self::Epub { file_name, .. } => file_name.clone(),
            Self::Pdf { file_name, .. } => file_name.clone(),
            Self::Markdown { file_name, .. } => file_name.clone(),
            Self::Lum { file_name, .. } => file_name.clone(),
            Self::WebPage { .. } => "snapshot.json".to_owned(),
            Self::TelegramText { .. } => "message.json".to_owned(),
            Self::TelegramComposite { .. } => "envelope.json".to_owned(),
        }
    }
}

fn decode_source_ref(value: serde_json::Value) -> Result<SourceRef, ImportServiceError> {
    if let Ok(source_ref) = serde_json::from_value::<SourceRef>(value.clone()) {
        return Ok(source_ref);
    }
    serde_json::from_value::<LegacyEpubSourceRef>(value)
        .map(|legacy| SourceRef::Epub {
            blob_hash: legacy.blob_hash,
            file_name: legacy.file_name,
            media_type: legacy.media_type,
            device_id: legacy.device_id,
        })
        .map_err(|_| ImportServiceError::Unavailable)
}

async fn insert_blob_record(
    tx: &mut Transaction<'_, Postgres>,
    hash: &str,
    media_type: &str,
    stored: &StoredBlob,
) -> Result<(), ImportServiceError> {
    sqlx::query(
        "INSERT INTO blobs (content_hash, byte_length, media_type, storage_backend, storage_key) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (content_hash) DO NOTHING",
    )
    .bind(hash)
    .bind(i64::try_from(stored.byte_length).map_err(|_| ImportServiceError::Unavailable)?)
    .bind(media_type)
    .bind(stored.storage_backend)
    .bind(&stored.storage_key)
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

async fn derivation_by_artifact_in_transaction(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    artifact_id: Uuid,
) -> Result<Option<DerivedMaterialRelation>, ImportServiceError> {
    let row = sqlx::query(
        "SELECT derivation.kind, derivation.source_material_id, derivation.source_revision_id, derivation.task_id, derivation.artifact_id, derivation.source_changed OR source.active_revision_id IS DISTINCT FROM derivation.source_revision_id AS source_changed, derivation.source_refs FROM material_derivations derivation JOIN materials source ON source.material_id = derivation.source_material_id AND source.owner_user_id = derivation.user_id WHERE derivation.artifact_id = $1 AND derivation.user_id = $2",
    )
    .bind(artifact_id)
    .bind(owner_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    row.map(|row| {
        let source_refs: serde_json::Value =
            row.try_get("source_refs").map_err(log_storage_error)?;
        Ok(DerivedMaterialRelation {
            kind: row.try_get("kind").map_err(log_storage_error)?,
            source_material_id: row
                .try_get("source_material_id")
                .map_err(log_storage_error)?,
            source_revision_id: row
                .try_get("source_revision_id")
                .map_err(log_storage_error)?,
            task_id: row.try_get("task_id").map_err(log_storage_error)?,
            artifact_id: row.try_get("artifact_id").map_err(log_storage_error)?,
            source_changed: row.try_get("source_changed").map_err(log_storage_error)?,
            source_refs: serde_json::from_value(source_refs)
                .map_err(|_| ImportServiceError::Unavailable)?,
        })
    })
    .transpose()
}

async fn insert_manifest_entry(
    tx: &mut Transaction<'_, Postgres>,
    manifest_id: Uuid,
    hash: &str,
    logical_path: &str,
    role: &str,
) -> Result<(), ImportServiceError> {
    sqlx::query(
        "INSERT INTO blob_manifest_entries (manifest_id, content_hash, logical_path, role) VALUES ($1, $2, $3, $4)",
    )
    .bind(manifest_id)
    .bind(hash)
    .bind(logical_path)
    .bind(role)
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

async fn insert_diagnostic(
    tx: &mut Transaction<'_, Postgres>,
    job_id: JobId,
    attempt: i32,
    diagnostic: &ImportDiagnostic,
) -> Result<(), ImportServiceError> {
    sqlx::query(
        "INSERT INTO import_diagnostics (job_id, severity, code, message, source_path, attempt) VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(job_id)
    .bind(severity_name(diagnostic.severity))
    .bind(&diagnostic.code)
    .bind(&diagnostic.message)
    .bind(&diagnostic.source_path)
    .bind(attempt.max(1))
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

struct ImportChange<'a> {
    space_id: Uuid,
    object_id: Uuid,
    device_id: Uuid,
    idempotency_key: &'a str,
    change_kind: &'a str,
    payload: serde_json::Value,
    now: OffsetDateTime,
}

struct LibraryChange<'a> {
    space_id: Uuid,
    material_id: MaterialId,
    object_revision: i64,
    device_id: Uuid,
    idempotency_key: &'a str,
    change_kind: &'a str,
    payload: serde_json::Value,
    now: OffsetDateTime,
}

struct ReaderChange<'a> {
    space_id: Uuid,
    object_type: &'a str,
    object_id: Uuid,
    object_revision: i64,
    device_id: Uuid,
    idempotency_key: &'a str,
    payload: serde_json::Value,
    now: OffsetDateTime,
}

struct AnnotationChange<'a> {
    space_id: Uuid,
    annotation: &'a Annotation,
    base_revision: Option<u64>,
    device_id: Uuid,
    idempotency_key: &'a str,
    change_kind: &'a str,
    command: serde_json::Value,
    now: OffsetDateTime,
}

async fn append_import_change(
    tx: &mut Transaction<'_, Postgres>,
    change: ImportChange<'_>,
) -> Result<(), ImportServiceError> {
    sqlx::query(
        "INSERT INTO sync_changes (change_id, space_id, object_type, object_id, object_revision, change_kind, payload, device_id, hlc, schema_version, idempotency_key, created_at) VALUES ($1, $2, 'material', $3, 1, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(Uuid::now_v7())
    .bind(change.space_id)
    .bind(change.object_id)
    .bind(change.change_kind)
    .bind(change.payload)
    .bind(change.device_id)
    .bind(format!(
        "{}-0000-server",
        change.now.unix_timestamp_nanos()
    ))
    .bind(lumi_core::DOMAIN_SCHEMA_VERSION)
    .bind(change.idempotency_key)
    .bind(change.now)
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

async fn append_library_change(
    tx: &mut Transaction<'_, Postgres>,
    change: LibraryChange<'_>,
) -> Result<(), ImportServiceError> {
    sqlx::query(
        "INSERT INTO sync_changes (change_id, space_id, object_type, object_id, object_revision, change_kind, payload, device_id, hlc, schema_version, idempotency_key, created_at) VALUES ($1, $2, 'material', $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(Uuid::now_v7())
    .bind(change.space_id)
    .bind(change.material_id)
    .bind(change.object_revision)
    .bind(change.change_kind)
    .bind(change.payload)
    .bind(change.device_id)
    .bind(format!("{}-0000-server", change.now.unix_timestamp_nanos()))
    .bind(lumi_core::DOMAIN_SCHEMA_VERSION)
    .bind(change.idempotency_key)
    .bind(change.now)
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

async fn library_change_exists(
    tx: &mut Transaction<'_, Postgres>,
    space_id: Uuid,
    material_id: MaterialId,
    idempotency_key: &str,
    payload: &serde_json::Value,
) -> Result<bool, ImportServiceError> {
    let existing = sqlx::query(
        "SELECT object_id, payload FROM sync_changes WHERE space_id = $1 AND idempotency_key = $2",
    )
    .bind(space_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    let existing_id: Uuid = existing.try_get("object_id").map_err(log_storage_error)?;
    let existing_payload: serde_json::Value =
        existing.try_get("payload").map_err(log_storage_error)?;
    if existing_id == material_id && existing_payload == *payload {
        Ok(true)
    } else {
        Err(ImportServiceError::Conflict)
    }
}

async fn append_reader_change(
    tx: &mut Transaction<'_, Postgres>,
    change: ReaderChange<'_>,
) -> Result<(), ImportServiceError> {
    sqlx::query(
        "INSERT INTO sync_changes (change_id, space_id, object_type, object_id, object_revision, change_kind, payload, device_id, hlc, schema_version, idempotency_key, created_at) VALUES ($1, $2, $3, $4, $5, 'update', $6, $7, $8, $9, $10, $11)",
    )
    .bind(Uuid::now_v7())
    .bind(change.space_id)
    .bind(change.object_type)
    .bind(change.object_id)
    .bind(change.object_revision)
    .bind(change.payload)
    .bind(change.device_id)
    .bind(format!("{}-0000-server", change.now.unix_timestamp_nanos()))
    .bind(lumi_core::DOMAIN_SCHEMA_VERSION)
    .bind(change.idempotency_key)
    .bind(change.now)
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

async fn reader_change_exists(
    tx: &mut Transaction<'_, Postgres>,
    space_id: Uuid,
    object_id: Uuid,
    object_type: &str,
    idempotency_key: &str,
    payload: &serde_json::Value,
) -> Result<bool, ImportServiceError> {
    let existing = sqlx::query(
        "SELECT object_type, object_id, payload FROM sync_changes WHERE space_id = $1 AND idempotency_key = $2",
    )
    .bind(space_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    let Some(existing) = existing else {
        return Ok(false);
    };
    let existing_type: String = existing.try_get("object_type").map_err(log_storage_error)?;
    let existing_id: Uuid = existing.try_get("object_id").map_err(log_storage_error)?;
    let existing_payload: serde_json::Value =
        existing.try_get("payload").map_err(log_storage_error)?;
    if existing_type == object_type && existing_id == object_id && existing_payload == *payload {
        Ok(true)
    } else {
        Err(ImportServiceError::Conflict)
    }
}

async fn append_annotation_change(
    tx: &mut Transaction<'_, Postgres>,
    change: AnnotationChange<'_>,
) -> Result<(), ImportServiceError> {
    let payload = serde_json::json!({
        "command": change.command,
        "annotation": change.annotation,
    });
    sqlx::query(
        "INSERT INTO sync_changes (change_id, space_id, object_type, object_id, object_revision, base_revision, change_kind, payload, device_id, hlc, schema_version, idempotency_key, created_at) VALUES ($1, $2, 'annotation', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(Uuid::now_v7())
    .bind(change.space_id)
    .bind(change.annotation.id)
    .bind(i64::try_from(change.annotation.revision).map_err(|_| ImportServiceError::Unavailable)?)
    .bind(change.base_revision.and_then(|value| i64::try_from(value).ok()))
    .bind(change.change_kind)
    .bind(payload)
    .bind(change.device_id)
    .bind(format!("{}-0000-server", change.now.unix_timestamp_nanos()))
    .bind(lumi_core::DOMAIN_SCHEMA_VERSION)
    .bind(change.idempotency_key)
    .bind(change.now)
    .execute(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    Ok(())
}

async fn annotation_retry(
    tx: &mut Transaction<'_, Postgres>,
    space_id: Uuid,
    idempotency_key: &str,
    command: &serde_json::Value,
) -> Result<Option<Annotation>, ImportServiceError> {
    let existing = sqlx::query(
        "SELECT object_type, payload FROM sync_changes WHERE space_id = $1 AND idempotency_key = $2",
    )
    .bind(space_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    let Some(existing) = existing else {
        return Ok(None);
    };
    let object_type: String = existing.try_get("object_type").map_err(log_storage_error)?;
    let payload: serde_json::Value = existing.try_get("payload").map_err(log_storage_error)?;
    if object_type != "annotation" || payload.get("command") != Some(command) {
        return Err(ImportServiceError::Conflict);
    }
    payload
        .get("annotation")
        .cloned()
        .ok_or(ImportServiceError::Unavailable)
        .and_then(|value| {
            serde_json::from_value(value).map_err(|_| ImportServiceError::Unavailable)
        })
        .map(Some)
}

async fn lock_idempotency_key(
    tx: &mut Transaction<'_, Postgres>,
    space_id: Uuid,
    idempotency_key: &str,
) -> Result<(), ImportServiceError> {
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{space_id}:{idempotency_key}"))
            .fetch_one(&mut **tx)
            .await
            .map_err(log_storage_error)?;
    acquired
        .then_some(())
        .ok_or(ImportServiceError::RateLimited)
}

async fn enforce_account_backpressure(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
) -> Result<(), ImportServiceError> {
    // Serialize admission decisions per account so concurrent distinct keys cannot all
    // observe the same pending count and overrun the configured queue bound.
    let acquired: bool =
        sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 1))")
            .bind(user_id.to_string())
            .fetch_one(&mut **tx)
            .await
            .map_err(log_storage_error)?;
    if !acquired {
        return Err(ImportServiceError::RateLimited);
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM import_jobs WHERE user_id = $1 AND status IN ('reserving_source', 'queued', 'running')",
    )
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    if pending >= MAX_PENDING_IMPORTS_PER_ACCOUNT {
        return Err(ImportServiceError::RateLimited);
    }
    Ok(())
}

fn annotation_from_row(row: &PgRow) -> Result<Annotation, ImportServiceError> {
    let anchor: serde_json::Value = row.try_get("anchor").map_err(log_storage_error)?;
    let kind: serde_json::Value = row.try_get("kind").map_err(log_storage_error)?;
    let tags: serde_json::Value = row.try_get("tags").map_err(log_storage_error)?;
    let revision: i64 = row.try_get("object_revision").map_err(log_storage_error)?;
    let anchor: lumi_core::Anchor =
        serde_json::from_value(anchor).map_err(|_| ImportServiceError::Unavailable)?;
    let annotation_type_value: String =
        row.try_get("annotation_type").map_err(log_storage_error)?;
    let target_kind: String = row.try_get("target_kind").map_err(log_storage_error)?;
    let target = match target_kind.as_str() {
        "text_range" => AnnotationTarget::TextRange,
        "block" => AnnotationTarget::Block {
            path: anchor.node_path.clone(),
        },
        "section" => AnnotationTarget::Section {
            path: anchor.node_path.clone(),
        },
        "document" => AnnotationTarget::Document,
        "page_area" => AnnotationTarget::PageArea {
            page_index: anchor.page_rects.first().map_or(0, |rect| rect.page_index),
            exact: annotation_type_value != "margin_note",
        },
        _ => return Err(ImportServiceError::Unavailable),
    };
    let annotation_type = match annotation_type_value.as_str() {
        "highlight" => AnnotationType::Highlight,
        "note" => AnnotationType::Note,
        "margin_note" => AnnotationType::MarginNote,
        "voice_note" => AnnotationType::VoiceNote,
        _ => return Err(ImportServiceError::Unavailable),
    };
    let status: String = row.try_get("status").map_err(log_storage_error)?;
    let status = match status.as_str() {
        "active" => AnnotationStatus::Active,
        "archived" => AnnotationStatus::Archived,
        _ => return Err(ImportServiceError::Unavailable),
    };
    Ok(Annotation {
        id: row.try_get("annotation_id").map_err(log_storage_error)?,
        material_id: row.try_get("material_id").map_err(log_storage_error)?,
        revision_id: row.try_get("revision_id").map_err(log_storage_error)?,
        anchor,
        annotation_type,
        target,
        kind: serde_json::from_value(kind).map_err(|_| ImportServiceError::Unavailable)?,
        title: row.try_get("title").map_err(log_storage_error)?,
        tags: serde_json::from_value(tags).map_err(|_| ImportServiceError::Unavailable)?,
        status,
        related_annotation_id: row
            .try_get("related_annotation_id")
            .map_err(log_storage_error)?,
        revision: u64::try_from(revision).map_err(|_| ImportServiceError::Unavailable)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(log_storage_error)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(log_storage_error)?),
    })
}

async fn replace_annotation_tags(
    tx: &mut Transaction<'_, Postgres>,
    annotation_id: Uuid,
    tags: &[String],
    now: OffsetDateTime,
) -> Result<(), ImportServiceError> {
    sqlx::query("DELETE FROM annotation_tags WHERE annotation_id = $1")
        .bind(annotation_id)
        .execute(&mut **tx)
        .await
        .map_err(log_storage_error)?;
    for (ordinal, tag) in tags.iter().enumerate() {
        sqlx::query(
            "INSERT INTO annotation_tags
                (annotation_id, ordinal, tag, tag_key, created_at)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(annotation_id)
        .bind(i16::try_from(ordinal).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(tag)
        .bind(tag.to_lowercase())
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(log_storage_error)?;
    }
    Ok(())
}

async fn annotation_tags(
    tx: &mut Transaction<'_, Postgres>,
    annotation_id: Uuid,
) -> Result<Vec<String>, ImportServiceError> {
    sqlx::query_scalar(
        "SELECT tag
           FROM annotation_tags
          WHERE annotation_id = $1
          ORDER BY ordinal",
    )
    .bind(annotation_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(log_storage_error)
}

async fn validate_related_annotation(
    tx: &mut Transaction<'_, Postgres>,
    space_id: Uuid,
    material_id: MaterialId,
    related_annotation_id: Option<Uuid>,
) -> Result<(), ImportServiceError> {
    let Some(related_annotation_id) = related_annotation_id else {
        return Ok(());
    };
    let valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
              FROM annotations
             WHERE annotation_id = $1
               AND space_id = $2
               AND material_id = $3
               AND status = 'active'
               AND deleted_at IS NULL
        )",
    )
    .bind(related_annotation_id)
    .bind(space_id)
    .bind(material_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    if valid {
        Ok(())
    } else {
        Err(ImportServiceError::BadRequest(
            "related annotation must be active and belong to the same material",
        ))
    }
}

async fn validate_voice_attachment(
    tx: &mut Transaction<'_, Postgres>,
    user_id: Uuid,
    kind: &AnnotationKind,
) -> Result<(), ImportServiceError> {
    let AnnotationKind::VoiceNote {
        audio_attachment_id,
        transcript_artifact_id,
        ..
    } = kind
    else {
        return Ok(());
    };
    let attachment_valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
              FROM audio_attachments
             WHERE id = $1
               AND owner_id = $2
               AND audio_deleted_at IS NULL
        )",
    )
    .bind(audio_attachment_id)
    .bind(user_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    if !attachment_valid {
        return Err(ImportServiceError::BadRequest(
            "voice note requires an active owner-scoped audio attachment",
        ));
    }
    let Some(transcript_artifact_id) = transcript_artifact_id else {
        return Ok(());
    };
    let transcript_valid: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1
              FROM transcript_artifacts
             WHERE id = $1
               AND owner_id = $2
               AND attachment_id = $3
        )",
    )
    .bind(transcript_artifact_id)
    .bind(user_id)
    .bind(audio_attachment_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(log_storage_error)?;
    if transcript_valid {
        Ok(())
    } else {
        Err(ImportServiceError::BadRequest(
            "voice transcript must belong to the referenced attachment",
        ))
    }
}

fn validate_anchor_shape(
    revision_id: DocumentRevisionId,
    anchor: &lumi_core::Anchor,
) -> Result<(), ImportServiceError> {
    if matches!(
        anchor.source_locator,
        Some(lumi_core::SourceLocator::Pdf(_))
    ) {
        if anchor.revision_id != revision_id
            || anchor.quote.len() > 64 * 1024
            || anchor.prefix.len() > 512
            || anchor.suffix.len() > 512
            || anchor.content_hash.is_empty()
            || anchor.content_hash.len() > 128
            || anchor.page_rects.len() > 256
        {
            return Err(ImportServiceError::BadRequest(
                "PDF annotation anchor is incomplete or inconsistent",
            ));
        }
        return Ok(());
    }
    let range = anchor.text_range.ok_or(ImportServiceError::BadRequest(
        "annotation anchor needs a text range",
    ))?;
    if anchor.revision_id != revision_id
        || anchor.node_path.is_empty()
        || anchor.node_path.len() > 32
        || anchor.effective_end_node_path().is_empty()
        || anchor.effective_end_node_path().len() > 32
        || anchor.quote.trim().is_empty()
        || anchor.quote.len() > 64 * 1024
        || anchor.prefix.len() > 512
        || anchor.suffix.len() > 512
        || anchor.content_hash.is_empty()
        || anchor.content_hash.len() > 128
        || anchor.source_locator.is_none()
        || anchor.page_rects.len() > 256
        || anchor
            .node_path
            .iter()
            .chain(anchor.effective_end_node_path())
            .any(|component| component.is_empty() || component.len() > 512)
        || (anchor.node_path == anchor.effective_end_node_path() && range.start >= range.end)
    {
        return Err(ImportServiceError::BadRequest(
            "annotation anchor is incomplete or inconsistent",
        ));
    }
    Ok(())
}

fn validate_pdf_anchor(
    package: &FixedLayoutContentPackage,
    anchor: &lumi_core::Anchor,
    require_selection: bool,
) -> Result<(), ImportServiceError> {
    let Some(lumi_core::SourceLocator::Pdf(locator)) = anchor.source_locator.as_ref() else {
        return Err(ImportServiceError::BadRequest(
            "PDF anchor needs PDF source provenance",
        ));
    };
    let page = package
        .pages
        .get(locator.page_index as usize)
        .filter(|page| page.page_index == locator.page_index)
        .ok_or(ImportServiceError::BadRequest("PDF anchor page is unknown"))?;
    let has_geometry = !locator.page_rects.is_empty() || !locator.page_quads.is_empty();
    if anchor.revision_id != package.revision_id
        || locator.pdf_file_checksum != package.manifest.source.source_hash
        || locator.page_label != page.page_label
        || locator.page_revision_hash != page.page_hash
        || anchor.content_hash != page.page_hash
        || locator.page_rects.len() > 256
        || locator.page_quads.len() > 1_024
        || locator.normalized_rects.len() > 256
        || (require_selection && !has_geometry && anchor.quote.trim().is_empty())
        || locator
            .page_rects
            .iter()
            .chain(locator.normalized_rects.iter())
            .any(|rect| !pdf_rect_is_valid(*rect))
        || locator
            .page_quads
            .iter()
            .any(|quad| !pdf_quad_is_finite(*quad))
    {
        return Err(ImportServiceError::BadRequest(
            "PDF anchor does not match persisted page geometry",
        ));
    }
    Ok(())
}

fn pdf_rect_is_valid(rect: lumi_core::PdfRect) -> bool {
    rect.x.is_finite()
        && rect.y.is_finite()
        && rect.width.is_finite()
        && rect.height.is_finite()
        && rect.width >= 0.0
        && rect.height >= 0.0
}

fn pdf_quad_is_finite(quad: lumi_core::PdfQuad) -> bool {
    [
        quad.x1, quad.y1, quad.x2, quad.y2, quad.x3, quad.y3, quad.x4, quad.y4,
    ]
    .into_iter()
    .all(f32::is_finite)
}

fn validate_anchor_exact(
    plan: &RenderPlan,
    submitted: &lumi_core::Anchor,
) -> Result<(), ImportServiceError> {
    let range = submitted.text_range.ok_or(ImportServiceError::BadRequest(
        "annotation anchor needs a text range",
    ))?;
    let expected = plan
        .anchor_from_selection(
            &submitted.node_path,
            range.start,
            submitted.effective_end_node_path(),
            range.end,
        )
        .map_err(|_| ImportServiceError::BadRequest("annotation anchor does not match source"))?;
    if submitted.revision_id != expected.revision_id
        || submitted.node_path != expected.node_path
        || submitted.effective_end_node_path() != expected.effective_end_node_path()
        || submitted.text_range != expected.text_range
        || submitted.quote != expected.quote
        || submitted.prefix != expected.prefix
        || submitted.suffix != expected.suffix
        || submitted.content_hash != expected.content_hash
        || submitted.source_locator != expected.source_locator
        || submitted
            .end_source_locator
            .as_ref()
            .or(submitted.source_locator.as_ref())
            != expected.end_source_locator.as_ref()
        || !submitted.page_rects.is_empty()
    {
        return Err(ImportServiceError::BadRequest(
            "annotation anchor does not match persisted normalized content",
        ));
    }
    Ok(())
}

fn canonicalize_anchor(anchor: &mut lumi_core::Anchor) {
    if anchor.end_node_path.is_empty() {
        anchor.end_node_path = anchor.node_path.clone();
    }
    if anchor.end_source_locator.is_none() {
        anchor.end_source_locator = anchor.source_locator.clone();
    }
}

fn validate_progress_locator(
    plan: &RenderPlan,
    locator: &lumi_core::Anchor,
) -> Result<(), ImportServiceError> {
    let block = plan
        .block(&locator.node_path)
        .ok_or(ImportServiceError::BadRequest(
            "progress locator path is unknown",
        ))?;
    let range = locator.text_range.ok_or(ImportServiceError::BadRequest(
        "progress locator needs an offset",
    ))?;
    let text_len = block
        .text
        .as_deref()
        .map_or(1, |text| text.chars().count().max(1));
    if locator.revision_id != plan.revision_id
        || locator.effective_end_node_path() != locator.node_path
        || range.start != range.end
        || range.start > text_len
        || locator.content_hash != block.anchor.content_hash
        || locator.source_locator != block.anchor.source_locator
        || locator
            .end_source_locator
            .as_ref()
            .or(locator.source_locator.as_ref())
            != block.anchor.source_locator.as_ref()
    {
        return Err(ImportServiceError::BadRequest(
            "progress locator does not match persisted normalized content",
        ));
    }
    Ok(())
}

fn library_entry_from_row(
    row: &PgRow,
    latest_job: Job,
) -> Result<LibraryEntry, ImportServiceError> {
    let kind: String = row.try_get("kind").map_err(log_storage_error)?;
    let library_state: String = row.try_get("library_state").map_err(log_storage_error)?;
    let import_status: String = row.try_get("import_status").map_err(log_storage_error)?;
    let source_identity: serde_json::Value =
        row.try_get("source_identity").map_err(log_storage_error)?;
    let derivation = row
        .try_get::<Option<serde_json::Value>, _>("derivation")
        .map_err(log_storage_error)?
        .map(serde_json::from_value::<lumi_core::DerivedMaterialRelation>)
        .transpose()
        .map_err(|_| ImportServiceError::Unavailable)?;
    Ok(LibraryEntry {
        id: row.try_get("material_id").map_err(log_storage_error)?,
        owner_id: row.try_get("owner_user_id").map_err(log_storage_error)?,
        kind: match kind.as_str() {
            "epub" => MaterialKind::Epub,
            "pdf" => MaterialKind::Pdf,
            "web_page" => MaterialKind::WebPage,
            "telegram" => MaterialKind::Telegram,
            "markdown" => MaterialKind::Markdown,
            "lum" => MaterialKind::Lum,
            _ => return Err(ImportServiceError::Unavailable),
        },
        canonical_title: row.try_get("canonical_title").map_err(log_storage_error)?,
        title_override: row.try_get("title_override").map_err(log_storage_error)?,
        active_revision_id: row
            .try_get("active_revision_id")
            .map_err(log_storage_error)?,
        library_state: match library_state.as_str() {
            "active" => LibraryState::Active,
            "archived" => LibraryState::Archived,
            "deleted" => LibraryState::Deleted,
            _ => return Err(ImportServiceError::Unavailable),
        },
        source_identity: serde_json::from_value::<SourceIdentity>(source_identity)
            .map_err(|_| ImportServiceError::Unavailable)?,
        import_status: match import_status.as_str() {
            "queued" => MaterialImportStatus::Queued,
            "running" => MaterialImportStatus::Importing,
            "ready" => MaterialImportStatus::Ready,
            "failed" => MaterialImportStatus::Failed,
            "cancelled" => MaterialImportStatus::Cancelled,
            _ => return Err(ImportServiceError::Unavailable),
        },
        latest_job,
        derivation,
        created_at: timestamp_ms(row.try_get("created_at").map_err(log_storage_error)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(log_storage_error)?),
    })
}

fn library_state_name(state: LibraryState) -> &'static str {
    match state {
        LibraryState::Active => "active",
        LibraryState::Archived => "archived",
        LibraryState::Deleted => "deleted",
    }
}

fn validate_idempotency_key(idempotency_key: &str) -> Result<(), ImportServiceError> {
    if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
        Err(ImportServiceError::BadRequest(
            "Idempotency-Key must contain 1 to 200 characters",
        ))
    } else {
        Ok(())
    }
}

fn job_from_row(
    row: &PgRow,
    diagnostics: Vec<ImportDiagnostic>,
) -> Result<Job, ImportServiceError> {
    let status: String = row.try_get("status").map_err(log_storage_error)?;
    let stage: String = row.try_get("stage").map_err(log_storage_error)?;
    Ok(Job {
        id: row.try_get("job_id").map_err(log_storage_error)?,
        account_id: row.try_get("user_id").map_err(log_storage_error)?,
        kind: JobKind::Import,
        status: parse_status(&status)?,
        stage: parse_stage(&stage)?,
        material_id: row
            .try_get("result_material_id")
            .map_err(log_storage_error)?,
        revision_id: row.try_get("revision_id").map_err(log_storage_error)?,
        diagnostics,
        created_at: timestamp_ms(row.try_get("created_at").map_err(log_storage_error)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(log_storage_error)?),
    })
}

fn projected_job_from_row(
    row: &PgRow,
    diagnostics: Vec<ImportDiagnostic>,
) -> Result<Job, ImportServiceError> {
    let status: String = row.try_get("job_status").map_err(log_storage_error)?;
    let stage: String = row.try_get("job_stage").map_err(log_storage_error)?;
    Ok(Job {
        id: row.try_get("job_id").map_err(log_storage_error)?,
        account_id: row.try_get("job_user_id").map_err(log_storage_error)?,
        kind: JobKind::Import,
        status: parse_status(&status)?,
        stage: parse_stage(&stage)?,
        material_id: row
            .try_get("job_result_material_id")
            .map_err(log_storage_error)?,
        revision_id: row.try_get("job_revision_id").map_err(log_storage_error)?,
        diagnostics,
        created_at: timestamp_ms(row.try_get("job_created_at").map_err(log_storage_error)?),
        updated_at: timestamp_ms(row.try_get("job_updated_at").map_err(log_storage_error)?),
    })
}

fn diagnostic_from_row(row: PgRow) -> Result<ImportDiagnostic, ImportServiceError> {
    let severity: String = row.try_get("severity").map_err(log_storage_error)?;
    Ok(ImportDiagnostic {
        severity: match severity.as_str() {
            "info" => DiagnosticSeverity::Info,
            "warning" => DiagnosticSeverity::Warning,
            "error" => DiagnosticSeverity::Error,
            _ => return Err(ImportServiceError::Unavailable),
        },
        code: row.try_get("code").map_err(log_storage_error)?,
        message: row.try_get("message").map_err(log_storage_error)?,
        source_path: row.try_get("source_path").map_err(log_storage_error)?,
    })
}

fn parse_status(value: &str) -> Result<JobStatus, ImportServiceError> {
    match value {
        "reserving_source" | "queued" => Ok(JobStatus::Queued),
        "running" => Ok(JobStatus::Running),
        "succeeded" => Ok(JobStatus::Succeeded),
        "failed" => Ok(JobStatus::Failed),
        "cancelled" => Ok(JobStatus::Cancelled),
        _ => Err(ImportServiceError::Unavailable),
    }
}

fn parse_stage(value: &str) -> Result<JobStage, ImportServiceError> {
    match value {
        "source_accepted" => Ok(JobStage::SourceAccepted),
        "fetching_source" => Ok(JobStage::FetchingSource),
        "capturing_snapshot" => Ok(JobStage::CapturingSnapshot),
        "capturing_telegram_media" => Ok(JobStage::CapturingTelegramMedia),
        "fetching_linked_sources" => Ok(JobStage::FetchingLinkedSources),
        "extracting_content" => Ok(JobStage::ExtractingContent),
        "validating_container" => Ok(JobStage::ValidatingContainer),
        "inspecting_document" => Ok(JobStage::InspectingDocument),
        "normalizing" => Ok(JobStage::Normalizing),
        "persisting" => Ok(JobStage::Persisting),
        "reader_document_built" => Ok(JobStage::ReaderDocumentBuilt),
        "committed" => Ok(JobStage::Committed),
        _ => Err(ImportServiceError::Unavailable),
    }
}

fn severity_name(value: DiagnosticSeverity) -> &'static str {
    match value {
        DiagnosticSeverity::Info => "info",
        DiagnosticSeverity::Warning => "warning",
        DiagnosticSeverity::Error => "error",
    }
}

fn source_map(package: &NormalizedContentPackage) -> Result<serde_json::Value, ImportServiceError> {
    let entries = package
        .blocks
        .iter()
        .map(|block| {
            serde_json::json!({
                "block_id": block.id,
                "node_path": block.node_path,
                "content_hash": block.content_hash,
                "source_locator": block.source_locator,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_value(entries).map_err(|_| ImportServiceError::Unavailable)
}

fn pdf_source_map(
    package: &FixedLayoutContentPackage,
) -> Result<serde_json::Value, ImportServiceError> {
    let entries = package
        .pages
        .iter()
        .map(|page| {
            serde_json::json!({
                "page_index": page.page_index,
                "page_label": page.page_label,
                "page_hash": page.page_hash,
                "crop_box": page.crop_box,
                "text_layer_state": page.text_layer_state,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_value(entries).map_err(|_| ImportServiceError::Unavailable)
}

fn reading_document_from_package(
    package: &NormalizedContentPackage,
    material_id: MaterialId,
) -> ReadingDocument {
    let blocks = package
        .blocks
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect::<HashMap<_, _>>();
    let nodes = package
        .units
        .iter()
        .enumerate()
        .map(|(index, unit)| ReadingNode {
            id: unit.id.clone(),
            path: vec![format!("unit-{index}")],
            kind: ReadingNodeKind::Section,
            text: Some(unit.title.clone()),
            resource_hash: None,
            content_hash: content_hash(unit.title.as_bytes()),
            source_locator: unit.source_locator.clone(),
            links: Vec::new(),
            children: unit
                .block_ids
                .iter()
                .filter_map(|id| blocks.get(id.as_str()))
                .map(|block| ReadingNode {
                    id: block.id.clone(),
                    path: block.node_path.clone(),
                    kind: block.kind.clone(),
                    text: block.text.clone(),
                    resource_hash: block.resource_hash.clone(),
                    content_hash: block.content_hash.clone(),
                    source_locator: block.source_locator.clone(),
                    links: block.links.clone(),
                    children: Vec::new(),
                })
                .collect(),
        })
        .collect();
    ReadingDocument {
        material_id,
        revision_id: package.revision_id,
        title: package.manifest.title.clone(),
        creators: package.manifest.creators.clone(),
        nodes,
        navigation: package.navigation.clone(),
    }
}

fn safe_file_name(value: &str) -> Result<String, ImportServiceError> {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim()
        .chars()
        .filter(|character| !character.is_control() && !matches!(character, '"' | '\\'))
        .take(240)
        .collect::<String>();
    let lowercase = name.to_ascii_lowercase();
    if name.is_empty()
        || !(lowercase.ends_with(".epub")
            || lowercase.ends_with(".pdf")
            || lowercase.ends_with(".md")
            || lowercase.ends_with(".markdown")
            || lowercase.ends_with(".lum"))
    {
        Err(ImportServiceError::BadRequest(
            "upload must have a non-empty .epub, .pdf, .md, .markdown or .lum file name",
        ))
    } else {
        Ok(name)
    }
}

fn upload_title(file_name: &str) -> String {
    file_name
        .strip_suffix(".epub")
        .or_else(|| file_name.strip_suffix(".EPUB"))
        .or_else(|| file_name.strip_suffix(".pdf"))
        .or_else(|| file_name.strip_suffix(".PDF"))
        .or_else(|| file_name.strip_suffix(".markdown"))
        .or_else(|| file_name.strip_suffix(".MARKDOWN"))
        .or_else(|| file_name.strip_suffix(".md"))
        .or_else(|| file_name.strip_suffix(".MD"))
        .or_else(|| file_name.strip_suffix(".lum"))
        .or_else(|| file_name.strip_suffix(".LUM"))
        .unwrap_or(file_name)
        .to_owned()
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

fn map_blob_error(error: BlobStoreError) -> ImportServiceError {
    tracing::error!(%error, "blob backend operation failed");
    ImportServiceError::Unavailable
}

fn log_storage_error(error: impl std::fmt::Display) -> ImportServiceError {
    tracing::error!(%error, "import repository operation failed");
    ImportServiceError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumi_core::{
        import_epub_fixture, rich_epub_fixture, sample_fixture_highlight, AnnotationKind,
        HighlightStyle,
    };

    #[test]
    fn safe_file_name_should_strip_client_path() {
        let name = safe_file_name("C:\\fakepath\\book.epub");

        assert!(matches!(name.as_deref(), Ok("book.epub")));
    }

    #[test]
    fn upload_kind_should_detect_markdown_extensions() -> Result<(), Box<dyn std::error::Error>> {
        let source = include_bytes!("../../../tests/fixtures/markdown/supported.md");

        assert_eq!(
            FileUploadKind::detect("guide.markdown", source)?,
            FileUploadKind::Markdown
        );
        Ok(())
    }

    #[test]
    fn upload_kind_should_detect_lum_extension() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            FileUploadKind::detect("guide.lum", b"PK\x03\x04")?,
            FileUploadKind::Lum
        );
        assert_eq!(safe_file_name("C:\\fakepath\\guide.lum")?, "guide.lum");
        Ok(())
    }

    #[test]
    fn safe_file_name_should_reject_unsupported_extension() -> Result<(), Box<dyn std::error::Error>>
    {
        let Err(error) = safe_file_name("book.html") else {
            return Err(std::io::Error::other("unsupported extension was accepted").into());
        };

        assert!(matches!(error, ImportServiceError::BadRequest(_)));
        Ok(())
    }

    #[tokio::test]
    async fn postgres_pdf_import_publishes_fixed_layout_reader_and_anchors(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let source = include_bytes!("../../../tests/fixtures/pdf/text-layer.pdf");
        match PopplerPdfEngine::from_environment().inspect(source) {
            Ok(_) => {}
            Err(lumi_core::PdfImportError::EngineUnavailable(error)) => {
                eprintln!("PDF import integration test skipped: {error}");
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }

        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(6)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'PDF test', 'web')")
            .bind(device_id)
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let blob_root = std::env::temp_dir().join(format!("lumi-pdf-test-{}", Uuid::now_v7()));
        let service = Arc::new(ImportService::local(pool, blob_root.clone()));
        let session = AuthenticatedSession {
            user_id,
            session_id: Uuid::now_v7(),
            device_id,
            csrf_hash: [0; 32],
            instance_role: lumi_core::InstanceRole::User,
        };
        let accepted = service
            .accept(
                &session,
                "text-layer.pdf",
                "pdf-import-integration",
                source.to_vec(),
            )
            .await?;
        let job = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let job = service.job(user_id, accepted.job.id).await?;
                if matches!(
                    job.status,
                    JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
                ) {
                    return Ok::<Job, ImportServiceError>(job);
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await??;
        assert_eq!(job.status, JobStatus::Succeeded, "{:?}", job.diagnostics);
        let revision_id = job
            .revision_id
            .ok_or_else(|| std::io::Error::other("PDF revision is missing"))?;
        let entry = service.material(user_id, accepted.material_id).await?;
        assert_eq!(entry.kind, MaterialKind::Pdf);
        assert_eq!(entry.import_status, MaterialImportStatus::Ready);
        let document = service.page_fidelity_document(user_id, revision_id).await?;
        assert_eq!(document.pages.len(), 2);
        assert_eq!(
            document.text_layer_state,
            lumi_core::PdfTextLayerState::Native
        );
        assert!(document.pages[1].width_points > document.pages[1].height_points);
        let page = &document.pages[0];
        let rect = lumi_core::PdfRect {
            x: 62.0,
            y: 120.0,
            width: 180.0,
            height: 18.0,
        };
        let source_locator = lumi_core::SourceLocator::Pdf(lumi_core::PdfSourceLocator {
            pdf_file_checksum: entry.source_identity.source_hash.clone(),
            page_index: page.page_index,
            page_label: page.page_label.clone(),
            page_revision_hash: page.page_hash.clone(),
            page_rects: vec![rect],
            page_quads: Vec::new(),
            text_layer_revision: Some("integration-test.v1".to_owned()),
            text_block_start: None,
            text_block_end: None,
            text_char_start: None,
            text_char_end: None,
            normalized_rects: vec![lumi_core::PdfRect {
                x: rect.x / page.width_points,
                y: rect.y / page.height_points,
                width: rect.width / page.width_points,
                height: rect.height / page.height_points,
            }],
        });
        let anchor = lumi_core::Anchor {
            revision_id,
            node_path: vec!["page-0".to_owned()],
            end_node_path: vec!["page-0".to_owned()],
            text_range: Some(lumi_core::TextRange { start: 0, end: 15 }),
            quote: "searchable text".to_owned(),
            prefix: String::new(),
            suffix: String::new(),
            content_hash: page.page_hash.clone(),
            source_locator: Some(source_locator.clone()),
            end_source_locator: Some(source_locator),
            page_rects: vec![lumi_core::PageRect {
                page_index: 0,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            }],
        };
        let annotation = service
            .create_annotation(
                &session,
                CreateAnnotationCommand {
                    material_id: entry.id,
                    revision_id,
                    anchor: anchor.clone(),
                    target: AnnotationTarget::PageArea {
                        page_index: 0,
                        exact: true,
                    },
                    kind: AnnotationKind::Highlight {
                        style: HighlightStyle::Yellow,
                    },
                    title: None,
                    tags: Vec::new(),
                    status: AnnotationStatus::Active,
                    related_annotation_id: None,
                },
                "pdf-annotation-integration",
            )
            .await?;
        assert_eq!(annotation.anchor, anchor);
        let progress = service
            .move_reading_position(
                &session,
                MoveReadingPositionCommand {
                    material_id: entry.id,
                    revision_id,
                    locator: anchor,
                    progress_fraction: 0.5,
                },
                "pdf-progress-integration",
            )
            .await?;
        assert_eq!(progress.progress_fraction, 0.5);
        let (name, media_type, downloaded) = service.source(user_id, entry.id).await?;
        assert_eq!(name, "text-layer.pdf");
        assert_eq!(media_type, PDF_SOURCE_MEDIA_TYPE);
        assert_eq!(downloaded, source);
        let _ = tokio::fs::remove_dir_all(blob_root).await;
        Ok(())
    }

    #[tokio::test]
    async fn postgres_markdown_import_publishes_reflowable_reader_and_source(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let source = include_bytes!("../../../tests/fixtures/markdown/supported.md");
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(6)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'Markdown test', 'web')")
            .bind(device_id)
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let blob_root = std::env::temp_dir().join(format!("lumi-markdown-test-{}", Uuid::now_v7()));
        let service = Arc::new(ImportService::local(pool, blob_root.clone()));
        let session = AuthenticatedSession {
            user_id,
            session_id: Uuid::now_v7(),
            device_id,
            csrf_hash: [0; 32],
            instance_role: lumi_core::InstanceRole::User,
        };
        let accepted = service
            .accept(
                &session,
                "supported.md",
                "markdown-import-integration",
                source.to_vec(),
            )
            .await?;
        let job = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let job = service.job(user_id, accepted.job.id).await?;
                if matches!(
                    job.status,
                    JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
                ) {
                    return Ok::<Job, ImportServiceError>(job);
                }
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
        })
        .await??;
        assert_eq!(job.status, JobStatus::Succeeded, "{:?}", job.diagnostics);
        let revision_id = job
            .revision_id
            .ok_or_else(|| std::io::Error::other("Markdown revision is missing"))?;
        let entry = service.material(user_id, accepted.material_id).await?;
        assert_eq!(entry.kind, MaterialKind::Markdown);
        assert_eq!(entry.canonical_title, "Руководство Lumi");
        let document = service.reading_document(user_id, revision_id).await?;
        assert!(document
            .navigation
            .iter()
            .any(|item| item.label == "Детали"));
        assert!(document.nodes[0]
            .children
            .iter()
            .any(|node| matches!(node.kind, ReadingNodeKind::Table)));
        let (name, media_type, downloaded) = service.source(user_id, entry.id).await?;
        assert_eq!(name, "supported.md");
        assert_eq!(media_type, MARKDOWN_SOURCE_MEDIA_TYPE);
        assert_eq!(downloaded, source);
        let _ = tokio::fs::remove_dir_all(blob_root).await;
        Ok(())
    }

    #[tokio::test]
    async fn performance_large_library_continuation_is_one_bounded_projection(
    ) -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var("LUMI_PERFORMANCE").as_deref() != Ok("1") {
            return Ok(());
        }
        let database_url = std::env::var("LUMI_TEST_DATABASE_URL")?;
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let imported = import_epub_fixture(user_id, &rich_epub_fixture())?;
        let anchor_template = sample_fixture_highlight(&imported)
            .ok_or_else(|| std::io::Error::other("fixture anchor missing"))?
            .anchor;
        let mut expected_material_id = Uuid::nil();
        let base_time = OffsetDateTime::now_utc();
        let mut tx = pool.begin().await?;
        for index in 0..1_000_i64 {
            let material_id = Uuid::now_v7();
            let revision_id = Uuid::now_v7();
            let job_id = Uuid::now_v7();
            let updated_at = base_time + time::Duration::milliseconds(index);
            let source_identity = serde_json::json!({
                "format": "epub",
                "source_name": format!("performance-{index}.epub"),
                "source_hash": "a".repeat(64),
            });
            sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status, created_at, updated_at) VALUES ($1, $2, $3, 'epub', $4, 'active', $5, 'ready', $6, $6)")
                .bind(material_id)
                .bind(space_id)
                .bind(user_id)
                .bind(format!("Performance material {index}"))
                .bind(source_identity)
                .bind(updated_at)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO document_revisions (revision_id, material_id, space_id, source_format, source_hash, importer_id, importer_version, created_at) VALUES ($1, $2, $3, 'epub', $4, 'lumi.epub', 's1.2', $5)")
                .bind(revision_id)
                .bind(material_id)
                .bind(space_id)
                .bind("a".repeat(64))
                .bind(updated_at)
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, result_material_id, revision_id, created_at, updated_at) VALUES ($1, $2, $3, 'succeeded', 'committed', $4, $5, $6, $6)")
                .bind(job_id)
                .bind(user_id)
                .bind(space_id)
                .bind(material_id)
                .bind(revision_id)
                .bind(updated_at)
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE materials SET active_revision_id = $2, latest_import_job_id = $3 WHERE material_id = $1")
                .bind(material_id)
                .bind(revision_id)
                .bind(job_id)
                .execute(&mut *tx)
                .await?;
            let mut locator = anchor_template.clone();
            locator.revision_id = revision_id;
            sqlx::query("INSERT INTO reading_progress (space_id, material_id, revision_id, locator, progress_fraction, updated_at) VALUES ($1, $2, $3, $4, 0.5, $5)")
                .bind(space_id)
                .bind(material_id)
                .bind(revision_id)
                .bind(serde_json::to_value(locator)?)
                .bind(updated_at)
                .execute(&mut *tx)
                .await?;
            expected_material_id = material_id;
        }
        tx.commit().await?;
        let root = std::env::temp_dir().join(format!("lumi-performance-{}", Uuid::now_v7()));
        tokio::fs::create_dir_all(&root).await?;
        let service = ImportService::local(pool, root.clone());
        let started = std::time::Instant::now();
        let projection = service
            .continue_reading(user_id)
            .await?
            .ok_or_else(|| std::io::Error::other("continuation projection missing"))?;
        let elapsed = started.elapsed();
        let _ = tokio::fs::remove_dir_all(root).await;

        assert_eq!(projection.entry.id, expected_material_id);
        assert!(elapsed < std::time::Duration::from_millis(300));
        Ok(())
    }

    #[tokio::test]
    async fn postgres_annotations_are_idempotent_conflict_safe_and_tombstoned(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(6)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let foreign_user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        let imported = import_epub_fixture(user_id, &rich_epub_fixture())?;
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active'), ($2, 'active')")
            .bind(user_id)
            .bind(foreign_user_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'Stage 5 test', 'web')")
            .bind(device_id)
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let mut tx = pool.begin().await?;
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, active_revision_id, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'epub', $4, NULL, 'active', $5, 'ready')")
            .bind(imported.material.id)
            .bind(space_id)
            .bind(user_id)
            .bind(&imported.material.canonical_title)
            .bind(serde_json::to_value(&imported.material.source_identity)?)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO document_revisions (revision_id, material_id, space_id, source_format, source_hash, importer_id, importer_version, normalized_hash, package_format_version) VALUES ($1, $2, $3, 'epub', $4, $5, $6, $7, $8)")
            .bind(imported.revision.id)
            .bind(imported.material.id)
            .bind(space_id)
            .bind(&imported.revision.source_hash)
            .bind(&imported.revision.importer_id)
            .bind(&imported.revision.importer_version)
            .bind(&imported.revision.normalized_hash)
            .bind(&imported.revision.package_format_version)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE materials SET active_revision_id = $1 WHERE material_id = $2")
            .bind(imported.revision.id)
            .bind(imported.material.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO normalized_packages (package_id, revision_id, schema_version, payload, source_map) VALUES ($1, $2, $3, $4, '[]'::jsonb)")
            .bind(imported.package.id)
            .bind(imported.revision.id)
            .bind(&imported.package.manifest.package_format_version)
            .bind(serde_json::to_value(&imported.package)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;

        let service = ImportService::local(pool.clone(), std::env::temp_dir());
        let session = AuthenticatedSession {
            user_id,
            session_id: Uuid::now_v7(),
            device_id,
            csrf_hash: [0; 32],
            instance_role: lumi_core::InstanceRole::User,
        };
        let plan = RenderPlan::from_document(&imported.reading_document);
        let block = plan
            .blocks
            .iter()
            .find(|block| {
                block
                    .text
                    .as_deref()
                    .is_some_and(|text| text.chars().count() > 12)
            })
            .ok_or_else(|| std::io::Error::other("fixture block missing"))?;
        let anchor = plan.anchor_from_selection(&block.node_path, 1, &block.node_path, 9)?;
        let command = CreateAnnotationCommand {
            material_id: imported.material.id,
            revision_id: imported.revision.id,
            anchor: anchor.clone(),
            target: AnnotationTarget::TextRange,
            kind: AnnotationKind::Note {
                body: "original".to_owned(),
            },
            title: None,
            tags: vec!["integration".to_owned()],
            status: AnnotationStatus::Active,
            related_annotation_id: None,
        };
        let created = service
            .create_annotation(&session, command.clone(), "stage5-create")
            .await?;
        let edited = service
            .update_annotation(
                &session,
                UpdateAnnotationCommand {
                    material_id: imported.material.id,
                    annotation_id: created.id,
                    expected_revision: created.revision,
                    target: created.target.clone(),
                    kind: AnnotationKind::Note {
                        body: "edited".to_owned(),
                    },
                    title: Some("Edited".to_owned()),
                    tags: created.tags.clone(),
                    status: created.status,
                    related_annotation_id: None,
                },
                "stage5-update",
            )
            .await?;
        let replay = service
            .create_annotation(&session, command.clone(), "stage5-create")
            .await?;
        assert_eq!(replay, created);
        let mut different = command.clone();
        different.kind = AnnotationKind::Highlight {
            style: HighlightStyle::Blue,
        };
        assert!(matches!(
            service
                .create_annotation(&session, different, "stage5-create")
                .await,
            Err(ImportServiceError::Conflict)
        ));
        assert!(matches!(
            service
                .update_annotation(
                    &session,
                    UpdateAnnotationCommand {
                        material_id: imported.material.id,
                        annotation_id: created.id,
                        expected_revision: 1,
                        target: created.target.clone(),
                        kind: AnnotationKind::Note {
                            body: "stale".to_owned(),
                        },
                        title: None,
                        tags: Vec::new(),
                        status: AnnotationStatus::Active,
                        related_annotation_id: None,
                    },
                    "stage5-stale",
                )
                .await,
            Err(ImportServiceError::Conflict)
        ));

        let concurrent_command = CreateAnnotationCommand {
            kind: AnnotationKind::Highlight {
                style: HighlightStyle::Green,
            },
            ..command
        };
        let left_service = service.clone();
        let left_session = session.clone();
        let left_command = concurrent_command.clone();
        let retry_command = concurrent_command.clone();
        let right_service = service.clone();
        let right_session = session.clone();
        let (left, right) = tokio::join!(
            left_service.create_annotation(&left_session, left_command, "stage5-concurrent"),
            right_service.create_annotation(
                &right_session,
                concurrent_command,
                "stage5-concurrent"
            )
        );
        let concurrent_id = match (left, right) {
            (Ok(left), Ok(right)) => {
                assert_eq!(left.id, right.id);
                left.id
            }
            (Ok(created), Err(ImportServiceError::RateLimited))
            | (Err(ImportServiceError::RateLimited), Ok(created)) => created.id,
            (left, right) => {
                return Err(std::io::Error::other(format!(
                    "unexpected concurrent idempotency results: {left:?}, {right:?}"
                ))
                .into());
            }
        };
        let replayed = service
            .create_annotation(&session, retry_command, "stage5-concurrent")
            .await?;
        assert_eq!(replayed.id, concurrent_id);

        let deleted = service
            .delete_annotation(
                &session,
                DeleteAnnotationCommand {
                    material_id: imported.material.id,
                    annotation_id: created.id,
                    expected_revision: edited.revision,
                },
                "stage5-delete",
            )
            .await?;
        assert_eq!(deleted.revision, 3);
        assert!(!service
            .annotations(user_id, imported.material.id)
            .await?
            .iter()
            .any(|annotation| annotation.id == created.id));
        assert!(!service
            .export_annotations(user_id, imported.material.id)
            .await?
            .entries
            .iter()
            .any(|entry| entry.annotation.id == created.id));
        assert!(matches!(
            service
                .annotations(foreign_user_id, imported.material.id)
                .await,
            Err(ImportServiceError::NotFound)
        ));
        let tombstoned: bool = sqlx::query_scalar(
            "SELECT deleted_at IS NOT NULL FROM annotations WHERE annotation_id = $1",
        )
        .bind(created.id)
        .fetch_one(&pool)
        .await?;
        assert!(tombstoned);
        let changes: Vec<(String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT change_kind, base_revision, object_revision FROM sync_changes WHERE object_id = $1 ORDER BY change_seq",
        )
        .bind(created.id)
        .fetch_all(&pool)
        .await?;
        assert_eq!(
            changes,
            vec![
                ("create".to_owned(), None, 1),
                ("update".to_owned(), Some(1), 2),
                ("delete".to_owned(), Some(2), 3),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn postgres_worker_lease_cancel_and_admission_are_fenced(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _recovery_guard = POSTGRES_RECOVERY_TEST_LOCK.lock().await;
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'Lease test', 'web')")
            .bind(device_id)
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let service = Arc::new(ImportService::local(
            pool.clone(),
            std::env::temp_dir().join(format!("lumi-lease-{}", Uuid::now_v7())),
        ));

        let active_material = Uuid::now_v7();
        let active_job = Uuid::now_v7();
        let active_claim = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'epub', 'active lease', 'active', $4, 'running')")
            .bind(active_material)
            .bind(space_id)
            .bind(user_id)
            .bind(serde_json::json!({"format":"epub","source_name":"lease.epub","source_hash":"lease"}))
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, attempt, max_attempts, worker_claim_id, lease_expires_at) VALUES ($1, $2, $3, 'running', 'normalizing', $4, $5, 'lease-active', 'epub', 3, 3, $6, now() + interval '20 minutes')")
            .bind(active_job)
            .bind(user_id)
            .bind(space_id)
            .bind(serde_json::json!({"kind":"epub","blob_hash":"missing","file_name":"lease.epub","media_type":"application/epub+zip","device_id":device_id}))
            .bind(active_material)
            .bind(active_claim)
            .execute(&pool)
            .await?;
        service.recover().await?;
        let active_status: String =
            sqlx::query_scalar("SELECT status FROM import_jobs WHERE job_id = $1")
                .bind(active_job)
                .fetch_one(&pool)
                .await?;
        assert_eq!(active_status, "running");

        let cancel_material = Uuid::now_v7();
        let cancel_job = Uuid::now_v7();
        let cancel_claim = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'epub', 'cancel race', 'active', $4, 'running')")
            .bind(cancel_material).bind(space_id).bind(user_id)
            .bind(serde_json::json!({"format":"epub","source_name":"cancel.epub","source_hash":"cancel"}))
            .execute(&pool).await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, worker_claim_id, worker_fence, lease_expires_at, cancellation_requested) VALUES ($1, $2, $3, 'running', 'normalizing', $4, $5, 'lease-cancel', 'epub', $6, 1, now() + interval '20 minutes', true)")
            .bind(cancel_job).bind(user_id).bind(space_id)
            .bind(serde_json::json!({"kind":"epub","blob_hash":"missing","file_name":"cancel.epub","media_type":"application/epub+zip","device_id":device_id}))
            .bind(cancel_material).bind(cancel_claim).execute(&pool).await?;
        service
            .fail(
                ImportWorkerClaim {
                    job_id: cancel_job,
                    claim_id: cancel_claim,
                    fence: 1,
                },
                cancel_material,
                1,
                source_unavailable_diagnostic("epub"),
                false,
            )
            .await?;
        let cancelled: (String, String) = sqlx::query_as(
            "SELECT j.status, m.import_status FROM import_jobs j JOIN materials m ON m.material_id = j.result_material_id WHERE j.job_id = $1",
        )
        .bind(cancel_job)
        .fetch_one(&pool)
        .await?;
        assert_eq!(cancelled, ("cancelled".to_owned(), "cancelled".to_owned()));

        for index in 0..MAX_PENDING_IMPORTS_PER_ACCOUNT {
            let material_id = Uuid::now_v7();
            let job_id = Uuid::now_v7();
            sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'web_page', $4, 'active', $5, 'queued')")
                .bind(material_id).bind(space_id).bind(user_id).bind(format!("pending {index}"))
                .bind(serde_json::json!({"format":"web_page","source_name":"https://example.com","source_hash":format!("pending-{index}")}))
                .execute(&pool).await?;
            sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind) VALUES ($1, $2, $3, 'queued', 'source_accepted', $4, $5, $6, 'web_page')")
                .bind(job_id).bind(user_id).bind(space_id)
                .bind(serde_json::json!({"kind":"web_page","url":"https://example.com","snapshot_blob_hash":null,"device_id":device_id}))
                .bind(material_id).bind(format!("pending-{index}")).execute(&pool).await?;
        }
        let session = AuthenticatedSession {
            user_id,
            session_id: Uuid::now_v7(),
            device_id,
            csrf_hash: [0; 32],
            instance_role: lumi_core::InstanceRole::User,
        };
        assert!(matches!(
            service
                .accept_web(&session, "https://example.com/new", "over-limit")
                .await,
            Err(ImportServiceError::RateLimited)
        ));
        assert!(service.try_begin_upload(user_id).is_ok());
        Ok(())
    }

    #[tokio::test]
    async fn postgres_source_reservation_atomic_mutations_and_heartbeat_recover_safely(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(());
        };
        let _recovery_guard = POSTGRES_RECOVERY_TEST_LOCK.lock().await;
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query("INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'Reservation test', 'web')")
            .bind(device_id)
            .bind(user_id)
            .execute(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(user_id)
        .execute(&pool)
        .await?;
        let blob_root = std::env::temp_dir().join(format!("lumi-reservation-{}", Uuid::now_v7()));
        let base = ImportService::local(pool.clone(), blob_root.clone());
        let service = Arc::new(ImportService {
            worker_slots: Arc::new(Semaphore::new(0)),
            ..base
        });
        let second_base = ImportService::local(pool.clone(), blob_root.clone());
        let second_recoverer = Arc::new(ImportService {
            worker_slots: Arc::new(Semaphore::new(0)),
            ..second_base
        });

        let source = b"durable-but-invalid-epub";
        let source_hash = content_hash(source);
        service.blobs.put(&source_hash, source).await?;
        let reserved_material = Uuid::now_v7();
        let reserved_job = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'epub', 'reserved source', 'active', $4, 'queued')")
            .bind(reserved_material).bind(space_id).bind(user_id)
            .bind(serde_json::json!({"format":"epub","source_name":"reserved.epub","source_hash":source_hash}))
            .execute(&pool).await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, lease_expires_at) VALUES ($1, $2, $3, 'reserving_source', 'source_accepted', $4, $5, 'reservation-present', 'epub', now() - interval '1 second')")
            .bind(reserved_job).bind(user_id).bind(space_id)
            .bind(serde_json::json!({"kind":"epub","blob_hash":source_hash,"file_name":"reserved.epub","media_type":EPUB_SOURCE_MEDIA_TYPE,"device_id":device_id}))
            .bind(reserved_material).execute(&pool).await?;

        let missing_hash = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let missing_material = Uuid::now_v7();
        let missing_job = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'telegram', 'missing source', 'active', $4, 'queued')")
            .bind(missing_material).bind(space_id).bind(user_id)
            .bind(serde_json::json!({"format":"telegram","source_name":"telegram:1/1","source_hash":missing_hash}))
            .execute(&pool).await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, lease_expires_at) VALUES ($1, $2, $3, 'reserving_source', 'source_accepted', $4, $5, 'reservation-missing', 'telegram', now() - interval '1 second')")
            .bind(missing_job).bind(user_id).bind(space_id)
            .bind(serde_json::json!({"kind":"telegram_text","snapshot_blob_hash":missing_hash,"device_id":device_id}))
            .bind(missing_material).execute(&pool).await?;

        let (first_recovery, second_recovery) =
            tokio::join!(service.recover(), second_recoverer.recover());
        first_recovery?;
        second_recovery?;
        let reserved_status: String =
            sqlx::query_scalar("SELECT status FROM import_jobs WHERE job_id = $1")
                .bind(reserved_job)
                .fetch_one(&pool)
                .await?;
        assert_eq!(reserved_status, "queued");
        let blob_recorded: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM blobs WHERE content_hash = $1)")
                .bind(&source_hash)
                .fetch_one(&pool)
                .await?;
        assert!(blob_recorded);
        let missing: (String, String) = sqlx::query_as(
            "SELECT j.status, m.import_status FROM import_jobs j JOIN materials m ON m.material_id = j.result_material_id WHERE j.job_id = $1",
        )
        .bind(missing_job)
        .fetch_one(&pool)
        .await?;
        assert_eq!(missing, ("failed".to_owned(), "failed".to_owned()));

        let writer_source = b"writer-is-still-producing-source";
        let writer_hash = content_hash(writer_source);
        let writer_material = Uuid::now_v7();
        let writer_job = Uuid::now_v7();
        let writer_claim = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'telegram', 'active reservation writer', 'active', $4, 'queued')")
            .bind(writer_material).bind(space_id).bind(user_id)
            .bind(serde_json::json!({"format":"telegram","source_name":"telegram:2/2","source_hash":writer_hash}))
            .execute(&pool).await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, worker_claim_id, lease_expires_at) VALUES ($1, $2, $3, 'reserving_source', 'source_accepted', $4, $5, 'reservation-active-writer', 'telegram', $6, now() + interval '100 milliseconds')")
            .bind(writer_job).bind(user_id).bind(space_id)
            .bind(serde_json::json!({"kind":"telegram_text","snapshot_blob_hash":writer_hash,"device_id":device_id}))
            .bind(writer_material).bind(writer_claim).execute(&pool).await?;
        let writer_heartbeat = service.spawn_reservation_heartbeat(writer_job, writer_claim);
        tokio::time::sleep(std::time::Duration::from_millis(175)).await;
        let (first_during_writer, second_during_writer) =
            tokio::join!(service.recover(), second_recoverer.recover());
        first_during_writer?;
        second_during_writer?;
        let writer_still_reserved: (String, Option<Uuid>) =
            sqlx::query_as("SELECT status, worker_claim_id FROM import_jobs WHERE job_id = $1")
                .bind(writer_job)
                .fetch_one(&pool)
                .await?;
        assert_eq!(
            writer_still_reserved,
            ("reserving_source".to_owned(), Some(writer_claim))
        );
        service
            .persist_reserved_blob(&PendingBlob {
                hash: writer_hash,
                media_type: "application/vnd.lumi.telegram-message+json",
                bytes: writer_source.to_vec(),
            })
            .await?;
        writer_heartbeat.abort();
        service
            .activate_reserved_source(writer_job, writer_claim)
            .await?;
        let writer_activated: String =
            sqlx::query_scalar("SELECT status FROM import_jobs WHERE job_id = $1")
                .bind(writer_job)
                .fetch_one(&pool)
                .await?;
        assert_eq!(writer_activated, "queued");

        let mutation_material = Uuid::now_v7();
        let mutation_job = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'web_page', 'atomic mutation', 'active', $4, 'queued')")
            .bind(mutation_material).bind(space_id).bind(user_id)
            .bind(serde_json::json!({"format":"web_page","source_name":"https://example.com","source_hash":"atomic"}))
            .execute(&pool).await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind) VALUES ($1, $2, $3, 'queued', 'source_accepted', $4, $5, 'atomic-mutation', 'web_page')")
            .bind(mutation_job).bind(user_id).bind(space_id)
            .bind(serde_json::json!({"kind":"web_page","url":"https://example.com","snapshot_blob_hash":null,"device_id":device_id}))
            .bind(mutation_material).execute(&pool).await?;
        let suffix = Uuid::now_v7().simple().to_string();
        let function_name = format!("lumi_test_fail_material_{suffix}");
        let trigger_name = format!("lumi_test_fail_material_trigger_{suffix}");
        let create_function = format!(
            "CREATE FUNCTION {function_name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'forced material projection failure'; END $$"
        );
        sqlx::query(&create_function).execute(&pool).await?;
        let create_trigger = format!(
            "CREATE TRIGGER {trigger_name} BEFORE UPDATE ON materials FOR EACH ROW WHEN (OLD.material_id = '{mutation_material}'::uuid) EXECUTE FUNCTION {function_name}()"
        );
        sqlx::query(&create_trigger).execute(&pool).await?;
        assert!(matches!(
            service.cancel(user_id, mutation_job).await,
            Err(ImportServiceError::Unavailable)
        ));
        let after_cancel: (String, String) = sqlx::query_as(
            "SELECT j.status, m.import_status FROM import_jobs j JOIN materials m ON m.material_id = j.result_material_id WHERE j.job_id = $1",
        )
        .bind(mutation_job)
        .fetch_one(&pool)
        .await?;
        assert_eq!(after_cancel, ("queued".to_owned(), "queued".to_owned()));
        sqlx::query(&format!("DROP TRIGGER {trigger_name} ON materials"))
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE import_jobs SET status = 'failed' WHERE job_id = $1")
            .bind(mutation_job)
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE materials SET import_status = 'failed' WHERE material_id = $1")
            .bind(mutation_material)
            .execute(&pool)
            .await?;
        sqlx::query(&create_trigger).execute(&pool).await?;
        assert!(matches!(
            service.retry(user_id, mutation_job).await,
            Err(ImportServiceError::Unavailable)
        ));
        let after_retry: (String, String) = sqlx::query_as(
            "SELECT j.status, m.import_status FROM import_jobs j JOIN materials m ON m.material_id = j.result_material_id WHERE j.job_id = $1",
        )
        .bind(mutation_job)
        .fetch_one(&pool)
        .await?;
        assert_eq!(after_retry, ("failed".to_owned(), "failed".to_owned()));
        sqlx::query(&format!("DROP TRIGGER {trigger_name} ON materials"))
            .execute(&pool)
            .await?;
        sqlx::query(&format!("DROP FUNCTION {function_name}()"))
            .execute(&pool)
            .await?;

        let session = AuthenticatedSession {
            user_id,
            session_id: Uuid::now_v7(),
            device_id,
            csrf_hash: [0; 32],
            instance_role: lumi_core::InstanceRole::User,
        };
        let lock_key = "nonblocking-idempotency";
        let mut lock_tx = pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("{space_id}:{lock_key}"))
            .execute(&mut *lock_tx)
            .await?;
        assert!(matches!(
            service
                .accept_web(&session, "https://example.com/idempotent", lock_key)
                .await,
            Err(ImportServiceError::RateLimited)
        ));
        lock_tx.rollback().await?;

        let heartbeat_service = Arc::new(ImportService::local(
            pool.clone(),
            blob_root.join("heartbeat"),
        ));
        let heartbeat_material = Uuid::now_v7();
        let heartbeat_job = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'epub', 'heartbeat cleanup', 'active', $4, 'queued')")
            .bind(heartbeat_material).bind(space_id).bind(user_id)
            .bind(serde_json::json!({"format":"epub","source_name":"heartbeat.epub","source_hash":missing_hash}))
            .execute(&pool).await?;
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind) VALUES ($1, $2, $3, 'queued', 'source_accepted', $4, $5, 'heartbeat-cleanup', 'epub')")
            .bind(heartbeat_job).bind(user_id).bind(space_id)
            .bind(serde_json::json!({"kind":"epub","blob_hash":missing_hash,"file_name":"heartbeat.epub","media_type":EPUB_SOURCE_MEDIA_TYPE,"device_id":device_id}))
            .bind(heartbeat_material).execute(&pool).await?;
        let heartbeat_suffix = Uuid::now_v7().simple().to_string();
        let heartbeat_function = format!("lumi_test_fail_stage_{heartbeat_suffix}");
        let heartbeat_trigger = format!("lumi_test_fail_stage_trigger_{heartbeat_suffix}");
        sqlx::query(&format!("CREATE FUNCTION {heartbeat_function}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'forced worker infrastructure failure'; END $$"))
            .execute(&pool).await?;
        sqlx::query(&format!("CREATE TRIGGER {heartbeat_trigger} BEFORE UPDATE ON import_jobs FOR EACH ROW WHEN (OLD.job_id = '{heartbeat_job}'::uuid AND NEW.stage = 'validating_container') EXECUTE FUNCTION {heartbeat_function}()"))
            .execute(&pool).await?;
        assert!(matches!(
            Arc::clone(&heartbeat_service).run(heartbeat_job).await,
            Err(ImportServiceError::Unavailable)
        ));
        let (old_claim, old_fence): (Uuid, i64) = sqlx::query_as(
            "SELECT worker_claim_id, worker_fence FROM import_jobs WHERE job_id = $1",
        )
        .bind(heartbeat_job)
        .fetch_one(&pool)
        .await?;
        let stale_claim = ImportWorkerClaim {
            job_id: heartbeat_job,
            claim_id: old_claim,
            fence: old_fence,
        };
        let lease_after_exit: OffsetDateTime =
            sqlx::query_scalar("SELECT lease_expires_at FROM import_jobs WHERE job_id = $1")
                .bind(heartbeat_job)
                .fetch_one(&pool)
                .await?;
        tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        let lease_later: OffsetDateTime =
            sqlx::query_scalar("SELECT lease_expires_at FROM import_jobs WHERE job_id = $1")
                .bind(heartbeat_job)
                .fetch_one(&pool)
                .await?;
        assert_eq!(lease_later, lease_after_exit);
        sqlx::query(&format!("DROP TRIGGER {heartbeat_trigger} ON import_jobs"))
            .execute(&pool)
            .await?;
        sqlx::query(&format!("DROP FUNCTION {heartbeat_function}()"))
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE import_jobs SET lease_expires_at = now() - interval '1 second' WHERE job_id = $1")
            .bind(heartbeat_job).execute(&pool).await?;
        heartbeat_service.recover().await?;
        for _ in 0..100 {
            let status: String =
                sqlx::query_scalar("SELECT status FROM import_jobs WHERE job_id = $1")
                    .bind(heartbeat_job)
                    .fetch_one(&pool)
                    .await?;
            if status == "failed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let recovered: (String, Option<Uuid>) =
            sqlx::query_as("SELECT status, worker_claim_id FROM import_jobs WHERE job_id = $1")
                .bind(heartbeat_job)
                .fetch_one(&pool)
                .await?;
        assert_eq!(recovered, ("failed".to_owned(), None));
        assert!(matches!(
            heartbeat_service.set_stage(stale_claim, "persisting").await,
            Err(ImportServiceError::Conflict)
        ));
        assert!(matches!(
            heartbeat_service
                .fail(
                    stale_claim,
                    heartbeat_material,
                    1,
                    source_unavailable_diagnostic("epub"),
                    false,
                )
                .await,
            Err(ImportServiceError::Conflict)
        ));
        let _ = tokio::fs::remove_dir_all(blob_root).await;
        Ok(())
    }
}
