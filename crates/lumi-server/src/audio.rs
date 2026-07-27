//! Owner-scoped generic audio uploads and learning attachment lifecycle.

use std::sync::Arc;
use std::time::Instant;

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Extension, Path, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use lumi_core::{
    AcceptTranscriptCommand, AiCredentialState, AiProviderDescriptor, AudioAttachment,
    AudioRetentionPolicy, AudioUpload, AudioUploadStatus, CreateAudioAttachmentCommand,
    CreateAudioUploadCommand, CreateLearningAttachmentCommand, ProviderCredentialState,
    PutProviderCredentialRequest, TranscribeAudioCommand, TranscriptArtifact, TranscriptStatus,
    UserId, MAX_LEARNING_AUDIO_BYTES,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx_core::{query::query, row::Row};
use sqlx_postgres::PgPool;
use time::OffsetDateTime;
use url::Url;
use uuid::Uuid;

use crate::{
    account::AuthenticatedSession,
    blob::{BlobStore, BlobStoreError, LocalBlobStore},
    secrets::{SecretContext, SecretStoreError, SecretValue},
    AppError, AppState,
};

const TRANSCRIPTION_PROVIDER: &str = "openai";
const TRANSCRIPTION_MODEL: &str = "whisper-1";
const MAX_TRANSCRIPT_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_VOICE_NOTE_DURATION_MS: u64 = 10 * 60 * 1_000;

#[derive(Deserialize)]
struct OpenAiTranscriptionResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Clone, Copy)]
enum TranscriptionProviderError {
    MissingCredential,
    Authentication,
    RateLimited,
    QuotaExhausted,
    InvalidResponse,
    Timeout,
    Unavailable,
}

impl TranscriptionProviderError {
    const fn code(self) -> &'static str {
        match self {
            Self::MissingCredential => "missing_credential",
            Self::Authentication => "authentication",
            Self::RateLimited => "rate_limited",
            Self::QuotaExhausted => "quota_exhausted",
            Self::InvalidResponse => "invalid_response",
            Self::Timeout => "timeout",
            Self::Unavailable => "unavailable",
        }
    }
}

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

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/blobs/uploads", post(create_upload))
        .route(
            "/blobs/uploads/{upload_id}",
            put(put_upload).layer(DefaultBodyLimit::max(MAX_LEARNING_AUDIO_BYTES as usize)),
        )
        .route("/blobs/uploads/{upload_id}/complete", post(complete_upload))
        .route("/audio/attachments", post(create_generic_attachment))
        .route("/audio/attachments/{attachment_id}", get(get_attachment))
        .route(
            "/audio/attachments/{attachment_id}/audio",
            get(download_audio).delete(delete_audio),
        )
        .route(
            "/audio/attachments/{attachment_id}/transcript",
            get(get_transcript),
        )
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
        .route("/providers/openai", get(get_transcription_provider))
        .route(
            "/providers/openai/credential",
            put(put_transcription_credential).delete(delete_transcription_credential),
        )
}

