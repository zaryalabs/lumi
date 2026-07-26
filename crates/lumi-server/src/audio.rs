//! Owner-scoped generic audio uploads and learning attachment lifecycle.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use lumi_core::{
    AcceptTranscriptCommand, AudioAttachment, AudioRetentionPolicy, AudioUpload, AudioUploadStatus,
    CreateAudioUploadCommand, CreateLearningAttachmentCommand, TranscriptArtifact,
    TranscriptStatus, UserId, MAX_LEARNING_AUDIO_BYTES,
};
use serde::Deserialize;
use sqlx_core::{query::query, row::Row};
use sqlx_postgres::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{
    account::AuthenticatedSession,
    blob::{BlobStore, BlobStoreError, LocalBlobStore},
    AppError, AppState,
};

#[derive(Clone)]
pub(crate) struct AudioRuntime {
    pool: PgPool,
    blobs: Arc<dyn BlobStore>,
}

impl AudioRuntime {
    pub(crate) fn postgres(pool: PgPool, blob_root: std::path::PathBuf) -> Self {
        Self {
            pool,
            blobs: Arc::new(LocalBlobStore::new(blob_root)),
        }
    }
}

#[derive(Deserialize)]
struct TranscribeCommand {
    #[serde(default)]
    language: Option<String>,
}

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/blobs/uploads", post(create_upload))
        .route(
            "/blobs/uploads/{upload_id}",
            put(put_upload).layer(DefaultBodyLimit::max(MAX_LEARNING_AUDIO_BYTES as usize)),
        )
        .route("/blobs/uploads/{upload_id}/complete", post(complete_upload))
        .route("/learning/attachments", post(create_attachment))
        .route("/learning/attachments/{attachment_id}", get(get_attachment))
        .route(
            "/learning/attachments/{attachment_id}/audio",
            get(download_audio).delete(delete_audio),
        )
        .route(
            "/learning/attachments/{attachment_id}/transcribe",
            post(transcribe),
        )
        .route(
            "/learning/attachments/{attachment_id}/transcript",
            get(get_transcript),
        )
        .route(
            "/learning/attachments/{attachment_id}/transcript/accept",
            post(accept_transcript),
        )
}

async fn create_upload(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(command): Json<CreateAudioUploadCommand>,
) -> Result<Json<AudioUpload>, AppError> {
    command
        .validate()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let runtime = state.audio_runtime()?;
    let id = Uuid::now_v7();
    let row = query(
        "INSERT INTO audio_uploads (id, owner_id, media_type, byte_length, checksum_sha256, status)
         VALUES ($1, $2, $3, $4, $5, 'pending')
         RETURNING created_at",
    )
    .bind(id)
    .bind(session.user_id)
    .bind(&command.media_type)
    .bind(
        i64::try_from(command.byte_length)
            .map_err(|_| AppError::BadRequest("audio is too large".to_owned()))?,
    )
    .bind(&command.checksum_sha256)
    .fetch_one(&runtime.pool)
    .await
    .map_err(log_db)?;
    Ok(Json(AudioUpload {
        id,
        media_type: command.media_type,
        byte_length: command.byte_length,
        checksum_sha256: command.checksum_sha256,
        status: AudioUploadStatus::Pending,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_db)?),
    }))
}

