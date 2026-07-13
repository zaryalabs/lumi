//! Durable real EPUB upload, worker lifecycle and PostgreSQL projections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lumi_core::{
    content_hash, import_epub, AcceptedImport, Annotation, AnnotationExport, AnnotationKind,
    BlobManifest, BlobRef, BlobRole, CreateAnnotationCommand, DeleteAnnotationCommand,
    DiagnosticSeverity, DocumentRevision, DocumentRevisionId, EpubImportError, EpubImportRequest,
    EpubLimits, ImportDiagnostic, ImportStatusEntry, ImportedEpub, Job, JobId, JobKind, JobStage,
    JobStatus, LibraryEntry, LibraryState, Material, MaterialId, MaterialImportStatus,
    MaterialKind, MoveReadingPositionCommand, NormalizedContentPackage, ReaderSettings,
    ReadingDocument, ReadingNode, ReadingNodeKind, ReadingProgress, RenderPlan, SourceIdentity,
    UpdateAnnotationCommand,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, PgRow, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::account::AuthenticatedSession;
use crate::blob::{BlobStore, BlobStoreError, LocalBlobStore, StoredBlob};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
    #[cfg(test)]
    pub(crate) use sqlx_core::query_as::query_as;
    pub(crate) use sqlx_core::query_scalar::query_scalar;
}

const SOURCE_MEDIA_TYPE: &str = "application/epub+zip";