async fn get_transcription_provider(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<AiProviderDescriptor>, AppError> {
    let runtime = state.ai_runtime()?;
    let row = query(
        "SELECT credential.state, envelope.fingerprint
           FROM ai_provider_credentials credential
           JOIN secret_envelopes envelope
             ON envelope.secret_id = credential.secret_id
            AND envelope.owner_user_id = credential.user_id
            AND envelope.purpose = 'provider:openai'
          WHERE credential.user_id = $1
            AND credential.provider_kind = 'openai'
            AND credential.revoked_at IS NULL",
    )
    .bind(session.user_id)
    .fetch_optional(runtime.pool())
    .await
    .map_err(log_db)?;
    let (credential_state, credential_fingerprint) = if let Some(row) = row {
        let state: String = row.try_get("state").map_err(log_db)?;
        let fingerprint: Vec<u8> = row.try_get("fingerprint").map_err(log_db)?;
        (
            parse_credential_state(&state),
            Some(fingerprint_prefix(&fingerprint)),
        )
    } else {
        (AiCredentialState::Missing, None)
    };
    Ok(Json(AiProviderDescriptor {
        provider_kind: TRANSCRIPTION_PROVIDER.to_owned(),
        display_name: "OpenAI Whisper".to_owned(),
        capabilities: vec!["audio_transcription".to_owned()],
        allowed_models: vec![TRANSCRIPTION_MODEL.to_owned()],
        credential_state,
        credential_fingerprint,
    }))
}

async fn put_transcription_credential(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<PutProviderCredentialRequest>,
) -> Result<Json<ProviderCredentialState>, AppError> {
    if request.credential.trim().is_empty()
        || request.credential.len() > 4_096
        || request.validation_model != TRANSCRIPTION_MODEL
        || request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > 256
    {
        return Err(AppError::BadRequest(
            "OpenAI credential, whisper-1 model and idempotency key are required".to_owned(),
        ));
    }
    let runtime = state.ai_runtime()?;
    if let Some(row) = query(
        "SELECT credential.state, credential.last_validated_at, envelope.fingerprint
           FROM ai_provider_credentials credential
           JOIN secret_envelopes envelope ON envelope.secret_id = credential.secret_id
          WHERE credential.user_id = $1
            AND credential.provider_kind = 'openai'
            AND credential.write_idempotency_key = $2",
    )
    .bind(session.user_id)
    .bind(request.idempotency_key.trim())
    .fetch_optional(runtime.pool())
    .await
    .map_err(log_db)?
    {
        let state: String = row.try_get("state").map_err(log_db)?;
        let fingerprint: Vec<u8> = row.try_get("fingerprint").map_err(log_db)?;
        return Ok(Json(ProviderCredentialState {
            provider_kind: TRANSCRIPTION_PROVIDER.to_owned(),
            credential_state: parse_credential_state(&state),
            credential_fingerprint: Some(fingerprint_prefix(&fingerprint)),
            last_validated_at: row
                .try_get::<Option<OffsetDateTime>, _>("last_validated_at")
                .map_err(log_db)?
                .map(time_to_ms),
        }));
    }
    let context =
        SecretContext::new(session.user_id, "provider:openai").map_err(map_secret_error)?;
    let replacement = SecretValue::new(request.credential.as_bytes());
    let mut tx = runtime.pool().begin().await.map_err(log_db)?;
    let prior_secret: Option<Uuid> = sqlx_core::query_scalar::query_scalar(
        "SELECT secret_id FROM ai_provider_credentials
          WHERE user_id = $1 AND provider_kind = 'openai' AND revoked_at IS NULL
          FOR UPDATE",
    )
    .bind(session.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_db)?;
    if let Some(secret_id) = prior_secret {
        query(
            "DELETE FROM ai_provider_credentials
              WHERE user_id = $1 AND provider_kind = 'openai' AND secret_id = $2",
        )
        .bind(session.user_id)
        .bind(secret_id)
        .execute(&mut *tx)
        .await
        .map_err(log_db)?;
        runtime
            .secrets()
            .delete_in_transaction(&mut tx, &context, secret_id)
            .await
            .map_err(map_secret_error)?;
    }
    let stored = runtime
        .secrets()
        .store_in_transaction(&mut tx, &context, &replacement)
        .await
        .map_err(map_secret_error)?;
    query(
        "INSERT INTO ai_provider_credentials
         (credential_id, user_id, provider_kind, secret_id, state,
          validation_code, write_idempotency_key)
         VALUES ($1, $2, 'openai', $3, 'unvalidated', 'not_tested', $4)",
    )
    .bind(Uuid::now_v7())
    .bind(session.user_id)
    .bind(stored.secret_id)
    .bind(request.idempotency_key.trim())
    .execute(&mut *tx)
    .await
    .map_err(log_db)?;
    tx.commit().await.map_err(log_db)?;
    Ok(Json(ProviderCredentialState {
        provider_kind: TRANSCRIPTION_PROVIDER.to_owned(),
        credential_state: AiCredentialState::Unvalidated,
        credential_fingerprint: Some(stored.fingerprint),
        last_validated_at: None,
    }))
}

async fn delete_transcription_credential(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<StatusCode, AppError> {
    let runtime = state.ai_runtime()?;
    let context =
        SecretContext::new(session.user_id, "provider:openai").map_err(map_secret_error)?;
    let mut tx = runtime.pool().begin().await.map_err(log_db)?;
    let secret_id: Option<Uuid> = sqlx_core::query_scalar::query_scalar(
        "DELETE FROM ai_provider_credentials
          WHERE user_id = $1 AND provider_kind = 'openai' AND revoked_at IS NULL
          RETURNING secret_id",
    )
    .bind(session.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_db)?;
    if let Some(secret_id) = secret_id {
        runtime
            .secrets()
            .delete_in_transaction(&mut tx, &context, secret_id)
            .await
            .map_err(map_secret_error)?;
    }
    tx.commit().await.map_err(log_db)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_upload(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(command): Json<CreateAudioUploadCommand>,
) -> Result<Json<AudioUpload>, AppError> {
    let started = Instant::now();
    command
        .validate()
        .map_err(|error| AppError::BadRequest(error.to_string()))?;
    let runtime = state.audio_runtime()?;
    if let Err(error) = cleanup_expired_audio(runtime).await {
        tracing::warn!(
            error = ?error,
            "bounded audio orphan cleanup was deferred"
        );
    }
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
    let upload = AudioUpload {
        id,
        media_type: command.media_type,
        byte_length: command.byte_length,
        checksum_sha256: command.checksum_sha256,
        status: AudioUploadStatus::Pending,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_db)?),
    };
    tracing::info!(
        event = "learning.audio_upload_reserved",
        owner_id = %session.user_id,
        upload_id = %upload.id,
        byte_length = upload.byte_length,
        latency_ms = started.elapsed().as_millis(),
        "bounded audio upload reserved"
    );
    Ok(Json(upload))
}

async fn cleanup_expired_audio(runtime: &AudioRuntime) -> Result<(), AppError> {
    let rows = query(
        "WITH expired AS (
             SELECT upload.id
               FROM audio_uploads upload
              WHERE upload.created_at < now() - interval '24 hours'
                AND NOT EXISTS (
                    SELECT 1 FROM audio_attachments attachment
                     WHERE attachment.upload_id = upload.id
                )
              ORDER BY upload.created_at
              LIMIT 32
         )
         DELETE FROM audio_uploads upload
          USING expired
          WHERE upload.id = expired.id
         RETURNING upload.checksum_sha256",
    )
    .fetch_all(&runtime.pool)
    .await
    .map_err(log_db)?;
    let mut checksums = rows
        .into_iter()
        .map(|row| row.try_get::<String, _>("checksum_sha256").map_err(log_db))
        .collect::<Result<Vec<_>, _>>()?;
    let released = query(
        "SELECT DISTINCT attachment.checksum_sha256
           FROM audio_attachments attachment
          WHERE attachment.audio_deleted_at < now() - interval '24 hours'
          ORDER BY attachment.checksum_sha256
          LIMIT 32",
    )
    .fetch_all(&runtime.pool)
    .await
    .map_err(log_db)?;
    for row in released {
        checksums.push(row.try_get("checksum_sha256").map_err(log_db)?);
    }
    checksums.sort();
    checksums.dedup();
    for checksum in checksums {
        let still_needed: bool = sqlx_core::query_scalar::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM audio_uploads
                 WHERE checksum_sha256 = $1
                   AND storage_key IS NOT NULL
                   AND created_at >= now() - interval '24 hours'
                UNION ALL
                SELECT 1 FROM audio_attachments
                 WHERE checksum_sha256 = $1 AND audio_deleted_at IS NULL
                UNION ALL
                SELECT 1 FROM blobs WHERE content_hash = $1
            )",
        )
        .bind(&checksum)
        .fetch_one(&runtime.pool)
        .await
        .map_err(log_db)?;
        if !still_needed {
            runtime.blobs.delete(&checksum).await.map_err(map_blob)?;
        }
    }
    Ok(())
}

