//! Durable real EPUB upload, worker lifecycle and PostgreSQL projections.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use lumi_core::{
    content_hash, import_epub, AcceptedImport, BlobManifest, BlobRef, BlobRole, DiagnosticSeverity,
    DocumentRevision, DocumentRevisionId, EpubImportError, EpubImportRequest, EpubLimits,
    ImportDiagnostic, ImportStatusEntry, ImportedEpub, Job, JobId, JobKind, JobStage, JobStatus,
    MaterialId, NormalizedContentPackage, ReadingDocument, ReadingNode, ReadingNodeKind,
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
            "SELECT m.material_id, m.canonical_title, j.job_id FROM materials m JOIN import_jobs j ON j.job_id = m.latest_import_job_id WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL ORDER BY m.created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(log_storage_error)?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let job_id: Uuid = row.try_get("job_id").map_err(log_storage_error)?;
            entries.push(ImportStatusEntry {
                material_id: row.try_get("material_id").map_err(log_storage_error)?,
                title: row.try_get("canonical_title").map_err(log_storage_error)?,
                job: self.job(user_id, job_id).await?,
            });
        }
        Ok(entries)
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
}
