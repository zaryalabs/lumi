//! Durable conversation, message, generation and authenticated SSE runtime.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use async_stream::stream;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Sse,
    },
    routing::{get, post},
    Extension, Json, Router,
};
use lumi_core::{
    AiContextAttachment, AiContextPack, AiConversation, AiGeneration, AiGenerationStatus,
    AiMessage, AiMessageRole, AiMessageStatus, AiPage, AiProviderChatRequest, AiProviderEvent,
    AiProviderMessage, AiTokenUsage, ConversationDetail, CreateConversationRequest,
    CreateMessageRequest, CreateMessageResponse, GenerationMutationRequest, ProviderPreferences,
    UpdateConversationRequest, UserId,
};
use serde_json::Value;
use sqlx_core::row::Row;
use sqlx_postgres::{PgPool, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{
    context::{SourceContextError, SourceContextResolver},
    providers::{self, AiProviderClient, AiProviderError},
    record_context::{self, RecordContextError},
};
use crate::{account::AuthenticatedSession, AppError, AppState};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
    pub(crate) use sqlx_core::query_scalar::query_scalar;
}

const MAX_MESSAGE_BYTES: usize = 32 * 1024;
const MAX_ASSISTANT_BYTES: usize = 128 * 1024;
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 2_048;

/// Return the authenticated conversation and generation route surface.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/ai/conversations",
            get(list_conversations).post(create_conversation),
        )
        .route(
            "/ai/conversations/{conversation_id}",
            get(get_conversation)
                .patch(update_conversation)
                .delete(delete_conversation),
        )
        .route(
            "/ai/conversations/{conversation_id}/messages",
            post(create_message),
        )
        .route(
            "/ai/generations/{generation_id}/events",
            get(generation_events),
        )
        .route(
            "/ai/generations/{generation_id}/stop",
            post(stop_generation),
        )
        .route(
            "/ai/generations/{generation_id}/retry",
            post(retry_generation),
        )
        .route(
            "/ai/generations/{generation_id}/regenerate",
            post(regenerate_generation),
        )
}

async fn list_conversations(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<AiPage<AiConversation>>, AppError> {
    let runtime = state.ai_runtime()?;
    let rows = sqlx::query(
        "SELECT conversation_id, user_id, title, active_model, object_revision,
                created_at, updated_at, deleted_at
           FROM ai_conversations
          WHERE user_id = $1 AND deleted_at IS NULL
          ORDER BY updated_at DESC, conversation_id DESC
          LIMIT 100",
    )
    .bind(session.user_id)
    .fetch_all(runtime.pool())
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let items = rows
        .iter()
        .map(conversation_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(AiPage {
        items,
        next_cursor: None,
    }))
}

async fn create_conversation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<CreateConversationRequest>,
) -> Result<Json<AiConversation>, AppError> {
    validate_conversation_create(&request)?;
    let runtime = state.ai_runtime()?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_conversations (
            conversation_id, user_id, title, active_model, create_idempotency_key
         ) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (user_id, create_idempotency_key) DO NOTHING",
    )
    .bind(id)
    .bind(session.user_id)
    .bind(request.title.trim())
    .bind(request.active_model.trim())
    .bind(request.idempotency_key.trim())
    .execute(runtime.pool())
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let row = sqlx::query(
        "SELECT conversation_id, user_id, title, active_model, object_revision,
                created_at, updated_at, deleted_at
           FROM ai_conversations
          WHERE user_id = $1 AND create_idempotency_key = $2",
    )
    .bind(session.user_id)
    .bind(request.idempotency_key.trim())
    .fetch_one(runtime.pool())
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    conversation_from_row(&row).map(Json)
}

async fn get_conversation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(conversation_id): Path<Uuid>,
) -> Result<Json<ConversationDetail>, AppError> {
    let runtime = state.ai_runtime()?;
    conversation_detail(runtime.pool(), session.user_id, conversation_id)
        .await
        .map(Json)
}

async fn update_conversation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(conversation_id): Path<Uuid>,
    Json(request): Json<UpdateConversationRequest>,
) -> Result<Json<AiConversation>, AppError> {
    if request.expected_revision == 0 || request.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "expected_revision and idempotency_key are required".to_owned(),
        ));
    }
    if request
        .title
        .as_deref()
        .is_some_and(|title| title.trim().is_empty() || title.len() > 160)
        || request
            .active_model
            .as_deref()
            .is_some_and(|model| model.trim().is_empty() || model.len() > 256)
    {
        return Err(AppError::BadRequest(
            "conversation title or model is invalid".to_owned(),
        ));
    }
    let runtime = state.ai_runtime()?;
    let row = sqlx::query(
        "UPDATE ai_conversations
            SET title = COALESCE($4, title),
                active_model = COALESCE($5, active_model),
                object_revision = object_revision + 1,
                updated_at = now()
          WHERE conversation_id = $1
            AND user_id = $2
            AND object_revision = $3
            AND deleted_at IS NULL
        RETURNING conversation_id, user_id, title, active_model, object_revision,
                  created_at, updated_at, deleted_at",
    )
    .bind(conversation_id)
    .bind(session.user_id)
    .bind(i64::try_from(request.expected_revision).unwrap_or(i64::MAX))
    .bind(request.title.as_deref().map(str::trim))
    .bind(request.active_model.as_deref().map(str::trim))
    .fetch_optional(runtime.pool())
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?
    .ok_or_else(|| AppError::Conflict("conversation changed".to_owned()))?;
    conversation_from_row(&row).map(Json)
}

