//! Typed Axum routes for Community Spaces and revocable access links.

use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use lumi_core::{
    ClaimSharedMaterialRequest, CommunityAccessLink, CommunityAccessLinkId, CommunityLinkPreview,
    CommunityMembership, CommunitySpace, CommunitySpaceDetail, CommunitySpaceId,
    CreateCommunityAccessLinkRequest, CreateCommunitySpaceRequest, CreateSharedCommentRequest,
    CreateSharedThreadRequest, CreatedCommunityAccessLink, DeleteSharedCommentRequest,
    JoinCommunityLinkRequest, ModerateSocialContentRequest, ModerationAction,
    PreviewCommunityLinkRequest, ShareMaterialRequest, SharedComment, SharedCommentId,
    SharedCommentThread, SharedCommentThreadId, SharedDiscussionPage, SharedMaterial,
    SharedMaterialId, UpdateCommunityMemberRequest, UpdateCommunitySpaceRequest,
    UpdateSharedCommentRequest, UserId, UserMaterialClaim,
};
use serde::Deserialize;

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
        .route("/spaces/{space_id}/materials", get(list_materials))
        .route("/spaces/{space_id}/materials/share", post(share_material))
        .route(
            "/spaces/{space_id}/materials/{shared_material_id}",
            get(get_material).delete(delete_material),
        )
        .route(
            "/spaces/{space_id}/materials/{shared_material_id}/claim",
            get(get_claim).post(claim_material),
        )
        .route(
            "/spaces/{space_id}/materials/{shared_material_id}/claim/recheck",
            post(recheck_material),
        )
        .route(
            "/spaces/{space_id}/materials/{shared_material_id}/threads",
            get(list_discussions).post(create_thread),
        )
        .route(
            "/spaces/{space_id}/threads/{thread_id}/comments",
            post(add_comment),
        )
        .route(
            "/spaces/{space_id}/comments/{comment_id}",
            patch(update_comment).delete(delete_comment),
        )
        .route(
            "/spaces/{space_id}/moderation/actions",
            post(moderate_content),
        )
        .route("/shares/community-link/join", post(join_link))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

#[derive(Deserialize)]
struct DiscussionQuery {
    after: Option<String>,
    limit: Option<u16>,
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

async fn list_materials(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
) -> Result<Json<Vec<SharedMaterial>>, AppError> {
    state
        .social_runtime()
        .list_materials(session.user_id, space_id)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn share_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
    headers: HeaderMap,
    Json(request): Json<ShareMaterialRequest>,
) -> Result<Json<SharedMaterial>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .share_material(
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

async fn get_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
) -> Result<Json<SharedMaterial>, AppError> {
    state
        .social_runtime()
        .material(session.user_id, space_id, shared_material_id)
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn delete_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .delete_material(
            session.user_id,
            session.device_id,
            space_id,
            shared_material_id,
            idempotency_key,
        )
        .await
        .map_err(map_social_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn get_claim(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
) -> Result<Json<UserMaterialClaim>, AppError> {
    state
        .social_runtime()
        .material(session.user_id, space_id, shared_material_id)
        .await
        .map_err(map_social_error)?
        .claim
        .map(Json)
        .ok_or(AppError::NotFound("material claim"))
}

async fn claim_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
    headers: HeaderMap,
    Json(request): Json<ClaimSharedMaterialRequest>,
) -> Result<Json<SharedMaterial>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .claim_material(
            session.user_id,
            session.device_id,
            space_id,
            shared_material_id,
            idempotency_key,
            request,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn recheck_material(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
    headers: HeaderMap,
) -> Result<Json<SharedMaterial>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .recheck_material(
            session.user_id,
            session.device_id,
            space_id,
            shared_material_id,
            idempotency_key,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn list_discussions(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
    Query(query): Query<DiscussionQuery>,
) -> Result<Json<SharedDiscussionPage>, AppError> {
    state
        .social_runtime()
        .list_discussions(
            session.user_id,
            space_id,
            shared_material_id,
            query.after.as_deref(),
            query.limit.unwrap_or(50),
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn create_thread(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, shared_material_id)): Path<(CommunitySpaceId, SharedMaterialId)>,
    headers: HeaderMap,
    Json(request): Json<CreateSharedThreadRequest>,
) -> Result<(StatusCode, Json<SharedCommentThread>), AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    let thread = state
        .social_runtime()
        .create_thread(
            session.user_id,
            session.device_id,
            space_id,
            shared_material_id,
            idempotency_key,
            request,
        )
        .await
        .map_err(map_social_error)?;
    Ok((StatusCode::CREATED, Json(thread)))
}

async fn add_comment(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, thread_id)): Path<(CommunitySpaceId, SharedCommentThreadId)>,
    headers: HeaderMap,
    Json(request): Json<CreateSharedCommentRequest>,
) -> Result<(StatusCode, Json<SharedComment>), AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    let comment = state
        .social_runtime()
        .add_comment(
            session.user_id,
            session.device_id,
            space_id,
            thread_id,
            idempotency_key,
            request,
        )
        .await
        .map_err(map_social_error)?;
    Ok((StatusCode::CREATED, Json(comment)))
}

async fn update_comment(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, comment_id)): Path<(CommunitySpaceId, SharedCommentId)>,
    headers: HeaderMap,
    Json(request): Json<UpdateSharedCommentRequest>,
) -> Result<Json<SharedComment>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .update_comment(
            session.user_id,
            session.device_id,
            space_id,
            comment_id,
            idempotency_key,
            request,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn delete_comment(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((space_id, comment_id)): Path<(CommunitySpaceId, SharedCommentId)>,
    headers: HeaderMap,
    Json(request): Json<DeleteSharedCommentRequest>,
) -> Result<Json<SharedComment>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .delete_comment(
            session.user_id,
            session.device_id,
            space_id,
            comment_id,
            idempotency_key,
            request,
        )
        .await
        .map(Json)
        .map_err(map_social_error)
}

async fn moderate_content(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(space_id): Path<CommunitySpaceId>,
    headers: HeaderMap,
    Json(request): Json<ModerateSocialContentRequest>,
) -> Result<Json<ModerationAction>, AppError> {
    let idempotency_key = required_idempotency_key(&headers)?;
    state
        .social_runtime()
        .moderate_content(
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