#[derive(Debug, thiserror::Error)]
pub(crate) enum ImportServiceError {
    #[error("import object was not found")]
    NotFound,
    #[error("import command conflicts with current state")]
    Conflict,
    #[error("invalid import request: {0}")]
    BadRequest(&'static str),
    #[error("import service is unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub(crate) struct ImportService {
    pool: PgPool,
    blobs: Arc<dyn BlobStore>,
    cancellations: Arc<Mutex<HashMap<JobId, Arc<AtomicBool>>>>,
}

impl ImportService {
    pub(crate) fn local(pool: PgPool, blob_root: PathBuf) -> Self {
        Self {
            pool,
            blobs: Arc::new(LocalBlobStore::new(blob_root)),
            cancellations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn recover(self: &Arc<Self>) -> Result<(), ImportServiceError> {
        let cancelled = sqlx::query(
            "UPDATE import_jobs SET status = 'cancelled', finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE status IN ('queued', 'running') AND cancellation_requested = true RETURNING result_material_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        for row in cancelled {
            let material_id: Option<Uuid> = row
                .try_get("result_material_id")
                .map_err(log_storage_error)?;
            if let Some(material_id) = material_id {
                sqlx::query("UPDATE materials SET import_status = 'cancelled', updated_at = now() WHERE material_id = $1")
                    .bind(material_id)
                    .execute(&self.pool)
                    .await
                    .map_err(log_storage_error)?;
            }
        }

        let exhausted = sqlx::query(
            "UPDATE import_jobs SET status = 'failed', error_code = 'epub_retry_exhausted', finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE status IN ('queued', 'running') AND attempt >= max_attempts RETURNING job_id, result_material_id, attempt",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        for row in exhausted {
            let job_id: Uuid = row.try_get("job_id").map_err(log_storage_error)?;
            let attempt: i32 = row.try_get("attempt").map_err(log_storage_error)?;
            insert_diagnostic_pool(
                &self.pool,
                job_id,
                attempt.max(1),
                &ImportDiagnostic {
                    severity: DiagnosticSeverity::Error,
                    code: "epub_retry_exhausted".to_owned(),
                    message: "Import recovery exhausted the configured retry budget.".to_owned(),
                    source_path: None,
                },
            )
            .await?;
            let material_id: Option<Uuid> = row
                .try_get("result_material_id")
                .map_err(log_storage_error)?;
            if let Some(material_id) = material_id {
                sqlx::query("UPDATE materials SET import_status = 'failed', updated_at = now() WHERE material_id = $1")
                    .bind(material_id)
                    .execute(&self.pool)
                    .await
                    .map_err(log_storage_error)?;
            }
        }

        sqlx::query(
            "UPDATE import_jobs SET status = 'queued', stage = 'source_accepted', started_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE status = 'running' AND attempt < max_attempts AND cancellation_requested = false",
        )
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
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

    pub(crate) async fn accept(
        self: &Arc<Self>,
        session: &AuthenticatedSession,
        file_name: &str,
        idempotency_key: &str,
        source: Vec<u8>,
    ) -> Result<AcceptedImport, ImportServiceError> {
        let file_name = safe_file_name(file_name)?;
        if idempotency_key.trim().is_empty() || idempotency_key.len() > 200 {
            return Err(ImportServiceError::BadRequest(
                "Idempotency-Key must contain 1 to 200 characters",
            ));
        }
        if source.is_empty() {
            return Err(ImportServiceError::BadRequest("EPUB upload is empty"));
        }
        let source_len = u64::try_from(source.len()).unwrap_or(u64::MAX);
        if source_len > EpubLimits::s1().source_bytes {
            return Err(ImportServiceError::BadRequest(
                "EPUB upload exceeds the 100 MiB source limit",
            ));
        }
        let source_hash = content_hash(&source);
        let stored = self
            .blobs
            .put(&source_hash, &source)
            .await
            .map_err(map_blob_error)?;
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
        if let Some(row) = sqlx::query(
            "SELECT request_hash, response_body FROM idempotency_keys WHERE scope_id = $1 AND idempotency_key = $2",
        )
        .bind(space_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        {
            let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(log_storage_error)?;
            if stored_hash.as_slice() != request_hash.as_slice() {
                return Err(ImportServiceError::Conflict);
            }
            let response: serde_json::Value = row.try_get("response_body").map_err(log_storage_error)?;
            return serde_json::from_value(response).map_err(|_| ImportServiceError::Unavailable);
        }

        insert_blob_record(&mut tx, &source_hash, SOURCE_MEDIA_TYPE, &stored).await?;
        let material_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        let title = upload_title(&file_name);
        let source_identity = serde_json::json!({
            "format": "epub",
            "source_name": file_name,
            "source_hash": source_hash,
        });
        sqlx::query(
            "INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status, created_at, updated_at) VALUES ($1, $2, $3, 'epub', $4, 'active', $5, 'queued', $6, $6)",
        )
        .bind(material_id)
        .bind(space_id)
        .bind(session.user_id)
        .bind(&title)
        .bind(source_identity)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let source_ref = SourceRef {
            blob_hash: source_hash.clone(),
            file_name: file_name.clone(),
            media_type: SOURCE_MEDIA_TYPE.to_owned(),
            device_id: session.device_id,
        };
        sqlx::query(
            "INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, created_at, updated_at) VALUES ($1, $2, $3, 'queued', 'source_accepted', $4, $5, $6, $7, $7)",
        )
        .bind(job_id)
        .bind(session.user_id)
        .bind(space_id)
        .bind(serde_json::to_value(&source_ref).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(material_id)
        .bind(idempotency_key)
        .bind(now)
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
                payload: serde_json::json!({ "kind": "epub", "import_status": "queued" }),
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
        self.spawn(job_id);
        Ok(accepted)
    }

    pub(crate) async fn list(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ImportStatusEntry>, ImportServiceError> {
        let rows = sqlx::query(
            "SELECT m.material_id, m.owner_user_id, m.kind, m.canonical_title, m.title_override, m.active_revision_id, m.library_state, m.source_identity, m.import_status, m.created_at, m.updated_at, j.job_id FROM materials m JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL ORDER BY m.updated_at DESC, m.material_id DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let job_id: Uuid = row.try_get("job_id").map_err(log_storage_error)?;
            entries.push(library_entry_from_row(
                &row,
                self.job(user_id, job_id).await?,
            )?);
        }
        Ok(entries)
    }

    pub(crate) async fn material(
        &self,
        user_id: Uuid,
        material_id: MaterialId,
    ) -> Result<LibraryEntry, ImportServiceError> {
        let row = sqlx::query(
            "SELECT m.material_id, m.owner_user_id, m.kind, m.canonical_title, m.title_override, m.active_revision_id, m.library_state, m.source_identity, m.import_status, m.created_at, m.updated_at, j.job_id FROM materials m JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.material_id = $1 AND m.owner_user_id = $2 AND m.deleted_at IS NULL",
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
        let row = sqlx::query(
            "UPDATE import_jobs SET cancellation_requested = true, status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END, finished_at = CASE WHEN status = 'queued' THEN now() ELSE finished_at END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('queued', 'running') RETURNING result_material_id, status",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::Conflict)?;
        if let Ok(flags) = self.cancellations.lock() {
            if let Some(flag) = flags.get(&job_id) {
                flag.store(true, Ordering::Release);
            }
        }
        let status: String = row.try_get("status").map_err(log_storage_error)?;
        if status == "cancelled" {
            let material_id: Option<Uuid> = row
                .try_get("result_material_id")
                .map_err(log_storage_error)?;
            if let Some(material_id) = material_id {
                sqlx::query("UPDATE materials SET import_status = 'cancelled', updated_at = now() WHERE material_id = $1")
                    .bind(material_id)
                    .execute(&self.pool)
                    .await
                    .map_err(log_storage_error)?;
            }
        }
        self.job(user_id, job_id).await
    }

    pub(crate) async fn retry(
        self: &Arc<Self>,
        user_id: Uuid,
        job_id: JobId,
    ) -> Result<Job, ImportServiceError> {
        let updated = sqlx::query(
            "UPDATE import_jobs SET status = 'queued', stage = 'source_accepted', cancellation_requested = false, error_code = NULL, started_at = NULL, finished_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('failed', 'cancelled') AND attempt < max_attempts RETURNING result_material_id",
        )
        .bind(job_id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::Conflict)?;
        let material_id: Option<Uuid> = updated
            .try_get("result_material_id")
            .map_err(log_storage_error)?;
        if let Some(material_id) = material_id {
            sqlx::query("UPDATE materials SET import_status = 'queued', updated_at = now() WHERE material_id = $1")
                .bind(material_id)
                .execute(&self.pool)
                .await
                .map_err(log_storage_error)?;
        }
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
        let source_ref: SourceRef =
            serde_json::from_value(value).map_err(|_| ImportServiceError::Unavailable)?;
        let bytes = self
            .blobs
            .get(&source_ref.blob_hash)
            .await
            .map_err(map_blob_error)?;
        Ok((source_ref.file_name, source_ref.media_type, bytes))
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
        let package: NormalizedContentPackage =
            serde_json::from_value(payload).map_err(|_| ImportServiceError::Unavailable)?;
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
            diagnostics: package.diagnostics,
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
        let package_value: serde_json::Value = sqlx::query_scalar(
            "SELECT p.payload FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id WHERE p.revision_id = $1 AND r.material_id = $2 AND r.space_id = $3",
        )
        .bind(command.revision_id)
        .bind(command.material_id)
        .bind(space_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let package: NormalizedContentPackage =
            serde_json::from_value(package_value).map_err(|_| ImportServiceError::Unavailable)?;
        let document = reading_document_from_package(&package, command.material_id);
        validate_progress_locator(&RenderPlan::from_document(&document), &command.locator)?;
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
            "SELECT a.annotation_id, a.material_id, a.revision_id, a.anchor, a.kind, a.object_revision, a.created_at, a.updated_at FROM annotations a JOIN materials m ON m.material_id = a.material_id AND m.space_id = a.space_id WHERE a.material_id = $1 AND m.owner_user_id = $2 AND m.deleted_at IS NULL AND a.deleted_at IS NULL ORDER BY a.created_at, a.annotation_id",
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
        validate_annotation_kind(&command.kind)?;
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
        let package_value: serde_json::Value = sqlx::query_scalar(
            "SELECT p.payload FROM normalized_packages p JOIN document_revisions r ON r.revision_id = p.revision_id JOIN materials m ON m.material_id = r.material_id AND m.space_id = r.space_id WHERE p.revision_id = $1 AND r.material_id = $2 AND m.owner_user_id = $3 AND m.active_revision_id = $1 AND m.deleted_at IS NULL",
        )
        .bind(command.revision_id)
        .bind(command.material_id)
        .bind(session.user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?
        .ok_or(ImportServiceError::NotFound)?;
        let package: NormalizedContentPackage =
            serde_json::from_value(package_value).map_err(|_| ImportServiceError::Unavailable)?;
        let document = reading_document_from_package(&package, command.material_id);
        validate_anchor_exact(&RenderPlan::from_document(&document), &command.anchor)?;
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
        let annotation = Annotation::create(command, timestamp_ms(now));
        let anchor = serde_json::to_value(&annotation.anchor)
            .map_err(|_| ImportServiceError::Unavailable)?;
        let kind =
            serde_json::to_value(&annotation.kind).map_err(|_| ImportServiceError::Unavailable)?;
        sqlx::query(
            "INSERT INTO annotations (annotation_id, space_id, material_id, revision_id, kind, anchor, object_revision, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $7)",
        )
        .bind(annotation.id)
        .bind(space_id)
        .bind(annotation.material_id)
        .bind(annotation.revision_id)
        .bind(kind)
        .bind(anchor)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
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
        command: UpdateAnnotationCommand,
        idempotency_key: &str,
    ) -> Result<Annotation, ImportServiceError> {
        validate_idempotency_key(idempotency_key)?;
        validate_annotation_kind(&command.kind)?;
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
        let row = sqlx::query(
            "UPDATE annotations SET kind = $1, object_revision = object_revision + 1, updated_at = $2 WHERE annotation_id = $3 AND material_id = $4 AND space_id = $5 AND object_revision = $6 AND deleted_at IS NULL RETURNING annotation_id, material_id, revision_id, anchor, kind, object_revision, created_at, updated_at",
        )
        .bind(serde_json::to_value(&command.kind).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(now)
        .bind(command.annotation_id)
        .bind(command.material_id)
        .bind(space_id)
        .bind(i64::try_from(command.expected_revision).map_err(|_| ImportServiceError::Conflict)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let annotation = match row {
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
            "UPDATE annotations SET object_revision = object_revision + 1, updated_at = $1, deleted_at = $1 WHERE annotation_id = $2 AND material_id = $3 AND space_id = $4 AND object_revision = $5 AND deleted_at IS NULL RETURNING annotation_id, material_id, revision_id, anchor, kind, object_revision, created_at, updated_at",
        )
        .bind(now)
        .bind(command.annotation_id)
        .bind(command.material_id)
        .bind(space_id)
        .bind(i64::try_from(command.expected_revision).map_err(|_| ImportServiceError::Conflict)?)
        .fetch_optional(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let annotation = match row {
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
            "SELECT annotation_id, material_id, revision_id, anchor, kind, object_revision, created_at, updated_at FROM annotations WHERE material_id = $1 AND space_id = (SELECT space_id FROM materials WHERE material_id = $1 AND owner_user_id = $2) AND deleted_at IS NULL ORDER BY created_at, annotation_id",
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
                tracing::error!(%job_id, error = %error, "durable EPUB worker failed");
            }
        });
    }

    async fn run(self: Arc<Self>, job_id: JobId) -> Result<(), ImportServiceError> {
        let row = sqlx::query(
            "UPDATE import_jobs SET status = 'running', stage = 'validating_container', attempt = import_jobs.attempt + 1, started_at = now(), updated_at = now(), object_revision = import_jobs.object_revision + 1 FROM materials m WHERE import_jobs.job_id = $1 AND import_jobs.status = 'queued' AND import_jobs.cancellation_requested = false AND import_jobs.attempt < import_jobs.max_attempts AND m.material_id = import_jobs.result_material_id RETURNING import_jobs.user_id, import_jobs.space_id, import_jobs.result_material_id, import_jobs.source_ref, import_jobs.attempt",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let Some(row) = row else {
            return Ok(());
        };
        let user_id: Uuid = row.try_get("user_id").map_err(log_storage_error)?;
        let space_id: Uuid = row.try_get("space_id").map_err(log_storage_error)?;
        let material_id: Uuid = row
            .try_get("result_material_id")
            .map_err(log_storage_error)?;
        let attempt: i32 = row.try_get("attempt").map_err(log_storage_error)?;
        let source_ref: SourceRef =
            serde_json::from_value(row.try_get("source_ref").map_err(log_storage_error)?)
                .map_err(|_| ImportServiceError::Unavailable)?;
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

        let source = match self.blobs.get(&source_ref.blob_hash).await {
            Ok(source) => source,
            Err(_) => {
                self.fail(
                    job_id,
                    material_id,
                    attempt,
                    ImportDiagnostic {
                        severity: DiagnosticSeverity::Error,
                        code: "epub_source_blob_unavailable".to_owned(),
                        message: "The uploaded EPUB source blob is unavailable.".to_owned(),
                        source_path: None,
                    },
                    false,
                )
                .await?;
                self.remove_cancellation(job_id);
                return Ok(());
            }
        };
        sqlx::query(
            "UPDATE import_jobs SET stage = 'normalizing', updated_at = now() WHERE job_id = $1",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let revision_id = Uuid::now_v7();
        let source_name = source_ref.file_name.clone();
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
            Ok(imported) => {
                if cancellation.load(Ordering::Acquire) {
                    self.fail(
                        job_id,
                        material_id,
                        attempt,
                        EpubImportError::Cancelled.diagnostic(),
                        true,
                    )
                    .await?;
                } else {
                    self.persist_success(job_id, space_id, &source_ref, attempt, imported)
                        .await?;
                }
            }
            Err(error) => {
                let cancelled = matches!(error, EpubImportError::Cancelled);
                self.fail(job_id, material_id, attempt, error.diagnostic(), cancelled)
                    .await?;
            }
        }
        self.remove_cancellation(job_id);
        Ok(())
    }

    async fn persist_success(
        &self,
        job_id: JobId,
        space_id: Uuid,
        source_ref: &SourceRef,
        attempt: i32,
        mut imported: ImportedEpub,
    ) -> Result<(), ImportServiceError> {
        sqlx::query(
            "UPDATE import_jobs SET stage = 'persisting', updated_at = now() WHERE job_id = $1",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(log_storage_error)?;
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
        let package_bytes =
            serde_json::to_vec(&imported.package).map_err(|_| ImportServiceError::Unavailable)?;
        let package_blob_hash = content_hash(&package_bytes);
        let stored_package = self
            .blobs
            .put(&package_blob_hash, &package_bytes)
            .await
            .map_err(map_blob_error)?;
        let source_map = source_map(&imported.package)?;
        let now = OffsetDateTime::now_utc();
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
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
        sqlx::query(
            "INSERT INTO document_revisions (revision_id, material_id, space_id, source_format, source_hash, importer_id, importer_version, created_at, normalized_hash, package_format_version, source_blob_hash, supersedes_revision_id) VALUES ($1, $2, $3, 'epub', $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(imported.revision.id)
        .bind(imported.revision.material_id)
        .bind(space_id)
        .bind(&imported.revision.source_hash)
        .bind(&imported.revision.importer_id)
        .bind(&imported.revision.importer_version)
        .bind(now)
        .bind(&imported.revision.normalized_hash)
        .bind(&imported.revision.package_format_version)
        .bind(&source_ref.blob_hash)
        .bind(imported.revision.supersedes_revision_id)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        let manifest_id = imported.package.resources.id;
        sqlx::query(
            "INSERT INTO blob_manifests (manifest_id, space_id, schema_version, created_at) VALUES ($1, $2, $3, $4)",
        )
        .bind(manifest_id)
        .bind(space_id)
        .bind(&imported.package.resources.schema_version)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        insert_manifest_entry(
            &mut tx,
            manifest_id,
            &source_ref.blob_hash,
            &format!("source/{}", source_ref.file_name),
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
        .bind(imported.package.id)
        .bind(imported.revision.id)
        .bind(&imported.revision.package_format_version)
        .bind(serde_json::to_value(&imported.package).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(source_map)
        .bind(manifest_id)
        .bind(&package_blob_hash)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        sqlx::query(
            "UPDATE materials SET canonical_title = $2, active_revision_id = $3, import_status = 'ready', object_revision = object_revision + 1, updated_at = $4 WHERE material_id = $1",
        )
        .bind(imported.revision.material_id)
        .bind(&imported.title)
        .bind(imported.revision.id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        for diagnostic in &imported.revision.diagnostics {
            insert_diagnostic(&mut tx, job_id, attempt, diagnostic).await?;
        }
        sqlx::query(
            "UPDATE import_jobs SET status = 'succeeded', stage = 'committed', revision_id = $2, error_code = NULL, finished_at = $3, updated_at = $3, object_revision = object_revision + 1 WHERE job_id = $1",
        )
        .bind(job_id)
        .bind(imported.revision.id)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
        append_import_change(
            &mut tx,
            ImportChange {
                space_id,
                object_id: imported.revision.material_id,
                device_id: source_ref.device_id,
                idempotency_key: &format!("{job_id}:complete"),
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
        job_id: JobId,
        material_id: MaterialId,
        attempt: i32,
        diagnostic: ImportDiagnostic,
        cancelled: bool,
    ) -> Result<(), ImportServiceError> {
        let now = OffsetDateTime::now_utc();
        let status = if cancelled { "cancelled" } else { "failed" };
        let mut tx = self.pool.begin().await.map_err(log_storage_error)?;
        insert_diagnostic(&mut tx, job_id, attempt, &diagnostic).await?;
        sqlx::query(
            "UPDATE import_jobs SET status = $2, error_code = $3, finished_at = $4, updated_at = $4, object_revision = object_revision + 1 WHERE job_id = $1",
        )
        .bind(job_id)
        .bind(status)
        .bind(&diagnostic.code)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(log_storage_error)?;
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SourceRef {
    blob_hash: String,
    file_name: String,
    media_type: String,
    device_id: Uuid,
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

async fn insert_diagnostic_pool(
    pool: &PgPool,
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
    .execute(pool)
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
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("{space_id}:{idempotency_key}"))
        .execute(&mut **tx)
        .await
        .map_err(log_storage_error)?;
    Ok(())
}

fn annotation_from_row(row: &PgRow) -> Result<Annotation, ImportServiceError> {
    let anchor: serde_json::Value = row.try_get("anchor").map_err(log_storage_error)?;
    let kind: serde_json::Value = row.try_get("kind").map_err(log_storage_error)?;
    let revision: i64 = row.try_get("object_revision").map_err(log_storage_error)?;
    Ok(Annotation {
        id: row.try_get("annotation_id").map_err(log_storage_error)?,
        material_id: row.try_get("material_id").map_err(log_storage_error)?,
        revision_id: row.try_get("revision_id").map_err(log_storage_error)?,
        anchor: serde_json::from_value(anchor).map_err(|_| ImportServiceError::Unavailable)?,
        kind: serde_json::from_value(kind).map_err(|_| ImportServiceError::Unavailable)?,
        revision: u64::try_from(revision).map_err(|_| ImportServiceError::Unavailable)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(log_storage_error)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(log_storage_error)?),
    })
}

fn validate_annotation_kind(kind: &AnnotationKind) -> Result<(), ImportServiceError> {
    if let AnnotationKind::Note { body } = kind {
        if body.trim().is_empty() {
            return Err(ImportServiceError::BadRequest(
                "note body must not be empty",
            ));
        }
        if body.len() > 100_000 {
            return Err(ImportServiceError::BadRequest(
                "note body exceeds the 100,000 byte limit",
            ));
        }
    }
    Ok(())
}

fn validate_anchor_shape(
    revision_id: DocumentRevisionId,
    anchor: &lumi_core::Anchor,
) -> Result<(), ImportServiceError> {
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
    Ok(LibraryEntry {
        id: row.try_get("material_id").map_err(log_storage_error)?,
        owner_id: row.try_get("owner_user_id").map_err(log_storage_error)?,
        kind: match kind.as_str() {
            "epub" => MaterialKind::Epub,
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
        "queued" => Ok(JobStatus::Queued),
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
        "validating_container" => Ok(JobStage::ValidatingContainer),
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
    if name.is_empty() || !name.to_ascii_lowercase().ends_with(".epub") {
        Err(ImportServiceError::BadRequest(
            "upload must have a non-empty .epub file name",
        ))
    } else {
        Ok(name)
    }
}

fn upload_title(file_name: &str) -> String {
    file_name
        .strip_suffix(".epub")
        .or_else(|| file_name.strip_suffix(".EPUB"))
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
    tracing::error!(%error, "EPUB import repository operation failed");
    ImportServiceError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumi_core::{import_epub_fixture, rich_epub_fixture, AnnotationKind, HighlightStyle};

    #[test]
    fn safe_file_name_should_strip_client_path() {
        let name = safe_file_name("C:\\fakepath\\book.epub");

        assert!(matches!(name.as_deref(), Ok("book.epub")));
    }

    #[test]
    fn safe_file_name_should_reject_non_epub_extension() -> Result<(), Box<dyn std::error::Error>> {
        let Err(error) = safe_file_name("book.html") else {
            return Err(std::io::Error::other("non-EPUB extension was accepted").into());
        };

        assert!(matches!(error, ImportServiceError::BadRequest(_)));
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
            kind: AnnotationKind::Note {
                body: "original".to_owned(),
            },
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
                    kind: AnnotationKind::Note {
                        body: "edited".to_owned(),
                    },
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
                        kind: AnnotationKind::Note {
                            body: "stale".to_owned(),
                        },
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
        assert_eq!(left?.id, right?.id);

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
            .any(|entry| entry.annotation_id == created.id));
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
}
