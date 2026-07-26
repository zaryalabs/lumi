//! Versioned Axum routes for deterministic learning operations.

use super::repository::{LearningStoreError, SessionTransition};
use crate::{account::AuthenticatedSession, required_idempotency_key, AppError, AppState};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::HeaderMap,
    routing::{get, patch, post},
    Extension, Json, Router,
};
use lumi_core::{
    ChangeLearningItemStatusCommand, CompleteReadingResponse, CompleteReadingScopeCommand,
    CreateLearningItemCommand, CreateLearningSessionCommand, LearningAttempt, LearningItem,
    LearningItemId, LearningItemPage, LearningItemStatus, LearningOffer, LearningSession,
    LearningSessionId, LearningSourceId, MaterialId, RecordLearningSourceOpenedCommand,
    SubmitLearningAttemptCommand, UpdateLearningItemCommand, UpdateLearningOfferCommand,
};
use serde::Deserialize;

/// Return all authenticated learning routes.
pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/materials/{material_id}/reading-completions",
            post(complete_reading),
        )
        .route("/materials/{material_id}/learning-offer", get(get_offer))
        .route(
            "/materials/{material_id}/learning-settings",
            patch(update_offer),
        )
        .route("/learning/items", get(list_items).post(create_item))
        .route(
            "/learning/items/{item_id}",
            get(get_item).patch(update_item),
        )
        .route("/learning/items/{item_id}/activate", post(activate_item))
        .route("/learning/items/{item_id}/archive", post(archive_item))
        .route("/learning/sessions", post(create_session))
        .route("/learning/sessions/{session_id}", get(get_session))
        .route("/learning/sessions/{session_id}/start", post(start_session))
        .route(
            "/learning/sessions/{session_id}/items/{item_id}/source-opened",
            post(source_opened),
        )
        .route(
            "/learning/sessions/{session_id}/items/{item_id}/attempts",
            post(submit_attempt),
        )
        .route(
            "/learning/sessions/{session_id}/complete",
            post(complete_session),
        )
        .route(
            "/learning/sessions/{session_id}/abandon",
            post(abandon_session),
        )
        .layer(DefaultBodyLimit::max(512 * 1024))
}

#[derive(Deserialize)]
struct OfferQuery {
    source_id: LearningSourceId,
}

#[derive(Deserialize)]
struct ItemListQuery {
    material_id: Option<MaterialId>,
    source_id: Option<LearningSourceId>,
    status: Option<LearningItemStatus>,
    cursor: Option<LearningItemId>,
    limit: Option<usize>,
}

