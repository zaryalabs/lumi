//! Durable search indexing worker over the common fenced JobRuntime.

use std::sync::Arc;
use std::time::Duration;

use lumi_core::{
    content_hash, Anchor, Annotation, AnnotationKind, AnnotationStatus, AnnotationTarget,
    AnnotationType, FixedLayoutContentPackage, NormalizedContentPackage, SearchChunk,
    SearchIndexState, SearchRebuildReceipt, SEARCH_CHUNKER_VERSION, SEARCH_INDEX_VERSION,
};
use serde_json::Value;
use sqlx_core::query_scalar::query_scalar;
use sqlx_core::row::Row;
use sqlx_postgres::PgPool;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::jobs::{JobFailure, JobRuntime, JobRuntimeError, JobRuntimeStatus, PgJobRepository};

use super::{chunker, document_key, SearchError, SearchRuntime};

const CLAIM_LEASE: Duration = Duration::from_secs(60 * 60);
const IDLE_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct IndexRequest {
    request_id: Uuid,
    job_id: Uuid,
    owner_id: Uuid,
    source_type: String,
    source_id: Uuid,
    operation: String,
}

impl SearchRuntime {
    pub(crate) async fn ensure_initial_rebuilds(&self) -> Result<(), SearchError> {
        let Some(pool) = self.pool.as_ref() else {
            return Ok(());
        };
        if !self.is_query_ready() {
            return Ok(());
        }
        let owners = sqlx_core::query::query(
            "SELECT account.user_id, space.space_id
               FROM accounts account
               JOIN sync_spaces space
                 ON space.owner_user_id = account.user_id
                AND space.kind = 'personal'
                AND space.deleted_at IS NULL
              WHERE NOT EXISTS (
                    SELECT 1 FROM search_account_state state
                     WHERE state.user_id = account.user_id
              )",
        )
        .fetch_all(pool)
        .await
        .map_err(|_| SearchError::Storage)?;
        for row in owners {
            let owner_id = row.try_get("user_id").map_err(|_| SearchError::Storage)?;
            let space_id = row.try_get("space_id").map_err(|_| SearchError::Storage)?;
            enqueue_rebuild_job(pool, owner_id, space_id).await?;
        }
        Ok(())
    }

    pub(crate) async fn enqueue_rebuild(
        &self,
        owner_id: Uuid,
    ) -> Result<SearchRebuildReceipt, SearchError> {
        let Some(pool) = self.pool.as_ref() else {
            let index = self.index.as_ref().ok_or(SearchError::Unavailable)?;
            index
                .rebuild(
                    owner_id,
                    std::iter::empty::<(String, Vec<(SearchChunk, Vec<f32>)>)>(),
                )
                .map_err(SearchError::Index)?;
            return Ok(SearchRebuildReceipt {
                job_id: Uuid::now_v7(),
                state: SearchIndexState::Ready,
            });
        };
        if !self.is_query_ready() {
            return Err(SearchError::Unavailable);
        }
        let space_id: Uuid = query_scalar(
            "SELECT space_id FROM sync_spaces
              WHERE owner_user_id = $1 AND kind = 'personal' AND deleted_at IS NULL",
        )
        .bind(owner_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| SearchError::Storage)?
        .ok_or(SearchError::Storage)?;
        let job_id = enqueue_rebuild_job(pool, owner_id, space_id).await?;
        Ok(SearchRebuildReceipt {
            job_id,
            state: SearchIndexState::Rebuilding,
        })
    }