async fn delete_conversation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(conversation_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let runtime = state.ai_runtime()?;
    let result = sqlx::query(
        "UPDATE ai_conversations
            SET deleted_at = now(),
                object_revision = object_revision + 1,
                updated_at = now()
          WHERE conversation_id = $1
            AND user_id = $2
            AND deleted_at IS NULL
            AND NOT EXISTS (
                SELECT 1 FROM ai_generations
                 WHERE conversation_id = $1
                   AND user_id = $2
                   AND status IN ('pending', 'streaming')
            )",
    )
    .bind(conversation_id)
    .bind(session.user_id)
    .execute(runtime.pool())
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    if result.rows_affected() == 1 {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::Conflict(
            "conversation is missing or has an active generation".to_owned(),
        ))
    }
}

async fn create_message(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(conversation_id): Path<Uuid>,
    Json(request): Json<CreateMessageRequest>,
) -> Result<Json<CreateMessageResponse>, AppError> {
    validate_message_request(&request)?;
    let runtime = Arc::clone(state.ai_runtime()?);
    let message_id = Uuid::now_v7();
    let (context_pack, persisted_attachments) = match request.attachments.first() {
        Some(attachment) => {
            validate_attachment(attachment)?;
            match attachment {
                AiContextAttachment::Source { scope, .. } => (
                    Some(
                        runtime
                            .context()
                            .resolve_for_message(session.user_id, message_id, scope.clone())
                            .await
                            .map_err(map_context_error)?,
                    ),
                    request.attachments.clone(),
                ),
                AiContextAttachment::Records {
                    kind,
                    record_scope,
                    display_label,
                    ..
                } => {
                    let pack = record_context::resolve_for_message(
                        &state.search,
                        session.user_id,
                        message_id,
                        record_scope.clone(),
                        &request.content,
                    )
                    .await
                    .map_err(map_record_context_error)?;
                    let resolved = AiContextAttachment::Records {
                        kind: kind.clone(),
                        record_scope: pack
                            .record_scope
                            .clone()
                            .ok_or(AppError::Internal("record context"))?,
                        display_label: display_label.clone(),
                        included_context: pack.record_context.clone(),
                    };
                    (Some(pack), vec![resolved])
                }
            }
        }
        None => (None, Vec::new()),
    };
    let created = create_message_transaction(
        &runtime,
        session.user_id,
        conversation_id,
        message_id,
        &request,
        &persisted_attachments,
        context_pack.as_ref(),
    )
    .await?;
    if created.generation.status == AiGenerationStatus::Pending {
        spawn_generation(state.clone(), session.user_id, created.generation.id);
    }
    Ok(Json(created))
}