async fn complete_reading(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    headers: HeaderMap,
    Json(command): Json<CompleteReadingScopeCommand>,
) -> Result<Json<CompleteReadingResponse>, AppError> {
    if command.material_id != material_id {
        return Err(AppError::BadRequest(
            "material id in path and body must match".to_owned(),
        ));
    }
    let command_key = required_idempotency_key(&headers)?;
    let context = state.learning_material_context(session.user_id, &command)?;
    state
        .learning_runtime()
        .complete_reading(
            session.user_id,
            session.device_id,
            context,
            &command,
            command_key,
        )
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn get_offer(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    Query(query): Query<OfferQuery>,
) -> Result<Json<LearningOffer>, AppError> {
    state
        .learning_runtime()
        .offer(session.user_id, material_id, query.source_id)
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn update_offer(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<MaterialId>,
    headers: HeaderMap,
    Json(command): Json<UpdateLearningOfferCommand>,
) -> Result<Json<LearningOffer>, AppError> {
    let command_key = required_idempotency_key(&headers)?;
    state
        .learning_runtime()
        .update_offer(session.user_id, material_id, &command, command_key)
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn list_items(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(query): Query<ItemListQuery>,
) -> Result<Json<LearningItemPage>, AppError> {
    state
        .learning_runtime()
        .list_items(
            session.user_id,
            query.material_id,
            query.source_id,
            query.status,
            query.cursor,
            query.limit.unwrap_or(50),
        )
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn create_item(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    Json(command): Json<CreateLearningItemCommand>,
) -> Result<Json<LearningItem>, AppError> {
    let command_key = required_idempotency_key(&headers)?;
    state
        .learning_runtime()
        .create_item(session.user_id, session.device_id, &command, command_key)
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn get_item(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(item_id): Path<LearningItemId>,
) -> Result<Json<LearningItem>, AppError> {
    state
        .learning_runtime()
        .get_item(session.user_id, item_id)
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn update_item(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(item_id): Path<LearningItemId>,
    headers: HeaderMap,
    Json(command): Json<UpdateLearningItemCommand>,
) -> Result<Json<LearningItem>, AppError> {
    let command_key = required_idempotency_key(&headers)?;
    state
        .learning_runtime()
        .update_item(
            session.user_id,
            session.device_id,
            item_id,
            &command,
            command_key,
        )
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn activate_item(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(item_id): Path<LearningItemId>,
    headers: HeaderMap,
    Json(command): Json<ChangeLearningItemStatusCommand>,
) -> Result<Json<LearningItem>, AppError> {
    change_item_status(
        &state,
        &session,
        item_id,
        LearningItemStatus::Active,
        command,
        &headers,
    )
    .await
    .map(Json)
}

async fn archive_item(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(item_id): Path<LearningItemId>,
    headers: HeaderMap,
    Json(command): Json<ChangeLearningItemStatusCommand>,
) -> Result<Json<LearningItem>, AppError> {
    change_item_status(
        &state,
        &session,
        item_id,
        LearningItemStatus::Archived,
        command,
        &headers,
    )
    .await
    .map(Json)
}

async fn change_item_status(
    state: &AppState,
    session: &AuthenticatedSession,
    item_id: LearningItemId,
    status: LearningItemStatus,
    command: ChangeLearningItemStatusCommand,
    headers: &HeaderMap,
) -> Result<LearningItem, AppError> {
    let command_key = required_idempotency_key(headers)?;
    state
        .learning_runtime()
        .change_item_status(
            session.user_id,
            session.device_id,
            item_id,
            status,
            command,
            command_key,
        )
        .await
        .map_err(map_learning_error)
}

async fn create_session(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    Json(command): Json<CreateLearningSessionCommand>,
) -> Result<Json<LearningSession>, AppError> {
    let command_key = required_idempotency_key(&headers)?;
    state
        .learning_runtime()
        .create_session(session.user_id, session.device_id, command, command_key)
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn get_session(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(session_id): Path<LearningSessionId>,
) -> Result<Json<LearningSession>, AppError> {
    state
        .learning_runtime()
        .get_session(session.user_id, session_id)
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn start_session(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(session_id): Path<LearningSessionId>,
    headers: HeaderMap,
) -> Result<Json<LearningSession>, AppError> {
    transition_session(
        &state,
        &session,
        session_id,
        SessionTransition::Start,
        &headers,
    )
    .await
    .map(Json)
}

async fn complete_session(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(session_id): Path<LearningSessionId>,
    headers: HeaderMap,
) -> Result<Json<LearningSession>, AppError> {
    transition_session(
        &state,
        &session,
        session_id,
        SessionTransition::Complete,
        &headers,
    )
    .await
    .map(Json)
}

async fn abandon_session(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(session_id): Path<LearningSessionId>,
    headers: HeaderMap,
) -> Result<Json<LearningSession>, AppError> {
    transition_session(
        &state,
        &session,
        session_id,
        SessionTransition::Abandon,
        &headers,
    )
    .await
    .map(Json)
}

async fn transition_session(
    state: &AppState,
    session: &AuthenticatedSession,
    session_id: LearningSessionId,
    transition: SessionTransition,
    headers: &HeaderMap,
) -> Result<LearningSession, AppError> {
    let command_key = required_idempotency_key(headers)?;
    state
        .learning_runtime()
        .transition_session(
            session.user_id,
            session.device_id,
            session_id,
            transition,
            command_key,
        )
        .await
        .map_err(map_learning_error)
}

async fn source_opened(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((session_id, item_id)): Path<(LearningSessionId, LearningItemId)>,
    headers: HeaderMap,
) -> Result<Json<LearningSession>, AppError> {
    let command_key = required_idempotency_key(&headers)?;
    state
        .learning_runtime()
        .source_opened(
            session.user_id,
            session_id,
            RecordLearningSourceOpenedCommand { item_id },
            command_key,
        )
        .await
        .map(Json)
        .map_err(map_learning_error)
}

async fn submit_attempt(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((session_id, item_id)): Path<(LearningSessionId, LearningItemId)>,
    headers: HeaderMap,
    Json(command): Json<SubmitLearningAttemptCommand>,
) -> Result<Json<LearningAttempt>, AppError> {
    let command_key = required_idempotency_key(&headers)?;
    state
        .learning_runtime()
        .submit_attempt(
            session.user_id,
            session.device_id,
            session_id,
            item_id,
            &command,
            command_key,
        )
        .await
        .map(Json)
        .map_err(map_learning_error)
}

fn map_learning_error(error: LearningStoreError) -> AppError {
    match error {
        LearningStoreError::NotFound => AppError::NotFound("learning object"),
        LearningStoreError::Conflict => {
            AppError::Conflict("learning command conflicts with current state".to_owned())
        }
        LearningStoreError::Invalid(error) => AppError::BadRequest(error.to_string()),
        LearningStoreError::InvalidDetail(detail) => AppError::BadRequest(detail),
        LearningStoreError::Unavailable => AppError::Unavailable("learning repository"),
    }
}