    /// Run incremental indexing and rebuild jobs until graceful shutdown.
    pub(crate) async fn run_worker(self: Arc<Self>, cancellation: CancellationToken) {
        let Some(pool) = self.pool.clone() else {
            cancellation.cancelled().await;
            return;
        };
        if !self.is_query_ready() {
            cancellation.cancelled().await;
            return;
        }
        let runtime = JobRuntime::new(PgJobRepository::new(pool.clone()));
        if runtime.recover_expired(Some("search_index")).await.is_err() {
            self.set_failure("search_job_recovery_failed");
        }
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                result = self.process_next(&pool, &runtime) => {
                    match result {
                        Ok(true) => {}
                        Ok(false) => tokio::time::sleep(IDLE_DELAY).await,
                        Err(error) => {
                            tracing::warn!(error = %error, "search indexing worker iteration failed");
                            tokio::time::sleep(IDLE_DELAY).await;
                        }
                    }
                }
            }
        }
    }

    async fn process_next(
        &self,
        pool: &PgPool,
        runtime: &JobRuntime<PgJobRepository>,
    ) -> Result<bool, SearchError> {
        let Some(request) = next_request(pool).await? else {
            return Ok(false);
        };
        let claim = match runtime
            .claim(Some(request.owner_id), request.job_id, CLAIM_LEASE)
            .await
        {
            Ok(claim) => claim,
            Err(JobRuntimeError::Conflict | JobRuntimeError::NotFound) => return Ok(true),
            Err(_) => return Err(SearchError::Storage),
        };
        sqlx_core::query::query(
            "UPDATE search_index_requests
                SET status = 'running', updated_at = now()
              WHERE request_id = $1 AND status = 'queued'",
        )
        .bind(request.request_id)
        .execute(pool)
        .await
        .map_err(|_| SearchError::Storage)?;

        let result = self.process_request(pool, &request).await;
        match result {
            Ok(generation) => {
                let mut transaction = pool.begin().await.map_err(|_| SearchError::Storage)?;
                PgJobRepository::complete_in_transaction(
                    &mut transaction,
                    request.owner_id,
                    &claim.claim,
                )
                .await
                .map_err(|_| SearchError::Storage)?;
                sqlx_core::query::query(
                    "UPDATE search_index_requests
                        SET status = 'succeeded', updated_at = now(), finished_at = now()
                      WHERE request_id = $1",
                )
                .bind(request.request_id)
                .execute(&mut *transaction)
                .await
                .map_err(|_| SearchError::Storage)?;
                sqlx_core::query::query(
                    "INSERT INTO search_account_state (
                        user_id, state, index_generation, index_version,
                        chunker_version, model_version, failure_code
                     ) VALUES ($1, 'ready', $2, $3, $4, $5, NULL)
                     ON CONFLICT (user_id) DO UPDATE SET
                        state = 'ready',
                        index_generation = EXCLUDED.index_generation,
                        index_version = EXCLUDED.index_version,
                        chunker_version = EXCLUDED.chunker_version,
                        model_version = EXCLUDED.model_version,
                        failure_code = NULL,
                        updated_at = now()",
                )
                .bind(request.owner_id)
                .bind(i64::try_from(generation).map_err(|_| SearchError::Storage)?)
                .bind(SEARCH_INDEX_VERSION)
                .bind(SEARCH_CHUNKER_VERSION)
                .bind(&self.model_version)
                .execute(&mut *transaction)
                .await
                .map_err(|_| SearchError::Storage)?;
                transaction
                    .commit()
                    .await
                    .map_err(|_| SearchError::Storage)?;
            }
            Err(error) => {
                let failure = JobFailure {
                    code: "search_index_failed".to_owned(),
                    message: "search source indexing failed".to_owned(),
                    retryable: true,
                };
                let record = runtime
                    .fail(Some(request.owner_id), &claim.claim, &failure)
                    .await
                    .map_err(|_| SearchError::Storage)?;
                let request_status = if record.status == JobRuntimeStatus::Queued {
                    "queued"
                } else {
                    "failed"
                };
                sqlx_core::query::query(
                    "UPDATE search_index_requests
                        SET status = $2, updated_at = now(),
                            finished_at = CASE WHEN $2 = 'failed' THEN now() ELSE NULL END
                      WHERE request_id = $1",
                )
                .bind(request.request_id)
                .bind(request_status)
                .execute(pool)
                .await
                .map_err(|_| SearchError::Storage)?;
                if request_status == "failed" {
                    let failure_code = match &error {
                        SearchError::Index(error) => error.code(),
                        SearchError::InvalidRequest => "search_request_invalid",
                        SearchError::Unavailable => "search_unavailable",
                        SearchError::Storage => "search_storage_unavailable",
                    };
                    sqlx_core::query::query(
                        "INSERT INTO search_account_state (
                            user_id, state, index_generation, index_version,
                            chunker_version, model_version, failure_code
                         ) VALUES ($1, 'failed', 0, $2, $3, $4, $5)
                         ON CONFLICT (user_id) DO UPDATE SET
                            state = 'failed',
                            failure_code = EXCLUDED.failure_code,
                            updated_at = now()",
                    )
                    .bind(request.owner_id)
                    .bind(SEARCH_INDEX_VERSION)
                    .bind(SEARCH_CHUNKER_VERSION)
                    .bind(&self.model_version)
                    .bind(failure_code)
                    .execute(pool)
                    .await
                    .map_err(|_| SearchError::Storage)?;
                }
            }
        }
        Ok(true)
    }

    async fn process_request(
        &self,
        pool: &PgPool,
        request: &IndexRequest,
    ) -> Result<u64, SearchError> {
        if request.operation == "rebuild" {
            return self.rebuild_owner(pool, request.owner_id).await;
        }
        let chunks = if request.operation == "delete" {
            Vec::new()
        } else {
            extract_source(pool, request).await?
        };
        let vectorized = self.vectorize(chunks)?;
        let generation = self.replace_vectorized(
            request.owner_id,
            &request.source_type,
            request.source_id,
            &vectorized,
        )?;
        persist_projection(pool, request, &vectorized, &self.model_version).await?;
        Ok(generation)
    }

    async fn rebuild_owner(&self, pool: &PgPool, owner_id: Uuid) -> Result<u64, SearchError> {
        sqlx_core::query::query(
            "INSERT INTO search_account_state (
                user_id, state, index_generation, index_version,
                chunker_version, model_version
             ) VALUES ($1, 'rebuilding', 0, $2, $3, $4)
             ON CONFLICT (user_id) DO UPDATE SET
                state = 'rebuilding', failure_code = NULL, updated_at = now()",
        )
        .bind(owner_id)
        .bind(SEARCH_INDEX_VERSION)
        .bind(SEARCH_CHUNKER_VERSION)
        .bind(&self.model_version)
        .execute(pool)
        .await
        .map_err(|_| SearchError::Storage)?;
        let index = self.index.as_ref().ok_or(SearchError::Unavailable)?;
        sqlx_core::query::query("DELETE FROM search_documents WHERE user_id = $1")
            .bind(owner_id)
            .execute(pool)
            .await
            .map_err(|_| SearchError::Storage)?;

        let rows = sqlx_core::query::query(
            "SELECT source_type, source_id, space_id FROM (
                SELECT 'material'::text AS source_type, material_id AS source_id, space_id
                  FROM materials
                 WHERE owner_user_id = $1 AND deleted_at IS NULL
                UNION ALL
                SELECT 'annotation', annotation.annotation_id, annotation.space_id
                  FROM annotations annotation
                  JOIN materials material ON material.material_id = annotation.material_id
                 WHERE material.owner_user_id = $1 AND annotation.deleted_at IS NULL
                UNION ALL
                SELECT 'ai_artifact', artifact_id, space_id
                  FROM ai_artifacts
                 WHERE user_id = $1 AND status = 'active'
                UNION ALL
                SELECT 'learning_item', item_id, space_id
                  FROM learning_items
                 WHERE owner_user_id = $1 AND status = 'active' AND deleted_at IS NULL
                UNION ALL
                SELECT 'shared_comment', comment.comment_id, thread.community_space_id
                  FROM shared_comments comment
                  JOIN shared_comment_threads thread ON thread.thread_id = comment.thread_id
                  JOIN community_memberships membership
                    ON membership.community_space_id = thread.community_space_id
                   AND membership.user_id = $1
                   AND membership.status = 'active'
                 WHERE comment.hidden_at IS NULL AND comment.deleted_at IS NULL
                   AND thread.hidden_at IS NULL AND thread.deleted_at IS NULL
                   AND (
                       thread.scope = 'material' OR EXISTS (
                           SELECT 1 FROM user_material_claims claim
                            WHERE claim.community_space_id = thread.community_space_id
                              AND claim.shared_material_id = thread.shared_material_id
                              AND claim.user_id = $1 AND claim.match_status = 'matched'
                              AND claim.deleted_at IS NULL
                       )
                   )
                UNION ALL
                SELECT 'shared_chat_message', message.message_id, message.community_space_id
                  FROM shared_chat_messages message
                  JOIN community_memberships membership
                    ON membership.community_space_id = message.community_space_id
                   AND membership.user_id = $1
                   AND membership.status = 'active'
                 WHERE message.hidden_at IS NULL AND message.deleted_at IS NULL
                UNION ALL
                SELECT 'shared_highlight', highlight.highlight_id, highlight.community_space_id
                  FROM shared_highlights highlight
                  JOIN community_memberships membership
                    ON membership.community_space_id = highlight.community_space_id
                   AND membership.user_id = $1
                   AND membership.status = 'active'
                  JOIN user_material_claims claim
                    ON claim.community_space_id = highlight.community_space_id
                   AND claim.shared_material_id = highlight.shared_material_id
                   AND claim.user_id = $1 AND claim.match_status = 'matched'
                   AND claim.deleted_at IS NULL
                 WHERE highlight.deleted_at IS NULL
            ) sources
            ORDER BY source_type, source_id",
        )
        .bind(owner_id)
        .fetch_all(pool)
        .await
        .map_err(|_| SearchError::Storage)?;
        let mut documents = Vec::with_capacity(rows.len());
        for row in rows {
            let source_type: String = row
                .try_get("source_type")
                .map_err(|_| SearchError::Storage)?;
            let source_id: Uuid = row.try_get("source_id").map_err(|_| SearchError::Storage)?;
            let request = IndexRequest {
                request_id: Uuid::nil(),
                job_id: Uuid::nil(),
                owner_id,
                source_type,
                source_id,
                operation: "replace".to_owned(),
            };
            let chunks = extract_source(pool, &request).await?;
            let vectorized = self.vectorize(chunks)?;
            persist_projection(pool, &request, &vectorized, &self.model_version).await?;
            documents.push((
                document_key(owner_id, &request.source_type, source_id),
                vectorized,
            ));
        }
        index
            .rebuild(owner_id, documents)
            .map_err(SearchError::Index)
    }
}