async fn put_upload(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(upload_id): Path<Uuid>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let runtime = state.audio_runtime()?;
    let row = query(
        "SELECT byte_length, checksum_sha256, status FROM audio_uploads
         WHERE id = $1 AND owner_id = $2",
    )
    .bind(upload_id)
    .bind(session.user_id)
    .fetch_optional(&runtime.pool)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("audio_upload"))?;
    let expected_length: i64 = row.try_get("byte_length").map_err(log_db)?;
    let checksum: String = row.try_get("checksum_sha256").map_err(log_db)?;
    let status: String = row.try_get("status").map_err(log_db)?;
    if status != "pending" || usize::try_from(expected_length).ok() != Some(body.len()) {
        return Err(AppError::BadRequest(
            "audio upload length or state is invalid".to_owned(),
        ));
    }
    let stored = runtime
        .blobs
        .put(&checksum, &body)
        .await
        .map_err(map_blob)?;
    query(
        "UPDATE audio_uploads SET storage_key = $3
         WHERE id = $1 AND owner_id = $2 AND status = 'pending'",
    )
    .bind(upload_id)
    .bind(session.user_id)
    .bind(stored.storage_key)
    .execute(&runtime.pool)
    .await
    .map_err(log_db)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn complete_upload(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(upload_id): Path<Uuid>,
) -> Result<Json<AudioUpload>, AppError> {
    let runtime = state.audio_runtime()?;
    let row = query(
        "UPDATE audio_uploads SET status = 'completed', completed_at = now()
         WHERE id = $1 AND owner_id = $2 AND status = 'pending' AND storage_key IS NOT NULL
         RETURNING media_type, byte_length, checksum_sha256, created_at",
    )
    .bind(upload_id)
    .bind(session.user_id)
    .fetch_optional(&runtime.pool)
    .await
    .map_err(log_db)?
    .ok_or_else(|| AppError::BadRequest("audio bytes are not uploaded".to_owned()))?;
    Ok(Json(AudioUpload {
        id: upload_id,
        media_type: row.try_get("media_type").map_err(log_db)?,
        byte_length: u64::try_from(row.try_get::<i64, _>("byte_length").map_err(log_db)?)
            .map_err(|_| AppError::Unavailable("audio metadata"))?,
        checksum_sha256: row.try_get("checksum_sha256").map_err(log_db)?,
        status: AudioUploadStatus::Completed,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_db)?),
    }))
}

async fn create_attachment(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(command): Json<CreateLearningAttachmentCommand>,
) -> Result<Json<AudioAttachment>, AppError> {
    if command.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "idempotency key is required".to_owned(),
        ));
    }
    let runtime = state.audio_runtime()?;
    let mut tx = runtime.pool.begin().await.map_err(log_db)?;
    let owns_session: bool = sqlx_core::query_scalar::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM learning_sessions WHERE id = $1 AND user_id = $2)",
    )
    .bind(command.session_id)
    .bind(session.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(log_db)?;
    if !owns_session {
        return Err(AppError::NotFound("learning_session"));
    }
    let id = Uuid::now_v7();
    let retention = retention_name(command.retention);
    let row = query(
        "INSERT INTO audio_attachments
         (id, owner_id, upload_id, media_type, byte_length, checksum_sha256, retention)
         SELECT $1, owner_id, id, media_type, byte_length, checksum_sha256, $4
         FROM audio_uploads WHERE id = $2 AND owner_id = $3 AND status = 'completed'
         ON CONFLICT (owner_id, upload_id) DO UPDATE SET upload_id = EXCLUDED.upload_id
         RETURNING id, media_type, byte_length, checksum_sha256, retention, audio_deleted_at, created_at",
    )
    .bind(id)
    .bind(command.upload_id)
    .bind(session.user_id)
    .bind(retention)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("audio_upload"))?;
    let attachment_id: Uuid = row.try_get("id").map_err(log_db)?;
    query(
        "INSERT INTO learning_attachment_refs
         (attachment_id, owner_id, session_id, item_id, idempotency_key)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_id, idempotency_key) DO NOTHING",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .bind(command.session_id)
    .bind(command.item_id)
    .bind(command.idempotency_key)
    .execute(&mut *tx)
    .await
    .map_err(log_db)?;
    tx.commit().await.map_err(log_db)?;
    row_to_attachment(&row).map(Json)
}

async fn get_attachment(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
) -> Result<Json<AudioAttachment>, AppError> {
    let row = attachment_row(state.audio_runtime()?, session.user_id, attachment_id).await?;
    row_to_attachment(&row).map(Json)
}