async fn put_upload(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(upload_id): Path<Uuid>,
    body: Bytes,
) -> Result<StatusCode, AppError> {
    let runtime = state.audio_runtime()?;
    let row = query(
        "SELECT media_type, byte_length, checksum_sha256, status FROM audio_uploads
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
    let media_type: String = row.try_get("media_type").map_err(log_db)?;
    let status: String = row.try_get("status").map_err(log_db)?;
    if status != "pending" || usize::try_from(expected_length).ok() != Some(body.len()) {
        return Err(AppError::BadRequest(
            "audio upload length or state is invalid".to_owned(),
        ));
    }
    validate_audio_signature(&media_type, &body)?;
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

async fn create_generic_attachment(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(command): Json<CreateAudioAttachmentCommand>,
) -> Result<Json<AudioAttachment>, AppError> {
    if command.idempotency_key.trim().is_empty()
        || command.idempotency_key.len() > 256
        || command
            .duration_ms
            .is_some_and(|duration| duration == 0 || duration > MAX_VOICE_NOTE_DURATION_MS)
    {
        return Err(AppError::BadRequest(
            "valid idempotency key and duration up to 10 minutes are required".to_owned(),
        ));
    }
    let runtime = state.audio_runtime()?;
    let mut tx = runtime.pool.begin().await.map_err(log_db)?;
    let mutation_key = format!(
        "audio:generic-attachment:{}",
        command.idempotency_key.trim()
    );
    let hash = request_hash(&command)?;
    if let Some(previous) =
        replay_mutation::<AudioAttachment>(&mut tx, session.user_id, &mutation_key, hash).await?
    {
        return Ok(Json(previous));
    }
    let id = Uuid::now_v7();
    let row = query(
        "INSERT INTO audio_attachments
         (id, owner_id, upload_id, media_type, byte_length, checksum_sha256, retention, duration_ms)
         SELECT $1, owner_id, id, media_type, byte_length, checksum_sha256, $4, $5
           FROM audio_uploads
          WHERE id = $2 AND owner_id = $3 AND status = 'completed'
         RETURNING id, media_type, byte_length, duration_ms, checksum_sha256,
                   retention, audio_deleted_at, created_at",
    )
    .bind(id)
    .bind(command.upload_id)
    .bind(session.user_id)
    .bind(retention_name(command.retention))
    .bind(
        command
            .duration_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| AppError::BadRequest("audio duration is invalid".to_owned()))?,
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("audio_upload"))?;
    let attachment = row_to_attachment(&row)?;
    store_mutation(
        &mut tx,
        session.user_id,
        &mutation_key,
        "create_generic_audio_attachment",
        hash,
        &attachment,
    )
    .await?;
    tx.commit().await.map_err(log_db)?;
    tracing::info!(
        event = "records.voice_attachment_created",
        owner_id = %session.user_id,
        attachment_id = %attachment.id,
        byte_length = attachment.byte_length,
        "generic audio attachment created"
    );
    Ok(Json(attachment))
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
    let mutation_key = format!("audio:attachment:{}", command.idempotency_key.trim());
    let hash = request_hash(&command)?;
    if let Some(previous) =
        replay_mutation::<AudioAttachment>(&mut tx, session.user_id, &mutation_key, hash).await?
    {
        return Ok(Json(previous));
    }
    let owns_session_item: bool = sqlx_core::query_scalar::query_scalar(
        "SELECT EXISTS(
           SELECT 1
             FROM learning_sessions session
             JOIN learning_session_items item
               ON item.session_id = session.session_id
              AND item.item_id = $2
            WHERE session.session_id = $1
              AND session.user_id = $3
         )",
    )
    .bind(command.session_id)
    .bind(command.item_id)
    .bind(session.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(log_db)?;
    if !owns_session_item {
        return Err(AppError::NotFound("learning session item"));
    }
    let id = Uuid::now_v7();
    let retention = retention_name(command.retention);
    let row = query(
        "INSERT INTO audio_attachments
         (id, owner_id, upload_id, media_type, byte_length, checksum_sha256, retention)
         SELECT $1, owner_id, id, media_type, byte_length, checksum_sha256, $4
         FROM audio_uploads WHERE id = $2 AND owner_id = $3 AND status = 'completed'
         RETURNING id, media_type, byte_length, duration_ms, checksum_sha256,
                   retention, audio_deleted_at, created_at",
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
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .bind(command.session_id)
    .bind(command.item_id)
    .bind(command.idempotency_key.trim())
    .execute(&mut *tx)
    .await
    .map_err(log_db)?;
    let attachment = row_to_attachment(&row)?;
    store_mutation(
        &mut tx,
        session.user_id,
        &mutation_key,
        "create_audio_attachment",
        hash,
        &attachment,
    )
    .await?;
    tx.commit().await.map_err(log_db)?;
    Ok(Json(attachment))
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
    headers: HeaderMap,
) -> Result<axum::response::Response, AppError> {
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
    let full_length = bytes.len();
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .map(|value| parse_byte_range(value, full_length))
        .transpose()?;
    let (status, payload, content_range) = if let Some((start, end)) = range {
        (
            StatusCode::PARTIAL_CONTENT,
            bytes[start..=end].to_vec(),
            Some(format!("bytes {start}-{end}/{full_length}")),
        )
    } else {
        (StatusCode::OK, bytes, None)
    };
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&media_type)
            .map_err(|_| AppError::Unavailable("audio media type"))?,
    );
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline"),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&payload.len().to_string())
            .map_err(|_| AppError::Unavailable("audio length"))?,
    );
    if let Some(content_range) = content_range {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range)
                .map_err(|_| AppError::Unavailable("audio range"))?,
        );
    }
    Ok((status, response_headers, payload).into_response())
}