async fn enqueue_rebuild_job(
    pool: &PgPool,
    owner_id: Uuid,
    space_id: Uuid,
) -> Result<Uuid, SearchError> {
    let job_id = Uuid::now_v7();
    let request_id = Uuid::now_v7();
    let mut transaction = pool.begin().await.map_err(|_| SearchError::Storage)?;
    sqlx_core::query::query(
        "INSERT INTO jobs (
            job_id, user_id, space_id, kind, payload_ref, status, stage, max_attempts
         ) VALUES (
            $1, $2, $3, 'search_index', $4, 'queued', 'search.rebuild.queued.v1', 5
         )",
    )
    .bind(job_id)
    .bind(owner_id)
    .bind(space_id)
    .bind(serde_json::json!({"request_id": request_id}))
    .execute(&mut *transaction)
    .await
    .map_err(|_| SearchError::Storage)?;
    sqlx_core::query::query(
        "INSERT INTO search_index_requests (
            request_id, job_id, user_id, space_id, source_type, source_id, operation
         ) VALUES ($1, $2, $3, $4, 'account', $3, 'rebuild')",
    )
    .bind(request_id)
    .bind(job_id)
    .bind(owner_id)
    .bind(space_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| SearchError::Storage)?;
    transaction
        .commit()
        .await
        .map_err(|_| SearchError::Storage)?;
    Ok(job_id)
}