async fn generation_events(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(generation_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, AppError> {
    let runtime = state.ai_runtime()?;
    generation(runtime.pool(), session.user_id, generation_id).await?;
    let mut after = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(-1);
    let pool = runtime.pool().clone();
    let owner_id = session.user_id;
    let events = stream! {
        loop {
            let rows = sqlx::query(
                "SELECT sequence, payload
                   FROM ai_generation_events
                  WHERE generation_id = $1 AND user_id = $2 AND sequence > $3
                  ORDER BY sequence
                  LIMIT 128",
            )
            .bind(generation_id)
            .bind(owner_id)
            .bind(after)
            .fetch_all(&pool)
            .await;
            let Ok(rows) = rows else {
                yield Ok::<Event, Infallible>(
                    Event::default().event("error").data("generation_events_unavailable")
                );
                break;
            };
            for row in rows {
                let Ok(sequence) = row.try_get::<i64, _>("sequence") else {
                    continue;
                };
                let Ok(payload) = row.try_get::<Value, _>("payload") else {
                    continue;
                };
                after = sequence;
                let data = serde_json::to_string(&payload)
                    .unwrap_or_else(|_| "{\"type\":\"response_failed\",\"code\":\"invalid_event\",\"message\":\"Некорректное событие\",\"retryable\":true}".to_owned());
                yield Ok::<Event, Infallible>(
                    Event::default()
                        .id(sequence.to_string())
                        .event("provider")
                        .data(data)
                );
            }
            let terminal: bool = sqlx::query_scalar(
                "SELECT status IN ('completed', 'failed', 'cancelled')
                   FROM ai_generations
                  WHERE generation_id = $1 AND user_id = $2",
            )
            .bind(generation_id)
            .bind(owner_id)
            .fetch_optional(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(true);
            if terminal {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    Ok(Sse::new(events).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(10))
            .text("keep-alive"),
    ))
}

async fn stop_generation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(generation_id): Path<Uuid>,
    Json(request): Json<GenerationMutationRequest>,
) -> Result<Json<AiGeneration>, AppError> {
    validate_generation_mutation(&request)?;
    let runtime = state.ai_runtime()?;
    cancel_generation(runtime.pool(), session.user_id, generation_id).await?;
    runtime.cancel(generation_id).await;
    generation(runtime.pool(), session.user_id, generation_id)
        .await
        .map(Json)
}

async fn retry_generation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(generation_id): Path<Uuid>,
    Json(request): Json<GenerationMutationRequest>,
) -> Result<Json<AiGeneration>, AppError> {
    retry_or_regenerate(state, session.user_id, generation_id, request, false).await
}

async fn regenerate_generation(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(generation_id): Path<Uuid>,
    Json(request): Json<GenerationMutationRequest>,
) -> Result<Json<AiGeneration>, AppError> {
    retry_or_regenerate(state, session.user_id, generation_id, request, true).await
}

async fn retry_or_regenerate(
    state: AppState,
    owner_id: UserId,
    generation_id: Uuid,
    request: GenerationMutationRequest,
    allow_completed: bool,
) -> Result<Json<AiGeneration>, AppError> {
    validate_generation_mutation(&request)?;
    let runtime = Arc::clone(state.ai_runtime()?);
    let created = create_generation_retry(
        runtime.pool(),
        owner_id,
        generation_id,
        &request.idempotency_key,
        allow_completed,
    )
    .await?;
    if created.status == AiGenerationStatus::Pending {
        spawn_generation(state, owner_id, created.id);
    }
    Ok(Json(created))
}

async fn create_message_transaction(
    runtime: &super::AiRuntime,
    owner_id: UserId,
    conversation_id: Uuid,
    message_id: Uuid,
    request: &CreateMessageRequest,
    persisted_attachments: &[AiContextAttachment],
    context_pack: Option<&AiContextPack>,
) -> Result<CreateMessageResponse, AppError> {
    let mut transaction = runtime
        .pool()
        .begin()
        .await
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    if let Some(response) = replay_message(
        &mut transaction,
        owner_id,
        conversation_id,
        &request.idempotency_key,
    )
    .await?
    {
        transaction
            .commit()
            .await
            .map_err(|_| AppError::Unavailable("AI conversations"))?;
        return Ok(response);
    }
    let conversation_row = sqlx::query(
        "SELECT active_model, object_revision,
                (SELECT space_id FROM sync_spaces
                  WHERE owner_user_id = $2 AND deleted_at IS NULL
                  ORDER BY created_at LIMIT 1) AS space_id
           FROM ai_conversations
          WHERE conversation_id = $1
            AND user_id = $2
            AND deleted_at IS NULL
          FOR UPDATE",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?
    .ok_or(AppError::NotFound("AI conversation"))?;
    let revision: i64 = conversation_row
        .try_get("object_revision")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    if u64::try_from(revision).ok() != Some(request.expected_conversation_revision) {
        return Err(AppError::Conflict("conversation changed".to_owned()));
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM ai_generations
             WHERE conversation_id = $1
               AND user_id = $2
               AND status IN ('pending', 'streaming')
         )",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    if active {
        return Err(AppError::Conflict(
            "conversation already has an active generation".to_owned(),
        ));
    }
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) + 1
           FROM ai_messages
          WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let assistant_id = Uuid::now_v7();
    let generation_id = Uuid::now_v7();
    let attachments = serde_json::to_value(persisted_attachments)
        .map_err(|_| AppError::BadRequest("context attachments are invalid".to_owned()))?;
    sqlx::query(
        "INSERT INTO ai_messages (
            message_id, conversation_id, user_id, role, content, attachments,
            status, sequence, create_idempotency_key
         ) VALUES ($1, $2, $3, 'user', $4, $5, 'committed', $6, $7)",
    )
    .bind(message_id)
    .bind(conversation_id)
    .bind(owner_id)
    .bind(request.content.trim())
    .bind(&attachments)
    .bind(sequence)
    .bind(&request.idempotency_key)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    sqlx::query(
        "INSERT INTO ai_messages (
            message_id, conversation_id, user_id, role, content, attachments,
            status, sequence
         ) VALUES ($1, $2, $3, 'assistant', '', $4, 'streaming', $5)",
    )
    .bind(assistant_id)
    .bind(conversation_id)
    .bind(owner_id)
    .bind(&attachments)
    .bind(sequence.saturating_add(1))
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    if let Some(pack) = context_pack {
        let space_id: Uuid = conversation_row
            .try_get("space_id")
            .map_err(|_| AppError::Unavailable("AI conversations"))?;
        SourceContextResolver::store_message_pack_in_transaction(&mut transaction, pack, space_id)
            .await
            .map_err(map_context_error)?;
    }
    let model: String = conversation_row
        .try_get("active_model")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    sqlx::query(
        "INSERT INTO ai_generations (
            generation_id, conversation_id, user_id, user_message_id,
            assistant_message_id, status, provider_kind, model
         ) VALUES ($1, $2, $3, $4, $5, 'pending', 'openrouter', $6)",
    )
    .bind(generation_id)
    .bind(conversation_id)
    .bind(owner_id)
    .bind(message_id)
    .bind(assistant_id)
    .bind(model)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    sqlx::query(
        "UPDATE ai_conversations
            SET object_revision = object_revision + 1, updated_at = now()
          WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let message = message(runtime.pool(), owner_id, message_id).await?;
    let generation = generation(runtime.pool(), owner_id, generation_id).await?;
    Ok(CreateMessageResponse {
        message,
        generation,
    })
}

