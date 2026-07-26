//! Typed Axum routes for Community Spaces and revocable access links.

use axum::{
    extract::{DefaultBodyLimit, Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use lumi_core::{
    CommunityAccessLink, CommunityAccessLinkId, CommunityLinkPreview, CommunityMembership,
    CommunitySpace, CommunitySpaceDetail, CommunitySpaceId, CreateCommunityAccessLinkRequest,
    CreateCommunitySpaceRequest, CreatedCommunityAccessLink, JoinCommunityLinkRequest,
    PreviewCommunityLinkRequest, UpdateCommunityMemberRequest, UpdateCommunitySpaceRequest, UserId,
};

use crate::{account::AuthenticatedSession, required_idempotency_key, AppError, AppState};

use super::SocialStoreError;

pub(crate) fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/shares/community-link/preview", post(preview_link))
        .layer(DefaultBodyLimit::max(8 * 1024))
}

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/spaces", get(list_spaces).post(create_space))
        .route(
            "/spaces/{space_id}",
            get(get_space).patch(update_space).delete(delete_space),
        )
        .route("/spaces/{space_id}/members", get(list_members))
        .route(
            "/spaces/{space_id}/members/{user_id}",
            patch(update_member).delete(remove_member),
        )
        .route("/spaces/{space_id}/leave", post(leave_space))
        .route(
            "/spaces/{space_id}/access-links",
            get(list_links).post(create_link),
        )
        .route(
            "/spaces/{space_id}/access-links/{link_id}",
            delete(revoke_link),
        )
        .route(
            "/spaces/{space_id}/access-links/{link_id}/rotate",
            post(rotate_link),
        )
        .route("/shares/community-link/join", post(join_link))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

async fn preview_link(
    State(state): State<AppState>,
    Json(request): Json<PreviewCommunityLinkRequest>,
) -> Result<Json<CommunityLinkPreview>, AppError> {
    state
        .social_runtime()
        .preview(request)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn list_spaces(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<CommunitySpace>>, AppError> {
    state
        .social_runtime()
        .list(session.user_id)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn create_space(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    Json(request): Json<CreateCommunitySpaceRequest>,
) -> Result<(StatusCode, Json<CommunitySpaceDetail>), AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    let detail = state
        .social_runtime()
        .create(session.user_id, session.device_id, idempotency_key, request)
        .await
        .map_err(map_social_error)?;
    Ok((StatusCode::CREATED, Json(detail)))
}

async fn get_space(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
) -> Result<Json<CommunitySpaceDetail>, AppError> {
    state
        .social_runtime()
        .detail(session.user_id, space_id)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn update_space(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
    headers: HeaderMap,
    Json(request): Json<UpdateCommunitySpaceRequest>,
) -> Result<Json<CommunitySpaceDetail>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .update(
            session.user_id,
            session.device_id,
            space_id,
            idempotency_key,
            request,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn delete_space(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .delete(
            session.user_id,
            session.device_id,
            space_id,
            idempotency_key,
        )
        .await
        .map_err(map_social_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_members(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
) -> Result<Json<Vec<CommunityMembership>>, AppError> {
    state
        .social_runtime()
        .members(session.user_id, space_id)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn update_member(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, user_id)): Path<(CommunitySpaceId, UserId)>,
    headers: HeaderMap,
    Json(request): Json<UpdateCommunityMemberRequest>,
) -> Result<Json<CommunityMembership>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .update_member(
            session.user_id,
            session.device_id,
            space_id,
            user_id,
            idempotency_key,
            request,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn remove_member(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, user_id)): Path<(CommunitySpaceId, UserId)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .remove_member(
            session.user_id,
            session.device_id,
            space_id,
            user_id,
            idempotency_key,
        )
        .await
        .map_err(map_social_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn leave_space(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .leave(
            session.user_id,
            session.device_id,
            space_id,
            idempotency_key,
        )
        .await
        .map_err(map_social_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_links(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
) -> Result<Json<Vec<CommunityAccessLink>>, AppError> {
    state
        .social_runtime()
        .links(session.user_id, space_id)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn create_link(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
    headers: HeaderMap,
    Json(request): Json<CreateCommunityAccessLinkRequest>,
) -> Result<(StatusCode, Json<CreatedCommunityAccessLink>), AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    let link = state
        .social_runtime()
        .create_link(
            session.user_id,
            session.device_id,
            space_id,
            idempotency_key,
            request,
        )
        .await
        .map_err(map_social_error)?;
    Ok((StatusCode::CREATED, Json(link)))
}

async fn revoke_link(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, link_id)): Path<(CommunitySpaceId, CommunityAccessLinkId)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .revoke_link(
            session.user_id,
            session.device_id,
            space_id,
            link_id,
            idempotency_key,
        )
        .await
        .map_err(map_social_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn rotate_link(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, link_id)): Path<(CommunitySpaceId, CommunityAccessLinkId)>,
    headers: HeaderMap,
) -> Result<Json<CreatedCommunityAccessLink>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .rotate_link(
            session.user_id,
            session.device_id,
            space_id,
            link_id,
            idempotency_key,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn join_link(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    headers: HeaderMap,
    Json(request): Json<JoinCommunityLinkRequest>,
) -> Result<Json<CommunitySpaceDetail>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .join(session.user_id, session.device_id, idempotency_key, request)
        .await
        .map(Json)
        .map_err(map_social_error)
}

fn map_social_error(error: SocialStoreError) -> AppError {
    match error {
        SocialStoreError::NotFound => AppError::NotFound("community object"),
        SocialStoreError::Forbidden => AppError::Forbidden("community action is forbidden"),
        SocialStoreError::Conflict => {
            AppError::Conflict("community command conflicts with current state".to_owned())
        }
        SocialStoreError::Invalid(detail) => AppError::Unprocessable(detail),
        SocialStoreError::RateLimited => {
            AppError::TooManyRequests("community request rate exceeded")
        }
        SocialStoreError::Unavailable => AppError::Unavailable("community repository"),
    }
}