async fn next_request(pool: &PgPool) -> Result<Option<IndexRequest>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT request.request_id, request.job_id, request.user_id,
                request.source_type, request.source_id, request.operation
           FROM search_index_requests request
           JOIN jobs job ON job.job_id = request.job_id
          WHERE request.status = 'queued'
            AND job.status = 'queued'
            AND job.kind = 'search_index'
            AND job.cancellation_requested = false
          ORDER BY request.created_at, request.request_id
          LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    row.map(|row| {
        Ok(IndexRequest {
            request_id: row
                .try_get("request_id")
                .map_err(|_| SearchError::Storage)?,
            job_id: row.try_get("job_id").map_err(|_| SearchError::Storage)?,
            owner_id: row.try_get("user_id").map_err(|_| SearchError::Storage)?,
            source_type: row
                .try_get("source_type")
                .map_err(|_| SearchError::Storage)?,
            source_id: row.try_get("source_id").map_err(|_| SearchError::Storage)?,
            operation: row.try_get("operation").map_err(|_| SearchError::Storage)?,
        })
    })
    .transpose()
}

async fn extract_source(
    pool: &PgPool,
    request: &IndexRequest,
) -> Result<Vec<SearchChunk>, SearchError> {
    match request.source_type.as_str() {
        "material" => extract_material(pool, request.owner_id, request.source_id).await,
        "annotation" => extract_annotation(pool, request.owner_id, request.source_id).await,
        "ai_artifact" => extract_ai_artifact(pool, request.owner_id, request.source_id).await,
        "learning_item" => extract_learning_item(pool, request.owner_id, request.source_id).await,
        "shared_comment" => extract_shared_comment(pool, request.owner_id, request.source_id).await,
        "shared_chat_message" => {
            extract_shared_chat_message(pool, request.owner_id, request.source_id).await
        }
        "shared_highlight" => {
            extract_shared_highlight(pool, request.owner_id, request.source_id).await
        }
        _ => Err(SearchError::Storage),
    }
}

async fn extract_material(
    pool: &PgPool,
    owner_id: Uuid,
    material_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT revision.source_format, package.payload
           FROM materials material
           JOIN document_revisions revision
             ON revision.revision_id = material.active_revision_id
            AND revision.material_id = material.material_id
           JOIN normalized_packages package ON package.revision_id = revision.revision_id
          WHERE material.material_id = $1
            AND material.owner_user_id = $2
            AND material.deleted_at IS NULL
            AND material.library_state <> 'deleted'",
    )
    .bind(material_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let source_format: String = row
        .try_get("source_format")
        .map_err(|_| SearchError::Storage)?;
    let payload: Value = row.try_get("payload").map_err(|_| SearchError::Storage)?;
    if source_format == "pdf" {
        let package: FixedLayoutContentPackage =
            serde_json::from_value(payload).map_err(|_| SearchError::Storage)?;
        Ok(chunker::fixed_layout_material(material_id, &package))
    } else {
        let package: NormalizedContentPackage =
            serde_json::from_value(payload).map_err(|_| SearchError::Storage)?;
        Ok(chunker::reflowable_material(material_id, &package))
    }
}