async fn delete_audio(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(attachment_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let referenced: bool = sqlx_core::query_scalar::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM learning_attachment_refs WHERE attachment_id = $1
            UNION ALL
            SELECT 1 FROM annotations
             WHERE audio_attachment_id = $1 AND deleted_at IS NULL
        )",
    )
    .bind(attachment_id)
    .fetch_one(&state.audio_runtime()?.pool)
    .await
    .map_err(log_db)?;
    if referenced {
        return Err(AppError::Conflict(
            "audio attachment is still referenced".to_owned(),
        ));
    }
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
    Json(command): Json<TranscribeAudioCommand>,
) -> Result<Json<TranscriptArtifact>, AppError> {
    let started = Instant::now();
    if command.idempotency_key.trim().is_empty()
        || command
            .language
            .as_deref()
            .is_some_and(|language| language.trim().is_empty() || language.len() > 64)
    {
        return Err(AppError::BadRequest(
            "transcription idempotency key or language is invalid".to_owned(),
        ));
    }
    let _permit = state
        .learning_operation_limits()
        .acquire(
            session.user_id,
            crate::learning::limits::LearningOperationKind::Transcription,
        )
        .map_err(|error| match error {
            crate::learning::limits::LearningOperationLimitError::RateLimited
            | crate::learning::limits::LearningOperationLimitError::Busy => {
                AppError::TooManyRequests("transcription operation limit reached")
            }
            crate::learning::limits::LearningOperationLimitError::Unavailable => {
                AppError::Unavailable("learning operation limiter")
            }
        })?;
    let runtime = state.audio_runtime()?;
    let mut tx = runtime.pool.begin().await.map_err(log_db)?;
    let mutation_key = format!("audio:transcribe:{}", command.idempotency_key.trim());
    let hash = request_hash(&(attachment_id, &command))?;
    if let Some(previous) =
        replay_mutation::<TranscriptArtifact>(&mut tx, session.user_id, &mutation_key, hash).await?
    {
        return Ok(Json(previous));
    }
    let attachment_exists: bool = sqlx_core::query_scalar::query_scalar(
        "SELECT EXISTS(
           SELECT 1 FROM audio_attachments
            WHERE id = $1 AND owner_id = $2 AND audio_deleted_at IS NULL
         )",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(log_db)?;
    if !attachment_exists {
        return Err(AppError::NotFound("audio_attachment"));
    }
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
    .fetch_one(&mut *tx)
    .await
    .map_err(log_db)?;
    let transcript = TranscriptArtifact {
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
    };
    store_mutation(
        &mut tx,
        session.user_id,
        &mutation_key,
        "request_audio_transcription",
        hash,
        &transcript,
    )
    .await?;
    tx.commit().await.map_err(log_db)?;
    let transcript = execute_transcription(&state, runtime, session.user_id, transcript).await?;
    query(
        "UPDATE learning_mutations
            SET response_payload = $3
          WHERE user_id = $1 AND command_key = $2",
    )
    .bind(session.user_id)
    .bind(&mutation_key)
    .bind(serde_json::to_value(&transcript).map_err(|_| AppError::Unavailable("audio mutation"))?)
    .execute(&runtime.pool)
    .await
    .map_err(log_db)?;
    tracing::info!(
        event = "learning.transcription_requested",
        owner_id = %session.user_id,
        attachment_id = %attachment_id,
        transcript_id = %transcript.id,
        latency_ms = started.elapsed().as_millis(),
        "transcription request persisted"
    );
    Ok(Json(transcript))
}