async fn download_audio(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let runtime = state.audio_runtime()?;
    let row = query(
        "SELECT a.media_type, a.checksum_sha256 FROM audio_attachments a
         WHERE a.id = $1 AND a.owner_id = $2 AND a.audio_deleted_at IS NULL",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .fetch_optional(&runtime.pool)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("audio_attachment"))?;
    let media_type: String = row.try_get("media_type").map_err(log_db)?;
    let checksum: String = row.try_get("checksum_sha256").map_err(log_db)?;
    let bytes = runtime.blobs.get(&checksum).await.map_err(map_blob)?;
    Ok(([(header::CONTENT_TYPE, media_type)], bytes))
}

async fn delete_audio(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let result = query(
        "UPDATE audio_attachments SET audio_deleted_at = COALESCE(audio_deleted_at, now())
         WHERE id = $1 AND owner_id = $2",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .execute(&state.audio_runtime()?.pool)
    .await
    .map_err(log_db)?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("audio_attachment"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn transcribe(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
    Json(command): Json<TranscribeCommand>,
) -> Result<Json<TranscriptArtifact>, AppError> {
    let runtime = state.audio_runtime()?;
    attachment_row(runtime, session.user_id, attachment_id).await?;
    let id = Uuid::now_v7();
    let row = query(
        "INSERT INTO transcript_artifacts
         (id, owner_id, attachment_id, revision, status, language)
         VALUES ($1, $2, $3,
           COALESCE((SELECT max(revision) + 1 FROM transcript_artifacts WHERE attachment_id = $3), 1),
           'pending', $4)
         RETURNING revision, created_at",
    )
    .bind(id)
    .bind(session.user_id)
    .bind(attachment_id)
    .bind(command.language.as_deref())
    .fetch_one(&runtime.pool)
    .await
    .map_err(log_db)?;
    Ok(Json(TranscriptArtifact {
        id,
        attachment_id,
        revision: u32::try_from(row.try_get::<i32, _>("revision").map_err(log_db)?)
            .map_err(|_| AppError::Unavailable("transcript revision"))?,
        status: TranscriptStatus::Pending,
        text: String::new(),
        provider: None,
        model: None,
        language: command.language,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_db)?),
        accepted_at: None,
    }))
}

async fn get_transcript(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
) -> Result<Json<TranscriptArtifact>, AppError> {
    let row = query(
        "SELECT id, attachment_id, revision, status, transcript_text, provider, model,
                language, created_at, accepted_at
         FROM transcript_artifacts WHERE attachment_id = $1 AND owner_id = $2
         ORDER BY revision DESC LIMIT 1",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .fetch_optional(&state.audio_runtime()?.pool)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("transcript"))?;
    row_to_transcript(&row).map(Json)
}

async fn accept_transcript(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
    Json(command): Json<AcceptTranscriptCommand>,
) -> Result<Json<TranscriptArtifact>, AppError> {
    if command.text.trim().is_empty() || command.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "reviewed transcript and idempotency key are required".to_owned(),
        ));
    }
    let runtime = state.audio_runtime()?;
    let row = query(
        "INSERT INTO transcript_artifacts
         (id, owner_id, attachment_id, revision, status, transcript_text, provider, model, accepted_at)
         SELECT $1, $2, $3, COALESCE(max(revision) + 1, 1), 'accepted', $4, 'user', 'edited', now()
         FROM transcript_artifacts WHERE attachment_id = $3 AND owner_id = $2
         RETURNING id, attachment_id, revision, status, transcript_text, provider, model,
                   language, created_at, accepted_at",
    )
    .bind(Uuid::now_v7())
    .bind(session.user_id)
    .bind(attachment_id)
    .bind(command.text.trim())
    .fetch_one(&runtime.pool)
    .await
    .map_err(log_db)?;
    query(
        "UPDATE audio_attachments SET audio_deleted_at = now()
         WHERE id = $1 AND owner_id = $2 AND retention = 'delete_after_transcript'",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .execute(&runtime.pool)
    .await
    .map_err(log_db)?;
    row_to_transcript(&row).map(Json)
}

async fn attachment_row(
    runtime: &AudioRuntime,
    owner_id: UserId,
    attachment_id: Uuid,
) -> Result<sqlx_postgres::PgRow, AppError> {
    query(
        "SELECT id, media_type, byte_length, checksum_sha256, retention, audio_deleted_at, created_at
         FROM audio_attachments WHERE id = $1 AND owner_id = $2",
    )
    .bind(attachment_id)
    .bind(owner_id)
    .fetch_optional(&runtime.pool)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("audio_attachment"))
}