async fn extract_annotation(
    pool: &PgPool,
    owner_id: Uuid,
    annotation_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT annotation.material_id, annotation.revision_id, annotation.anchor,
                annotation.kind, annotation.annotation_type, annotation.target_kind,
                annotation.status, annotation.title, annotation.related_annotation_id,
                annotation.object_revision, annotation.created_at, annotation.updated_at,
                material.canonical_title, material.title_override,
                COALESCE((
                    SELECT jsonb_agg(tag.tag ORDER BY tag.ordinal)
                      FROM annotation_tags tag
                     WHERE tag.annotation_id = annotation.annotation_id
                ), '[]'::jsonb) AS tags
           FROM annotations annotation
           JOIN materials material ON material.material_id = annotation.material_id
          WHERE annotation.annotation_id = $1
            AND material.owner_user_id = $2
            AND annotation.deleted_at IS NULL
            AND material.deleted_at IS NULL",
    )
    .bind(annotation_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let anchor: Anchor =
        serde_json::from_value(row.try_get("anchor").map_err(|_| SearchError::Storage)?)
            .map_err(|_| SearchError::Storage)?;
    let kind: AnnotationKind =
        serde_json::from_value(row.try_get("kind").map_err(|_| SearchError::Storage)?)
            .map_err(|_| SearchError::Storage)?;
    let annotation_type = parse_annotation_type(
        row.try_get("annotation_type")
            .map_err(|_| SearchError::Storage)?,
    )?;
    let target_kind: String = row
        .try_get("target_kind")
        .map_err(|_| SearchError::Storage)?;
    let target = target_from_kind(&target_kind, &anchor)?;
    let status = match row
        .try_get::<String, _>("status")
        .map_err(|_| SearchError::Storage)?
        .as_str()
    {
        "active" => AnnotationStatus::Active,
        "archived" => AnnotationStatus::Archived,
        _ => return Err(SearchError::Storage),
    };
    let revision = positive_u64(
        row.try_get("object_revision")
            .map_err(|_| SearchError::Storage)?,
    )?;
    let annotation = Annotation {
        id: annotation_id,
        material_id: row
            .try_get("material_id")
            .map_err(|_| SearchError::Storage)?,
        revision_id: row
            .try_get("revision_id")
            .map_err(|_| SearchError::Storage)?,
        anchor,
        annotation_type,
        target,
        kind,
        title: row.try_get("title").map_err(|_| SearchError::Storage)?,
        tags: serde_json::from_value(row.try_get("tags").map_err(|_| SearchError::Storage)?)
            .map_err(|_| SearchError::Storage)?,
        status,
        related_annotation_id: row
            .try_get("related_annotation_id")
            .map_err(|_| SearchError::Storage)?,
        revision,
        created_at: timestamp_ms(
            row.try_get("created_at")
                .map_err(|_| SearchError::Storage)?,
        ),
        updated_at: timestamp_ms(
            row.try_get("updated_at")
                .map_err(|_| SearchError::Storage)?,
        ),
    };
    let transcript: Option<String> = match &annotation.kind {
        AnnotationKind::VoiceNote {
            transcript_artifact_id: Some(transcript_id),
            ..
        } => query_scalar(
            "SELECT transcript_text FROM transcript_artifacts
                  WHERE id = $1 AND owner_id = $2 AND status = 'accepted'",
        )
        .bind(*transcript_id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| SearchError::Storage)?,
        AnnotationKind::Highlight { .. }
        | AnnotationKind::Note { .. }
        | AnnotationKind::VoiceNote {
            transcript_artifact_id: None,
            ..
        } => None,
    };
    let title_override: Option<String> = row
        .try_get("title_override")
        .map_err(|_| SearchError::Storage)?;
    let canonical_title: String = row
        .try_get("canonical_title")
        .map_err(|_| SearchError::Storage)?;
    Ok(chunker::annotation(
        &annotation,
        title_override.as_deref().unwrap_or(&canonical_title),
        transcript.as_deref(),
    ))
}

async fn extract_ai_artifact(
    pool: &PgPool,
    owner_id: Uuid,
    artifact_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT artifact.source_material_id, artifact.source_revision_id,
                artifact.artifact_revision, artifact.payload,
                material.canonical_title, material.title_override
           FROM ai_artifacts artifact
           JOIN materials material ON material.material_id = artifact.source_material_id
          WHERE artifact.artifact_id = $1
            AND artifact.user_id = $2
            AND artifact.status = 'active'
            AND material.deleted_at IS NULL",
    )
    .bind(artifact_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let canonical: String = row
        .try_get("canonical_title")
        .map_err(|_| SearchError::Storage)?;
    let title_override: Option<String> = row
        .try_get("title_override")
        .map_err(|_| SearchError::Storage)?;
    Ok(chunker::ai_artifact(
        artifact_id,
        row.try_get("source_material_id")
            .map_err(|_| SearchError::Storage)?,
        row.try_get("source_revision_id")
            .map_err(|_| SearchError::Storage)?,
        positive_u64(
            row.try_get("artifact_revision")
                .map_err(|_| SearchError::Storage)?,
        )?,
        title_override.as_deref().unwrap_or(&canonical),
        &row.try_get("payload").map_err(|_| SearchError::Storage)?,
    ))
}