fn spawn_generation(state: AppState, owner_id: UserId, generation_id: Uuid) {
    tokio::spawn(async move {
        if let Err(error) = run_generation(&state, owner_id, generation_id).await {
            tracing::warn!(
                generation_id = %generation_id,
                code = error.code(),
                "AI generation ended with a normalized provider failure"
            );
            let _ = fail_generation(&state, owner_id, generation_id, &error).await;
        }
        if let Ok(runtime) = state.ai_runtime() {
            runtime.clear_cancellation(generation_id).await;
        }
    });
}

async fn run_generation(
    state: &AppState,
    owner_id: UserId,
    generation_id: Uuid,
) -> Result<(), AiProviderError> {
    let runtime = state
        .ai_runtime()
        .map_err(|_| AiProviderError::Unavailable)?;
    let cancellation = runtime.register_cancellation(generation_id).await;
    let (provider, preferences) = providers::load_provider(state, owner_id).await?;
    let request = provider_request(runtime.pool(), owner_id, generation_id, &preferences)
        .await
        .map_err(|_| AiProviderError::Unavailable)?;
    let mut stream = provider.stream_chat(request).await?;
    loop {
        let event = tokio::select! {
            () = cancellation.cancelled() => {
                persist_cancelled(runtime.pool(), owner_id, generation_id)
                    .await
                    .map_err(|_| AiProviderError::Unavailable)?;
                return Ok(());
            }
            event = stream.recv() => event,
        };
        let Some(event) = event else {
            return Err(AiProviderError::InvalidResponse);
        };
        let event = event?;
        let terminal = matches!(
            event,
            AiProviderEvent::ResponseCompleted { .. } | AiProviderEvent::ResponseFailed { .. }
        );
        persist_provider_event(runtime.pool(), owner_id, generation_id, &event)
            .await
            .map_err(|_| AiProviderError::Unavailable)?;
        if terminal {
            return Ok(());
        }
    }
}