fn row_to_attachment(row: &sqlx_postgres::PgRow) -> Result<AudioAttachment, AppError> {
    Ok(AudioAttachment {
        id: row.try_get("id").map_err(log_db)?,
        media_type: row.try_get("media_type").map_err(log_db)?,
        byte_length: u64::try_from(row.try_get::<i64, _>("byte_length").map_err(log_db)?)
            .map_err(|_| AppError::Unavailable("audio metadata"))?,
        checksum_sha256: row.try_get("checksum_sha256").map_err(log_db)?,
        retention: match row
            .try_get::<String, _>("retention")
            .map_err(log_db)?
            .as_str()
        {
            "delete_after_transcript" => AudioRetentionPolicy::DeleteAfterTranscript,
            _ => AudioRetentionPolicy::KeepUntilDeleted,
        },
        audio_deleted_at: row
            .try_get::<Option<OffsetDateTime>, _>("audio_deleted_at")
            .map_err(log_db)?
            .map(time_to_ms),
        created_at: time_to_ms(row.try_get("created_at").map_err(log_db)?),
    })
}

fn row_to_transcript(row: &sqlx_postgres::PgRow) -> Result<TranscriptArtifact, AppError> {
    let status: String = row.try_get("status").map_err(log_db)?;
    Ok(TranscriptArtifact {
        id: row.try_get("id").map_err(log_db)?,
        attachment_id: row.try_get("attachment_id").map_err(log_db)?,
        revision: u32::try_from(row.try_get::<i32, _>("revision").map_err(log_db)?)
            .map_err(|_| AppError::Unavailable("transcript revision"))?,
        status: match status.as_str() {
            "processing" => TranscriptStatus::Processing,
            "needs_review" => TranscriptStatus::NeedsReview,
            "accepted" => TranscriptStatus::Accepted,
            "failed" => TranscriptStatus::Failed,
            "cancelled" => TranscriptStatus::Cancelled,
            _ => TranscriptStatus::Pending,
        },
        text: row.try_get("transcript_text").map_err(log_db)?,
        provider: row.try_get("provider").map_err(log_db)?,
        model: row.try_get("model").map_err(log_db)?,
        language: row.try_get("language").map_err(log_db)?,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_db)?),
        accepted_at: row
            .try_get::<Option<OffsetDateTime>, _>("accepted_at")
            .map_err(log_db)?
            .map(time_to_ms),
    })
}

fn retention_name(value: AudioRetentionPolicy) -> &'static str {
    match value {
        AudioRetentionPolicy::DeleteAfterTranscript => "delete_after_transcript",
        AudioRetentionPolicy::KeepUntilDeleted => "keep_until_deleted",
    }
}

fn time_to_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or_default()
}

fn map_blob(error: BlobStoreError) -> AppError {
    match error {
        BlobStoreError::NotFound => AppError::NotFound("audio_blob"),
        BlobStoreError::InvalidHash | BlobStoreError::HashMismatch => {
            AppError::BadRequest("audio checksum mismatch".to_owned())
        }
        BlobStoreError::Unavailable => AppError::Unavailable("audio blob storage"),
    }
}

fn log_db(error: sqlx_core::error::Error) -> AppError {
    tracing::error!(%error, "audio repository operation failed");
    AppError::Unavailable("audio repository")
}