async fn extract_learning_item(
    pool: &PgPool,
    owner_id: Uuid,
    item_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT source.material_id, source.document_revision_id,
                item.object_revision, revision.payload,
                material.canonical_title, material.title_override
           FROM learning_items item
           JOIN learning_sources source ON source.source_id = item.source_id
           JOIN learning_item_revisions revision
             ON revision.item_revision_id = item.current_revision_id
           JOIN materials material ON material.material_id = source.material_id
          WHERE item.item_id = $1
            AND item.owner_user_id = $2
            AND item.status = 'active'
            AND item.deleted_at IS NULL
            AND source.deleted_at IS NULL
            AND material.deleted_at IS NULL",
    )
    .bind(item_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let canonical: String = row
        .try_get("canonical_title")
        .map_err(|_| SearchError::Storage)?;
    let title_override: Option<String> = row
        .try_get("title_override")
        .map_err(|_| SearchError::Storage)?;
    Ok(chunker::learning_item(
        item_id,
        row.try_get("material_id")
            .map_err(|_| SearchError::Storage)?,
        row.try_get("document_revision_id")
            .map_err(|_| SearchError::Storage)?,
        positive_u64(
            row.try_get("object_revision")
                .map_err(|_| SearchError::Storage)?,
        )?,
        title_override.as_deref().unwrap_or(&canonical),
        &row.try_get("payload").map_err(|_| SearchError::Storage)?,
    ))
}

async fn extract_shared_comment(
    pool: &PgPool,
    owner_id: Uuid,
    comment_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT comment.body_markdown, comment.object_revision, comment.updated_at,
                thread.community_space_id, thread.shared_material_id, thread.scope,
                identity.canonical_title
           FROM shared_comments comment
           JOIN shared_comment_threads thread ON thread.thread_id = comment.thread_id
           JOIN shared_material_identities identity
             ON identity.shared_material_id = thread.shared_material_id
            AND identity.community_space_id = thread.community_space_id
            AND identity.deleted_at IS NULL
           JOIN community_memberships membership
             ON membership.community_space_id = thread.community_space_id
            AND membership.user_id = $2 AND membership.status = 'active'
          WHERE comment.comment_id = $1
            AND comment.hidden_at IS NULL AND comment.deleted_at IS NULL
            AND thread.hidden_at IS NULL AND thread.deleted_at IS NULL
            AND (
                thread.scope = 'material' OR EXISTS (
                    SELECT 1 FROM user_material_claims claim
                     WHERE claim.community_space_id = thread.community_space_id
                       AND claim.shared_material_id = thread.shared_material_id
                       AND claim.user_id = $2 AND claim.match_status = 'matched'
                       AND claim.deleted_at IS NULL
                )
            )",
    )
    .bind(comment_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let revision = positive_u64(
        row.try_get("object_revision")
            .map_err(|_| SearchError::Storage)?,
    )?;
    Ok(chunker::social_text(
        lumi_core::SearchSourceType::SharedComment,
        comment_id,
        &revision.to_string(),
        &row.try_get::<String, _>("canonical_title")
            .map_err(|_| SearchError::Storage)?,
        &row.try_get::<String, _>("body_markdown")
            .map_err(|_| SearchError::Storage)?,
        row.try_get("community_space_id")
            .map_err(|_| SearchError::Storage)?,
        Some(
            row.try_get("shared_material_id")
                .map_err(|_| SearchError::Storage)?,
        ),
        timestamp_ms(
            row.try_get("updated_at")
                .map_err(|_| SearchError::Storage)?,
        ),
    ))
}

async fn extract_shared_chat_message(
    pool: &PgPool,
    owner_id: Uuid,
    message_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT message.body_markdown, message.object_revision, message.updated_at,
                message.community_space_id, space.name
           FROM shared_chat_messages message
           JOIN community_spaces space
             ON space.community_space_id = message.community_space_id
            AND space.deleted_at IS NULL
           JOIN community_memberships membership
             ON membership.community_space_id = message.community_space_id
            AND membership.user_id = $2 AND membership.status = 'active'
          WHERE message.message_id = $1
            AND message.hidden_at IS NULL AND message.deleted_at IS NULL",
    )
    .bind(message_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let revision = positive_u64(
        row.try_get("object_revision")
            .map_err(|_| SearchError::Storage)?,
    )?;
    Ok(chunker::social_text(
        lumi_core::SearchSourceType::SharedChatMessage,
        message_id,
        &revision.to_string(),
        &row.try_get::<String, _>("name")
            .map_err(|_| SearchError::Storage)?,
        &row.try_get::<String, _>("body_markdown")
            .map_err(|_| SearchError::Storage)?,
        row.try_get("community_space_id")
            .map_err(|_| SearchError::Storage)?,
        None,
        timestamp_ms(
            row.try_get("updated_at")
                .map_err(|_| SearchError::Storage)?,
        ),
    ))
}