async fn execute_transcription(
    state: &AppState,
    runtime: &AudioRuntime,
    owner_id: UserId,
    transcript: TranscriptArtifact,
) -> Result<TranscriptArtifact, AppError> {
    query(
        "UPDATE transcript_artifacts
            SET status = 'processing', provider = 'openai', model = 'whisper-1'
          WHERE id = $1 AND owner_id = $2 AND status = 'pending'",
    )
    .bind(transcript.id)
    .bind(owner_id)
    .execute(&runtime.pool)
    .await
    .map_err(log_db)?;
    let result = call_transcription_provider(state, runtime, owner_id, &transcript).await;
    match result {
        Ok(response) => {
            if response.text.trim().is_empty() || response.text.len() > 512 * 1024 {
                return finish_failed_transcription(
                    runtime,
                    owner_id,
                    transcript.id,
                    TranscriptionProviderError::InvalidResponse,
                )
                .await;
            }
            let row = query(
                "UPDATE transcript_artifacts
                    SET status = 'needs_review',
                        transcript_text = $3,
                        language = COALESCE($4, language),
                        provider = 'openai',
                        model = 'whisper-1'
                  WHERE id = $1 AND owner_id = $2 AND status = 'processing'
                RETURNING id, attachment_id, revision, status, transcript_text, provider,
                          model, language, created_at, accepted_at",
            )
            .bind(transcript.id)
            .bind(owner_id)
            .bind(response.text.trim())
            .bind(response.language)
            .fetch_one(&runtime.pool)
            .await
            .map_err(log_db)?;
            query(
                "UPDATE ai_provider_credentials
                    SET state = 'valid', validation_code = 'ok',
                        last_validated_at = now(), object_revision = object_revision + 1,
                        updated_at = now()
                  WHERE user_id = $1 AND provider_kind = 'openai'
                    AND revoked_at IS NULL",
            )
            .bind(owner_id)
            .execute(&runtime.pool)
            .await
            .map_err(log_db)?;
            row_to_transcript(&row)
        }
        Err(error) => {
            if matches!(error, TranscriptionProviderError::Authentication) {
                query(
                    "UPDATE ai_provider_credentials
                        SET state = 'invalid', validation_code = 'authentication',
                            last_validated_at = now(), object_revision = object_revision + 1,
                            updated_at = now()
                      WHERE user_id = $1 AND provider_kind = 'openai'
                        AND revoked_at IS NULL",
                )
                .bind(owner_id)
                .execute(&runtime.pool)
                .await
                .map_err(log_db)?;
            }
            finish_failed_transcription(runtime, owner_id, transcript.id, error).await
        }
    }
}