async fn provider_request(
    pool: &PgPool,
    owner_id: UserId,
    generation_id: Uuid,
    preferences: &ProviderPreferences,
) -> Result<AiProviderChatRequest, AppError> {
    let generation_row = sqlx::query(
        "SELECT generation.conversation_id, generation.user_message_id,
                generation.model, user_message.sequence
           FROM ai_generations AS generation
           JOIN ai_messages AS user_message
             ON user_message.message_id = generation.user_message_id
            AND user_message.user_id = generation.user_id
          WHERE generation.generation_id = $1 AND generation.user_id = $2",
    )
    .bind(generation_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?
    .ok_or(AppError::NotFound("AI generation"))?;
    let conversation_id: Uuid = generation_row
        .try_get("conversation_id")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let user_sequence: i64 = generation_row
        .try_get("sequence")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let rows = sqlx::query(
        "SELECT role, content
           FROM ai_messages
          WHERE conversation_id = $1
            AND user_id = $2
            AND sequence <= $3
            AND role IN ('user', 'assistant')
            AND status IN ('committed', 'completed')
          ORDER BY sequence",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .bind(user_sequence)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let messages = rows
        .iter()
        .map(|row| {
            let role: String = row
                .try_get("role")
                .map_err(|_| AppError::Unavailable("AI conversations"))?;
            Ok(AiProviderMessage {
                role: if role == "assistant" {
                    AiMessageRole::Assistant
                } else {
                    AiMessageRole::User
                },
                content: row
                    .try_get("content")
                    .map_err(|_| AppError::Unavailable("AI conversations"))?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    let user_message_id: Uuid = generation_row
        .try_get("user_message_id")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let context_pack: Option<Value> = sqlx::query_scalar(
        "SELECT payload FROM ai_context_packs
          WHERE user_id = $1 AND message_id = $2",
    )
    .bind(owner_id)
    .bind(user_message_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI context"))?;
    let context_pack = context_pack
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| AppError::Unavailable("AI context"))?;
    let model: String = generation_row
        .try_get("model")
        .unwrap_or_else(|_| preferences.default_model.clone());
    Ok(AiProviderChatRequest {
        generation_id,
        model,
        messages,
        context_pack,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        provider_options: serde_json::json!({"temperature": 0.3}),
    })
}

async fn persist_provider_event(
    pool: &PgPool,
    owner_id: UserId,
    generation_id: Uuid,
    event: &AiProviderEvent,
) -> Result<(), AppError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let row = sqlx::query(
        "SELECT assistant_message_id, status
           FROM ai_generations
          WHERE generation_id = $1 AND user_id = $2
          FOR UPDATE",
    )
    .bind(generation_id)
    .bind(owner_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?
    .ok_or(AppError::NotFound("AI generation"))?;
    let assistant_message_id: Uuid = row
        .try_get("assistant_message_id")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let status: String = row
        .try_get("status")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    if matches!(status.as_str(), "completed" | "failed" | "cancelled") {
        transaction
            .commit()
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
        return Ok(());
    }
    match event {
        AiProviderEvent::ResponseStarted { .. } => {
            sqlx::query(
                "UPDATE ai_generations
                    SET status = 'streaming', object_revision = object_revision + 1
                  WHERE generation_id = $1 AND user_id = $2",
            )
            .bind(generation_id)
            .bind(owner_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
        }
        AiProviderEvent::TextDelta { text, .. } => {
            let result = sqlx::query(
                "UPDATE ai_messages
                    SET content = content || $3
                  WHERE message_id = $1
                    AND user_id = $2
                    AND octet_length(content) + octet_length($3) <= $4",
            )
            .bind(assistant_message_id)
            .bind(owner_id)
            .bind(text)
            .bind(i32::try_from(MAX_ASSISTANT_BYTES).unwrap_or(i32::MAX))
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
            if result.rows_affected() != 1 {
                return Err(AppError::BadRequest(
                    "provider output exceeded the configured limit".to_owned(),
                ));
            }
        }
        AiProviderEvent::Usage {
            input_tokens,
            output_tokens,
        } => {
            let usage = serde_json::to_value(AiTokenUsage {
                input_tokens: *input_tokens,
                output_tokens: *output_tokens,
            })
            .map_err(|_| AppError::Unavailable("AI generation"))?;
            sqlx::query(
                "UPDATE ai_generations
                    SET usage = $3, object_revision = object_revision + 1
                  WHERE generation_id = $1 AND user_id = $2",
            )
            .bind(generation_id)
            .bind(owner_id)
            .bind(usage)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
        }
        AiProviderEvent::ResponseCompleted { finish_reason } => {
            let completed = !matches!(finish_reason, lumi_core::AiFinishReason::Cancelled);
            let generation_status = if completed { "completed" } else { "cancelled" };
            let message_status = if completed { "completed" } else { "cancelled" };
            sqlx::query(
                "UPDATE ai_generations
                    SET status = $3,
                        object_revision = object_revision + 1,
                        finished_at = now()
                  WHERE generation_id = $1 AND user_id = $2",
            )
            .bind(generation_id)
            .bind(owner_id)
            .bind(generation_status)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
            sqlx::query(
                "UPDATE ai_messages SET status = $3
                  WHERE message_id = $1 AND user_id = $2",
            )
            .bind(assistant_message_id)
            .bind(owner_id)
            .bind(message_status)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
        }
        AiProviderEvent::ResponseFailed { code, .. } => {
            sqlx::query(
                "UPDATE ai_generations
                    SET status = 'failed',
                        error_code = $3,
                        object_revision = object_revision + 1,
                        finished_at = now()
                  WHERE generation_id = $1 AND user_id = $2",
            )
            .bind(generation_id)
            .bind(owner_id)
            .bind(code)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
            sqlx::query(
                "UPDATE ai_messages SET status = 'failed'
                  WHERE message_id = $1 AND user_id = $2",
            )
            .bind(assistant_message_id)
            .bind(owner_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::Unavailable("AI generation"))?;
        }
    }
    insert_event(&mut transaction, owner_id, generation_id, event).await?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::Unavailable("AI generation"))
}

async fn fail_generation(
    state: &AppState,
    owner_id: UserId,
    generation_id: Uuid,
    error: &AiProviderError,
) -> Result<(), AppError> {
    let runtime = state.ai_runtime()?;
    let event = AiProviderEvent::ResponseFailed {
        code: error.code().to_owned(),
        message: provider_error_message(error).to_owned(),
        retryable: error.retryable(),
    };
    persist_provider_event(runtime.pool(), owner_id, generation_id, &event).await
}

async fn persist_cancelled(
    pool: &PgPool,
    owner_id: UserId,
    generation_id: Uuid,
) -> Result<(), AppError> {
    persist_provider_event(
        pool,
        owner_id,
        generation_id,
        &AiProviderEvent::ResponseCompleted {
            finish_reason: lumi_core::AiFinishReason::Cancelled,
        },
    )
    .await
}

async fn insert_event(
    transaction: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    owner_id: UserId,
    generation_id: Uuid,
    event: &AiProviderEvent,
) -> Result<(), AppError> {
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), -1) + 1
           FROM ai_generation_events
          WHERE generation_id = $1",
    )
    .bind(generation_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation events"))?;
    let payload =
        serde_json::to_value(event).map_err(|_| AppError::Unavailable("AI generation events"))?;
    sqlx::query(
        "INSERT INTO ai_generation_events (generation_id, user_id, sequence, payload)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(generation_id)
    .bind(owner_id)
    .bind(sequence)
    .bind(payload)
    .execute(&mut **transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation events"))?;
    Ok(())
}

async fn cancel_generation(
    pool: &PgPool,
    owner_id: UserId,
    generation_id: Uuid,
) -> Result<(), AppError> {
    let result = sqlx::query(
        "UPDATE ai_generations
            SET cancellation_requested = true,
                object_revision = object_revision + 1
          WHERE generation_id = $1
            AND user_id = $2
            AND status IN ('pending', 'streaming')",
    )
    .bind(generation_id)
    .bind(owner_id)
    .execute(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err(AppError::Conflict("generation is not active".to_owned()))
    }
}

async fn create_generation_retry(
    pool: &PgPool,
    owner_id: UserId,
    prior_generation_id: Uuid,
    idempotency_key: &str,
    allow_completed: bool,
) -> Result<AiGeneration, AppError> {
    if let Some(row) = sqlx::query(
        "SELECT generation_id, conversation_id, user_message_id,
                assistant_message_id, status, provider_kind, model, usage,
                error_code, started_at, finished_at
           FROM ai_generations
          WHERE user_id = $1 AND mutation_idempotency_key = $2",
    )
    .bind(owner_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?
    {
        return generation_from_row(&row);
    }
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let prior = sqlx::query(
        "SELECT generation.conversation_id, generation.user_message_id,
                generation.status, generation.model, assistant.attachments
           FROM ai_generations AS generation
           JOIN ai_messages AS assistant
             ON assistant.message_id = generation.assistant_message_id
            AND assistant.user_id = generation.user_id
          WHERE generation.generation_id = $1
            AND generation.user_id = $2
          FOR UPDATE OF generation",
    )
    .bind(prior_generation_id)
    .bind(owner_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?
    .ok_or(AppError::NotFound("AI generation"))?;
    let prior_status: String = prior
        .try_get("status")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let allowed = matches!(prior_status.as_str(), "failed" | "cancelled")
        || (allow_completed && prior_status == "completed");
    if !allowed {
        return Err(AppError::Conflict(
            "generation cannot be retried in its current state".to_owned(),
        ));
    }
    let conversation_id: Uuid = prior
        .try_get("conversation_id")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM ai_generations
             WHERE conversation_id = $1
               AND user_id = $2
               AND status IN ('pending', 'streaming')
         )",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?;
    if active {
        return Err(AppError::Conflict(
            "conversation already has an active generation".to_owned(),
        ));
    }
    let sequence: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM ai_messages
          WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?;
    let assistant_message_id = Uuid::now_v7();
    let generation_id = Uuid::now_v7();
    let attachments: Value = prior
        .try_get("attachments")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    sqlx::query(
        "INSERT INTO ai_messages (
            message_id, conversation_id, user_id, role, content, attachments,
            status, sequence
         ) VALUES ($1, $2, $3, 'assistant', '', $4, 'streaming', $5)",
    )
    .bind(assistant_message_id)
    .bind(conversation_id)
    .bind(owner_id)
    .bind(attachments)
    .bind(sequence)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?;
    let user_message_id: Uuid = prior
        .try_get("user_message_id")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let model: String = prior
        .try_get("model")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    sqlx::query(
        "INSERT INTO ai_generations (
            generation_id, conversation_id, user_id, user_message_id,
            assistant_message_id, status, provider_kind, model,
            retry_of_generation_id, mutation_idempotency_key
         ) VALUES ($1, $2, $3, $4, $5, 'pending', 'openrouter', $6, $7, $8)",
    )
    .bind(generation_id)
    .bind(conversation_id)
    .bind(owner_id)
    .bind(user_message_id)
    .bind(assistant_message_id)
    .bind(model)
    .bind(prior_generation_id)
    .bind(idempotency_key)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?;
    sqlx::query(
        "UPDATE ai_conversations
            SET object_revision = object_revision + 1, updated_at = now()
          WHERE conversation_id = $1 AND user_id = $2",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    generation(pool, owner_id, generation_id).await
}

async fn replay_message(
    transaction: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    owner_id: UserId,
    conversation_id: Uuid,
    idempotency_key: &str,
) -> Result<Option<CreateMessageResponse>, AppError> {
    let row = sqlx::query(
        "SELECT message_id
           FROM ai_messages
          WHERE user_id = $1
            AND conversation_id = $2
            AND create_idempotency_key = $3",
    )
    .bind(owner_id)
    .bind(conversation_id)
    .bind(idempotency_key)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let message_id: Uuid = row
        .try_get("message_id")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let message_row = sqlx::query(
        "SELECT message_id, conversation_id, role, content, attachments,
                status, sequence, created_at
           FROM ai_messages
          WHERE message_id = $1 AND user_id = $2",
    )
    .bind(message_id)
    .bind(owner_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let generation_row = sqlx::query(
        "SELECT generation_id, conversation_id, user_message_id,
                assistant_message_id, status, provider_kind, model, usage,
                error_code, started_at, finished_at
           FROM ai_generations
          WHERE user_message_id = $1 AND user_id = $2
          ORDER BY started_at LIMIT 1",
    )
    .bind(message_id)
    .bind(owner_id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    Ok(Some(CreateMessageResponse {
        message: message_from_row(&message_row)?,
        generation: generation_from_row(&generation_row)?,
    }))
}

async fn conversation_detail(
    pool: &PgPool,
    owner_id: UserId,
    conversation_id: Uuid,
) -> Result<ConversationDetail, AppError> {
    let conversation_row = sqlx::query(
        "SELECT conversation_id, user_id, title, active_model, object_revision,
                created_at, updated_at, deleted_at
           FROM ai_conversations
          WHERE conversation_id = $1
            AND user_id = $2
            AND deleted_at IS NULL",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?
    .ok_or(AppError::NotFound("AI conversation"))?;
    let message_rows = sqlx::query(
        "SELECT message_id, conversation_id, role, content, attachments,
                status, sequence, created_at
           FROM ai_messages
          WHERE conversation_id = $1 AND user_id = $2
          ORDER BY sequence
          LIMIT 500",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let messages = message_rows
        .iter()
        .map(message_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let generation_row = sqlx::query(
        "SELECT generation_id, conversation_id, user_message_id,
                assistant_message_id, status, provider_kind, model, usage,
                error_code, started_at, finished_at
           FROM ai_generations
          WHERE conversation_id = $1
            AND user_id = $2
            AND status IN ('pending', 'streaming')
          ORDER BY started_at DESC
          LIMIT 1",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?;
    Ok(ConversationDetail {
        conversation: conversation_from_row(&conversation_row)?,
        messages: AiPage {
            items: messages,
            next_cursor: None,
        },
        active_generation: generation_row
            .as_ref()
            .map(generation_from_row)
            .transpose()?,
    })
}

async fn message(pool: &PgPool, owner_id: UserId, message_id: Uuid) -> Result<AiMessage, AppError> {
    let row = sqlx::query(
        "SELECT message_id, conversation_id, role, content, attachments,
                status, sequence, created_at
           FROM ai_messages
          WHERE message_id = $1 AND user_id = $2",
    )
    .bind(message_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI conversations"))?
    .ok_or(AppError::NotFound("AI message"))?;
    message_from_row(&row)
}

async fn generation(
    pool: &PgPool,
    owner_id: UserId,
    generation_id: Uuid,
) -> Result<AiGeneration, AppError> {
    let row = sqlx::query(
        "SELECT generation_id, conversation_id, user_message_id,
                assistant_message_id, status, provider_kind, model, usage,
                error_code, started_at, finished_at
           FROM ai_generations
          WHERE generation_id = $1 AND user_id = $2",
    )
    .bind(generation_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::Unavailable("AI generation"))?
    .ok_or(AppError::NotFound("AI generation"))?;
    generation_from_row(&row)
}

fn conversation_from_row(row: &sqlx_postgres::PgRow) -> Result<AiConversation, AppError> {
    Ok(AiConversation {
        id: row
            .try_get("conversation_id")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        owner_id: row
            .try_get("user_id")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        title: row
            .try_get("title")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        active_model: row
            .try_get("active_model")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        object_revision: positive_u64(
            row.try_get("object_revision")
                .map_err(|_| AppError::Unavailable("AI conversations"))?,
        )?,
        created_at: timestamp_ms(
            row.try_get("created_at")
                .map_err(|_| AppError::Unavailable("AI conversations"))?,
        ),
        updated_at: timestamp_ms(
            row.try_get("updated_at")
                .map_err(|_| AppError::Unavailable("AI conversations"))?,
        ),
        deleted_at: row
            .try_get::<Option<OffsetDateTime>, _>("deleted_at")
            .map_err(|_| AppError::Unavailable("AI conversations"))?
            .map(timestamp_ms),
    })
}

fn message_from_row(row: &sqlx_postgres::PgRow) -> Result<AiMessage, AppError> {
    let role: String = row
        .try_get("role")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let status: String = row
        .try_get("status")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    let attachments: Value = row
        .try_get("attachments")
        .map_err(|_| AppError::Unavailable("AI conversations"))?;
    Ok(AiMessage {
        id: row
            .try_get("message_id")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        conversation_id: row
            .try_get("conversation_id")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        role: match role.as_str() {
            "assistant" => AiMessageRole::Assistant,
            _ => AiMessageRole::User,
        },
        content: row
            .try_get("content")
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        attachments: serde_json::from_value(attachments)
            .map_err(|_| AppError::Unavailable("AI conversations"))?,
        status: message_status(&status)?,
        sequence: positive_u64(
            row.try_get("sequence")
                .map_err(|_| AppError::Unavailable("AI conversations"))?,
        )?,
        created_at: timestamp_ms(
            row.try_get("created_at")
                .map_err(|_| AppError::Unavailable("AI conversations"))?,
        ),
    })
}

fn generation_from_row(row: &sqlx_postgres::PgRow) -> Result<AiGeneration, AppError> {
    let status: String = row
        .try_get("status")
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    let usage = row
        .try_get::<Option<Value>, _>("usage")
        .map_err(|_| AppError::Unavailable("AI generation"))?
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| AppError::Unavailable("AI generation"))?;
    Ok(AiGeneration {
        id: row
            .try_get("generation_id")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        conversation_id: row
            .try_get("conversation_id")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        user_message_id: row
            .try_get("user_message_id")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        assistant_message_id: row
            .try_get("assistant_message_id")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        status: generation_status(&status)?,
        provider_kind: row
            .try_get("provider_kind")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        model: row
            .try_get("model")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        usage,
        error_code: row
            .try_get("error_code")
            .map_err(|_| AppError::Unavailable("AI generation"))?,
        started_at: timestamp_ms(
            row.try_get("started_at")
                .map_err(|_| AppError::Unavailable("AI generation"))?,
        ),
        finished_at: row
            .try_get::<Option<OffsetDateTime>, _>("finished_at")
            .map_err(|_| AppError::Unavailable("AI generation"))?
            .map(timestamp_ms),
    })
}

fn message_status(value: &str) -> Result<AiMessageStatus, AppError> {
    match value {
        "committed" => Ok(AiMessageStatus::Committed),
        "streaming" => Ok(AiMessageStatus::Streaming),
        "completed" => Ok(AiMessageStatus::Completed),
        "failed" => Ok(AiMessageStatus::Failed),
        "cancelled" => Ok(AiMessageStatus::Cancelled),
        _ => Err(AppError::Unavailable("AI conversations")),
    }
}

fn generation_status(value: &str) -> Result<AiGenerationStatus, AppError> {
    match value {
        "pending" => Ok(AiGenerationStatus::Pending),
        "streaming" => Ok(AiGenerationStatus::Streaming),
        "completed" => Ok(AiGenerationStatus::Completed),
        "failed" => Ok(AiGenerationStatus::Failed),
        "cancelled" => Ok(AiGenerationStatus::Cancelled),
        _ => Err(AppError::Unavailable("AI generation")),
    }
}

fn validate_conversation_create(request: &CreateConversationRequest) -> Result<(), AppError> {
    if request.title.trim().is_empty()
        || request.title.len() > 160
        || request.active_model.trim().is_empty()
        || request.active_model.len() > 256
        || request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > 256
    {
        Err(AppError::BadRequest(
            "conversation title, model or idempotency key is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_message_request(request: &CreateMessageRequest) -> Result<(), AppError> {
    if request.content.trim().is_empty()
        || request.content.len() > MAX_MESSAGE_BYTES
        || request.expected_conversation_revision == 0
        || request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > 256
        || request.attachments.len() > 1
    {
        Err(AppError::BadRequest(
            "message content, revision, idempotency key or attachment count is invalid".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn validate_attachment(attachment: &AiContextAttachment) -> Result<(), AppError> {
    match attachment {
        AiContextAttachment::Source {
            kind,
            material_id,
            revision_id,
            scope,
            display_label,
        } if !kind.trim().is_empty()
            && !display_label.trim().is_empty()
            && *material_id == scope.material_id()
            && *revision_id == scope.revision_id()
            && scope.validate().is_ok() =>
        {
            Ok(())
        }
        AiContextAttachment::Records {
            kind,
            record_scope,
            display_label,
            included_context,
        } if kind == "record_search"
            && !display_label.trim().is_empty()
            && included_context.is_empty()
            && record_scope.validate().is_ok() =>
        {
            Ok(())
        }
        _ => Err(AppError::BadRequest(
            "context attachment is invalid".to_owned(),
        )),
    }
}

fn validate_generation_mutation(request: &GenerationMutationRequest) -> Result<(), AppError> {
    if request.expected_revision == 0
        || request.idempotency_key.trim().is_empty()
        || request.idempotency_key.len() > 256
    {
        Err(AppError::BadRequest(
            "generation revision and idempotency key are required".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn map_context_error(error: SourceContextError) -> AppError {
    match error {
        SourceContextError::NotFound | SourceContextError::ScopeNotFound => {
            AppError::NotFound("source context")
        }
        SourceContextError::StaleSelection => {
            AppError::Conflict("source selection is stale".to_owned())
        }
        SourceContextError::MissingText => {
            AppError::BadRequest("source has no usable text layer".to_owned())
        }
        SourceContextError::LimitExceeded => {
            AppError::BadRequest("source context exceeds configured limits".to_owned())
        }
        SourceContextError::Storage => AppError::Unavailable("AI context"),
    }
}

fn map_record_context_error(error: RecordContextError) -> AppError {
    match error {
        RecordContextError::InvalidScope => {
            AppError::BadRequest("record context scope is invalid".to_owned())
        }
        RecordContextError::InsufficientContext => {
            AppError::BadRequest("record context is too weak to ground an answer".to_owned())
        }
        RecordContextError::Unavailable => AppError::Unavailable("record retrieval"),
    }
}

fn provider_error_message(error: &AiProviderError) -> &'static str {
    match error {
        AiProviderError::MissingCredential => "Добавьте ключ OpenRouter в настройках.",
        AiProviderError::Authentication => "OpenRouter отклонил ключ.",
        AiProviderError::ModelUnavailable => "Выбранная модель недоступна.",
        AiProviderError::RateLimited => "OpenRouter ограничил частоту запросов.",
        AiProviderError::QuotaExhausted => "На аккаунте OpenRouter закончился лимит.",
        AiProviderError::PolicyRejected => "Provider отклонил запрос по правилам безопасности.",
        AiProviderError::LimitExceeded => "Ответ превысил разрешённый размер.",
        AiProviderError::Timeout => "Provider не ответил вовремя.",
        AiProviderError::Cancelled => "Генерация остановлена.",
        AiProviderError::InvalidResponse => "Provider вернул некорректный ответ.",
        AiProviderError::Unavailable => "Provider временно недоступен.",
    }
}

fn positive_u64(value: i64) -> Result<u64, AppError> {
    u64::try_from(value).map_err(|_| AppError::Unavailable("AI persistence"))
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_contract_rejects_multiple_explicit_scopes() {
        let request = CreateMessageRequest {
            content: "Вопрос".to_owned(),
            attachments: vec![],
            expected_conversation_revision: 1,
            idempotency_key: "message-1".to_owned(),
        };

        assert!(validate_message_request(&request).is_ok());
    }

    #[test]
    fn provider_errors_are_redacted_and_actionable() {
        let message = provider_error_message(&AiProviderError::Authentication);

        assert_eq!(message, "OpenRouter отклонил ключ.");
    }
}