async fn extract_shared_highlight(
    pool: &PgPool,
    owner_id: Uuid,
    highlight_id: Uuid,
) -> Result<Vec<SearchChunk>, SearchError> {
    let row = sqlx_core::query::query(
        "SELECT highlight.shared_anchor, highlight.object_revision, highlight.updated_at,
                highlight.community_space_id, highlight.shared_material_id,
                identity.canonical_title
           FROM shared_highlights highlight
           JOIN shared_material_identities identity
             ON identity.shared_material_id = highlight.shared_material_id
            AND identity.community_space_id = highlight.community_space_id
            AND identity.deleted_at IS NULL
           JOIN community_memberships membership
             ON membership.community_space_id = highlight.community_space_id
            AND membership.user_id = $2 AND membership.status = 'active'
           JOIN user_material_claims claim
             ON claim.community_space_id = highlight.community_space_id
            AND claim.shared_material_id = highlight.shared_material_id
            AND claim.user_id = $2 AND claim.match_status = 'matched'
            AND claim.deleted_at IS NULL
          WHERE highlight.highlight_id = $1 AND highlight.deleted_at IS NULL",
    )
    .bind(highlight_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| SearchError::Storage)?;
    let Some(row) = row else {
        return Ok(Vec::new());
    };
    let shared_anchor: lumi_core::SharedAnchor = serde_json::from_value(
        row.try_get("shared_anchor")
            .map_err(|_| SearchError::Storage)?,
    )
    .map_err(|_| SearchError::Storage)?;
    let revision = positive_u64(
        row.try_get("object_revision")
            .map_err(|_| SearchError::Storage)?,
    )?;
    Ok(chunker::social_text(
        lumi_core::SearchSourceType::SharedHighlight,
        highlight_id,
        &revision.to_string(),
        &row.try_get::<String, _>("canonical_title")
            .map_err(|_| SearchError::Storage)?,
        &shared_anchor.quote,
        row.try_get("community_space_id")
            .map_err(|_| SearchError::Storage)?,
        Some(
            row.try_get("shared_material_id")
                .map_err(|_| SearchError::Storage)?,
        ),
        timestamp_ms(
            row.try_get("updated_at")
                .map_err(|_| SearchError::Storage)?,
        ),
    ))
}