async fn call_transcription_provider(
    state: &AppState,
    runtime: &AudioRuntime,
    owner_id: UserId,
    transcript: &TranscriptArtifact,
) -> Result<OpenAiTranscriptionResponse, TranscriptionProviderError> {
    let ai = state
        .ai_runtime()
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    let secret_id: Uuid = sqlx_core::query_scalar::query_scalar(
        "SELECT secret_id
           FROM ai_provider_credentials
          WHERE user_id = $1 AND provider_kind = 'openai'
            AND state IN ('unvalidated', 'valid') AND revoked_at IS NULL",
    )
    .bind(owner_id)
    .fetch_optional(ai.pool())
    .await
    .map_err(|_| TranscriptionProviderError::Unavailable)?
    .ok_or(TranscriptionProviderError::MissingCredential)?;
    let context = SecretContext::new(owner_id, "provider:openai")
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    let secret = ai
        .secrets()
        .load(&context, secret_id)
        .await
        .map_err(|error| match error {
            SecretStoreError::NotFound => TranscriptionProviderError::MissingCredential,
            _ => TranscriptionProviderError::Unavailable,
        })?;
    let attachment = query(
        "SELECT media_type, checksum_sha256
           FROM audio_attachments
          WHERE id = $1 AND owner_id = $2 AND audio_deleted_at IS NULL",
    )
    .bind(transcript.attachment_id)
    .bind(owner_id)
    .fetch_optional(&runtime.pool)
    .await
    .map_err(|_| TranscriptionProviderError::Unavailable)?
    .ok_or(TranscriptionProviderError::Unavailable)?;
    let media_type: String = attachment
        .try_get("media_type")
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    let checksum: String = attachment
        .try_get("checksum_sha256")
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    let bytes = runtime
        .blobs
        .get(&checksum)
        .await
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    let endpoint = validate_transcription_endpoint(ai.transcription_endpoint())?;
    let credential = secret
        .expose_str()
        .map_err(|_| TranscriptionProviderError::Authentication)?;
    let part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(audio_filename(&media_type))
        .mime_str(&media_type)
        .map_err(|_| TranscriptionProviderError::InvalidResponse)?;
    let mut form = reqwest::multipart::Form::new()
        .text("model", TRANSCRIPTION_MODEL.to_owned())
        .text("response_format", "verbose_json")
        .part("file", part);
    if let Some(language) = transcript.language.as_deref() {
        form = form.text("language", language.to_owned());
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(90))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    let response = client
        .post(endpoint)
        .bearer_auth(credential)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                TranscriptionProviderError::Timeout
            } else {
                TranscriptionProviderError::Unavailable
            }
        })?;
    match response.status() {
        status if status.is_success() => {}
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
            return Err(TranscriptionProviderError::Authentication);
        }
        reqwest::StatusCode::TOO_MANY_REQUESTS => {
            return Err(TranscriptionProviderError::RateLimited);
        }
        reqwest::StatusCode::PAYMENT_REQUIRED => {
            return Err(TranscriptionProviderError::QuotaExhausted);
        }
        status if status.is_server_error() => {
            return Err(TranscriptionProviderError::Unavailable);
        }
        _ => return Err(TranscriptionProviderError::InvalidResponse),
    }
    let body = response
        .bytes()
        .await
        .map_err(|_| TranscriptionProviderError::Unavailable)?;
    if body.len() > MAX_TRANSCRIPT_RESPONSE_BYTES {
        return Err(TranscriptionProviderError::InvalidResponse);
    }
    serde_json::from_slice(&body).map_err(|_| TranscriptionProviderError::InvalidResponse)
}

async fn finish_failed_transcription(
    runtime: &AudioRuntime,
    owner_id: UserId,
    transcript_id: Uuid,
    error: TranscriptionProviderError,
) -> Result<TranscriptArtifact, AppError> {
    let row = query(
        "UPDATE transcript_artifacts
            SET status = 'failed', provider = 'openai', model = 'whisper-1'
          WHERE id = $1 AND owner_id = $2 AND status IN ('pending', 'processing')
        RETURNING id, attachment_id, revision, status, transcript_text, provider,
                  model, language, created_at, accepted_at",
    )
    .bind(transcript_id)
    .bind(owner_id)
    .fetch_one(&runtime.pool)
    .await
    .map_err(log_db)?;
    tracing::warn!(
        event = "learning.transcription_failed",
        owner_id = %owner_id,
        transcript_id = %transcript_id,
        error_code = error.code(),
        "transcription provider call failed"
    );
    row_to_transcript(&row)
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
    let started = Instant::now();
    if command.text.trim().is_empty() || command.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "reviewed transcript and idempotency key are required".to_owned(),
        ));
    }
    let runtime = state.audio_runtime()?;
    let mut tx = runtime.pool.begin().await.map_err(log_db)?;
    let mutation_key = format!("audio:accept:{}", command.idempotency_key.trim());
    let hash = request_hash(&(attachment_id, &command))?;
    if let Some(previous) =
        replay_mutation::<TranscriptArtifact>(&mut tx, session.user_id, &mutation_key, hash).await?
    {
        return Ok(Json(previous));
    }
    let row = query(
        "INSERT INTO transcript_artifacts
         (id, owner_id, attachment_id, revision, status, transcript_text,
          provider, model, language, accepted_at)
         SELECT $1, source.owner_id, source.attachment_id, source.revision + 1,
                'accepted', $4, 'user', 'edited', source.language, now()
           FROM transcript_artifacts source
           JOIN audio_attachments attachment
             ON attachment.id = source.attachment_id
            AND attachment.owner_id = source.owner_id
          WHERE source.id = (
                  SELECT latest.id
                    FROM transcript_artifacts latest
                   WHERE latest.attachment_id = $3
                     AND latest.owner_id = $2
                   ORDER BY latest.revision DESC
                   LIMIT 1
                   FOR UPDATE
                )
            AND source.status = 'needs_review'
         RETURNING id, attachment_id, revision, status, transcript_text, provider, model,
                   language, created_at, accepted_at",
    )
    .bind(Uuid::now_v7())
    .bind(session.user_id)
    .bind(attachment_id)
    .bind(command.text.trim())
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_db)?
    .ok_or_else(|| AppError::Conflict("latest transcript is not ready for review".to_owned()))?;
    query(
        "UPDATE audio_attachments attachment SET audio_deleted_at = now()
         WHERE attachment.id = $1 AND attachment.owner_id = $2
           AND attachment.retention = 'delete_after_transcript'
           AND NOT EXISTS (
               SELECT 1 FROM annotations annotation
                WHERE annotation.audio_attachment_id = attachment.id
                  AND annotation.deleted_at IS NULL
           )",
    )
    .bind(attachment_id)
    .bind(session.user_id)
    .execute(&mut *tx)
    .await
    .map_err(log_db)?;
    let transcript = row_to_transcript(&row)?;
    store_mutation(
        &mut tx,
        session.user_id,
        &mutation_key,
        "accept_audio_transcript",
        hash,
        &transcript,
    )
    .await?;
    tx.commit().await.map_err(log_db)?;
    tracing::info!(
        event = "learning.transcript_accepted",
        owner_id = %session.user_id,
        attachment_id = %attachment_id,
        transcript_id = %transcript.id,
        latency_ms = started.elapsed().as_millis(),
        "reviewed transcript accepted"
    );
    Ok(Json(transcript))
}