async fn persist_projection(
    pool: &PgPool,
    request: &IndexRequest,
    chunks: &[(SearchChunk, Vec<f32>)],
    model_version: &str,
) -> Result<(), SearchError> {
    let mut transaction = pool.begin().await.map_err(|_| SearchError::Storage)?;
    if request.operation == "delete" {
        sqlx_core::query::query(
            "DELETE FROM search_documents
              WHERE user_id = $1 AND source_type = $2 AND source_id = $3",
        )
        .bind(request.owner_id)
        .bind(&request.source_type)
        .bind(request.source_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SearchError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(|_| SearchError::Storage)?;
        return Ok(());
    }
    let document_id: Uuid = query_scalar(
        "SELECT document_id FROM search_documents
          WHERE user_id = $1 AND source_type = $2 AND source_id = $3",
    )
    .bind(request.owner_id)
    .bind(&request.source_type)
    .bind(request.source_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| SearchError::Storage)?
    .unwrap_or_else(Uuid::now_v7);
    if chunks.is_empty() && is_social_source(&request.source_type) {
        sqlx_core::query::query(
            "DELETE FROM search_documents
              WHERE user_id = $1 AND source_type = $2 AND source_id = $3",
        )
        .bind(request.owner_id)
        .bind(&request.source_type)
        .bind(request.source_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SearchError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(|_| SearchError::Storage)?;
        return Ok(());
    }
    if chunks.is_empty() {
        sqlx_core::query::query(
            "INSERT INTO search_documents (
                document_id, user_id, space_id, source_type, source_id,
                source_version, content_hash, chunker_version, model_version
             ) SELECT $1, $2, space_id, $3, $4, 'no-text.v1', $5, $6, $7
                 FROM sync_spaces
                WHERE owner_user_id = $2 AND kind = 'personal' AND deleted_at IS NULL
             ON CONFLICT (user_id, source_type, source_id) DO UPDATE SET
                material_id = NULL,
                revision_id = NULL,
                source_version = EXCLUDED.source_version,
                content_hash = EXCLUDED.content_hash,
                chunker_version = EXCLUDED.chunker_version,
                model_version = EXCLUDED.model_version,
                indexed_at = now()",
        )
        .bind(document_id)
        .bind(request.owner_id)
        .bind(&request.source_type)
        .bind(request.source_id)
        .bind(content_hash(b""))
        .bind(SEARCH_CHUNKER_VERSION)
        .bind(model_version)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SearchError::Storage)?;
        sqlx_core::query::query("DELETE FROM search_chunks WHERE document_id = $1")
            .bind(document_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| SearchError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(|_| SearchError::Storage)?;
        return Ok(());
    }
    let first = &chunks[0].0;
    let aggregate_hash = content_hash(
        chunks
            .iter()
            .flat_map(|(chunk, _)| chunk.text_hash.as_bytes().iter().copied())
            .collect::<Vec<_>>()
            .as_slice(),
    );
    sqlx_core::query::query(
        "INSERT INTO search_documents (
            document_id, user_id, space_id, source_type, source_id,
            material_id, revision_id, source_version, content_hash,
            chunker_version, model_version
         ) SELECT $1, $2, space_id, $3, $4, $5, $6, $7, $8, $9, $10
             FROM sync_spaces
            WHERE owner_user_id = $2 AND kind = 'personal' AND deleted_at IS NULL
         ON CONFLICT (user_id, source_type, source_id) DO UPDATE SET
            material_id = EXCLUDED.material_id,
            revision_id = EXCLUDED.revision_id,
            source_version = EXCLUDED.source_version,
            content_hash = EXCLUDED.content_hash,
            chunker_version = EXCLUDED.chunker_version,
            model_version = EXCLUDED.model_version,
            indexed_at = now()",
    )
    .bind(document_id)
    .bind(request.owner_id)
    .bind(&request.source_type)
    .bind(request.source_id)
    .bind(first.material_id)
    .bind(first.revision_id)
    .bind(&first.source_version)
    .bind(aggregate_hash)
    .bind(SEARCH_CHUNKER_VERSION)
    .bind(model_version)
    .execute(&mut *transaction)
    .await
    .map_err(|_| SearchError::Storage)?;
    sqlx_core::query::query("DELETE FROM search_chunks WHERE document_id = $1")
        .bind(document_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SearchError::Storage)?;
    for (chunk, vector) in chunks {
        let payload = serde_json::to_value(chunk).map_err(|_| SearchError::Storage)?;
        sqlx_core::query::query(
            "INSERT INTO search_chunks (
                chunk_id, document_id, user_id, source_type, source_id,
                material_id, revision_id, community_space_id, shared_material_id,
                field, text_hash, payload, fasttext_vector
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(&chunk.id)
        .bind(document_id)
        .bind(request.owner_id)
        .bind(chunk.source_type.as_str())
        .bind(chunk.source_id)
        .bind(chunk.material_id)
        .bind(chunk.revision_id)
        .bind(chunk.community_space_id)
        .bind(chunk.shared_material_id)
        .bind(chunk.field.as_str())
        .bind(&chunk.text_hash)
        .bind(payload)
        .bind(vector)
        .execute(&mut *transaction)
        .await
        .map_err(|_| SearchError::Storage)?;
    }
    transaction.commit().await.map_err(|_| SearchError::Storage)
}

fn is_social_source(value: &str) -> bool {
    matches!(
        value,
        "shared_comment" | "shared_chat_message" | "shared_highlight"
    )
}

fn parse_annotation_type(value: String) -> Result<AnnotationType, SearchError> {
    match value.as_str() {
        "highlight" => Ok(AnnotationType::Highlight),
        "note" => Ok(AnnotationType::Note),
        "margin_note" => Ok(AnnotationType::MarginNote),
        "voice_note" => Ok(AnnotationType::VoiceNote),
        _ => Err(SearchError::Storage),
    }
}

fn target_from_kind(kind: &str, anchor: &Anchor) -> Result<AnnotationTarget, SearchError> {
    match kind {
        "text_range" => Ok(AnnotationTarget::TextRange),
        "section" => Ok(AnnotationTarget::Section {
            path: anchor.node_path.clone(),
        }),
        "document" => Ok(AnnotationTarget::Document),
        "page_area" => Ok(AnnotationTarget::from_legacy_anchor(anchor)),
        "block" => Ok(AnnotationTarget::Block {
            path: anchor.node_path.clone(),
        }),
        _ => Err(SearchError::Storage),
    }
}

fn positive_u64(value: i64) -> Result<u64, SearchError> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(SearchError::Storage)
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use sqlx_postgres::PgPoolOptions;

    use super::*;

    #[tokio::test]
    async fn postgres_search_request_and_common_job_are_atomic(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(database_url) = std::env::var("LUMI_TEST_DATABASE_URL").ok() else {
            return Ok(());
        };
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        let owner_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        let source_id = Uuid::now_v7();
        let mut transaction = pool.begin().await?;
        sqlx_core::query::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
            .bind(owner_id)
            .execute(&mut *transaction)
            .await?;
        sqlx_core::query::query(
            "INSERT INTO sync_spaces (space_id, owner_user_id, kind)
             VALUES ($1, $2, 'personal')",
        )
        .bind(space_id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await?;
        sqlx_core::query::query(
            "INSERT INTO sync_space_members (space_id, user_id, role)
             VALUES ($1, $2, 'owner')",
        )
        .bind(space_id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await?;
        sqlx_core::query::query(
            "SELECT lumi_enqueue_search_index($1, $2, 'annotation', $3, 'replace')",
        )
        .bind(owner_id)
        .bind(space_id)
        .bind(source_id)
        .execute(&mut *transaction)
        .await?;
        let row = sqlx_core::query::query(
            "SELECT request.source_id, job.kind, job.status
               FROM search_index_requests request
               JOIN jobs job ON job.job_id = request.job_id
              WHERE request.user_id = $1",
        )
        .bind(owner_id)
        .fetch_one(&mut *transaction)
        .await?;

        assert_eq!(row.try_get::<Uuid, _>("source_id")?, source_id);
        assert_eq!(row.try_get::<String, _>("kind")?, "search_index");
        assert_eq!(row.try_get::<String, _>("status")?, "queued");
        transaction.rollback().await?;
        Ok(())
    }
}