async fn attachment_row(
    runtime: &AudioRuntime,
    owner_id: UserId,
    attachment_id: Uuid,
) -> Result<sqlx_postgres::PgRow, AppError> {
    query(
        "SELECT id, media_type, byte_length, duration_ms, checksum_sha256, retention,
                audio_deleted_at, created_at
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
        duration_ms: row
            .try_get::<Option<i64>, _>("duration_ms")
            .map_err(log_db)?
            .map(u64::try_from)
            .transpose()
            .map_err(|_| AppError::Unavailable("audio duration"))?,
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

fn validate_audio_signature(media_type: &str, bytes: &[u8]) -> Result<(), AppError> {
    let valid = match media_type {
        "audio/webm" => bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "audio/ogg" => bytes.starts_with(b"OggS"),
        "audio/mp4" => bytes.get(4..8) == Some(b"ftyp"),
        "audio/mpeg" => {
            bytes.starts_with(b"ID3")
                || bytes
                    .get(..2)
                    .is_some_and(|prefix| prefix[0] == 0xff && prefix[1] & 0xe0 == 0xe0)
        }
        "audio/wav" | "audio/x-wav" => {
            bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WAVE")
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(AppError::BadRequest(
            "audio bytes do not match the declared media type".to_owned(),
        ))
    }
}

fn parse_byte_range(value: &str, length: usize) -> Result<(usize, usize), AppError> {
    let value = value
        .strip_prefix("bytes=")
        .ok_or_else(|| AppError::BadRequest("invalid audio range".to_owned()))?;
    if value.contains(',') || length == 0 {
        return Err(AppError::BadRequest("invalid audio range".to_owned()));
    }
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| AppError::BadRequest("invalid audio range".to_owned()))?;
    let (start, end) = if start.is_empty() {
        let suffix = end
            .parse::<usize>()
            .map_err(|_| AppError::BadRequest("invalid audio range".to_owned()))?;
        if suffix == 0 {
            return Err(AppError::BadRequest("invalid audio range".to_owned()));
        }
        (length.saturating_sub(suffix), length - 1)
    } else {
        let start = start
            .parse::<usize>()
            .map_err(|_| AppError::BadRequest("invalid audio range".to_owned()))?;
        let end = if end.is_empty() {
            length - 1
        } else {
            end.parse::<usize>()
                .map_err(|_| AppError::BadRequest("invalid audio range".to_owned()))?
                .min(length - 1)
        };
        (start, end)
    };
    if start >= length || start > end {
        return Err(AppError::BadRequest("invalid audio range".to_owned()));
    }
    Ok((start, end))
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

fn parse_credential_state(value: &str) -> AiCredentialState {
    match value {
        "valid" => AiCredentialState::Valid,
        "invalid" => AiCredentialState::Invalid,
        _ => AiCredentialState::Unvalidated,
    }
}

fn validate_transcription_endpoint(value: &str) -> Result<Url, TranscriptionProviderError> {
    let url = Url::parse(value).map_err(|_| TranscriptionProviderError::Unavailable)?;
    let loopback_http = url.scheme() == "http"
        && url.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
    if (url.scheme() != "https" && !loopback_http)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TranscriptionProviderError::Unavailable);
    }
    Ok(url)
}

fn audio_filename(media_type: &str) -> &'static str {
    match media_type {
        "audio/ogg" => "recording.ogg",
        "audio/mp4" => "recording.m4a",
        "audio/mpeg" => "recording.mp3",
        "audio/wav" | "audio/x-wav" => "recording.wav",
        _ => "recording.webm",
    }
}

fn fingerprint_prefix(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(16);
    for byte in bytes.iter().take(8) {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn request_hash(value: &impl Serialize) -> Result<[u8; 32], AppError> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| AppError::BadRequest("audio request is invalid".to_owned()))?;
    Ok(Sha256::digest(encoded).into())
}

async fn replay_mutation<T: DeserializeOwned>(
    tx: &mut sqlx_postgres::PgTransaction<'_>,
    owner_id: UserId,
    command_key: &str,
    hash: [u8; 32],
) -> Result<Option<T>, AppError> {
    let row = query(
        "SELECT request_hash, response_payload
           FROM learning_mutations
          WHERE user_id = $1 AND command_key = $2",
    )
    .bind(owner_id)
    .bind(command_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_db)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(log_db)?;
    if stored_hash.as_slice() != hash {
        return Err(AppError::Conflict(
            "idempotency key was already used with another audio request".to_owned(),
        ));
    }
    let payload = row.try_get("response_payload").map_err(log_db)?;
    serde_json::from_value(payload)
        .map(Some)
        .map_err(|_| AppError::Unavailable("audio mutation"))
}

async fn store_mutation<T: Serialize>(
    tx: &mut sqlx_postgres::PgTransaction<'_>,
    owner_id: UserId,
    command_key: &str,
    operation: &str,
    hash: [u8; 32],
    response: &T,
) -> Result<(), AppError> {
    let response =
        serde_json::to_value(response).map_err(|_| AppError::Unavailable("audio mutation"))?;
    query(
        "INSERT INTO learning_mutations
         (user_id, command_key, operation, request_hash, response_payload)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(owner_id)
    .bind(command_key)
    .bind(operation)
    .bind(hash.as_slice())
    .bind(response)
    .execute(&mut **tx)
    .await
    .map_err(log_db)?;
    Ok(())
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

fn map_secret_error(error: SecretStoreError) -> AppError {
    match error {
        SecretStoreError::InvalidContext | SecretStoreError::InvalidPlaintext => {
            AppError::BadRequest("provider credential is invalid".to_owned())
        }
        SecretStoreError::NotFound => AppError::NotFound("provider credential"),
        SecretStoreError::Integrity
        | SecretStoreError::KeyRing
        | SecretStoreError::Storage
        | SecretStoreError::State => AppError::Unavailable("provider secret store"),
    }
}

fn log_db(error: sqlx_core::error::Error) -> AppError {
    tracing::error!(%error, "audio repository operation failed");
    AppError::Unavailable("audio repository")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcription_endpoint_allows_https_and_loopback_fixture_only() {
        assert!(
            validate_transcription_endpoint("https://api.openai.com/v1/audio/transcriptions")
                .is_ok()
        );
        assert!(
            validate_transcription_endpoint("http://127.0.0.1:4010/v1/audio/transcriptions")
                .is_ok()
        );
        assert!(
            validate_transcription_endpoint("http://provider.example/v1/audio/transcriptions")
                .is_err()
        );
        assert!(validate_transcription_endpoint(
            "https://token@api.openai.com/v1/audio/transcriptions"
        )
        .is_err());
    }

    #[test]
    fn audio_request_hash_includes_payload() -> Result<(), AppError> {
        let first = TranscribeAudioCommand {
            language: Some("ru".to_owned()),
            idempotency_key: "same-key".to_owned(),
        };
        let changed = TranscribeAudioCommand {
            language: Some("en".to_owned()),
            idempotency_key: "same-key".to_owned(),
        };

        assert_ne!(request_hash(&first)?, request_hash(&changed)?);
        Ok(())
    }

    #[test]
    fn audio_signature_validation_rejects_mime_spoofing() {
        assert!(
            validate_audio_signature("audio/webm", &[0x1a, 0x45, 0xdf, 0xa3, 0x42, 0x86]).is_ok()
        );
        assert!(validate_audio_signature("audio/webm", b"not webm").is_err());
        assert!(validate_audio_signature("audio/wav", b"RIFF0000WAVEdata").is_ok());
    }

    #[test]
    fn audio_range_parser_supports_open_and_suffix_ranges() -> Result<(), AppError> {
        assert_eq!(parse_byte_range("bytes=2-4", 10)?, (2, 4));
        assert_eq!(parse_byte_range("bytes=7-", 10)?, (7, 9));
        assert_eq!(parse_byte_range("bytes=-3", 10)?, (7, 9));
        assert!(parse_byte_range("bytes=11-12", 10).is_err());
        Ok(())
    }
}
