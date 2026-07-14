//! Axum account/auth routes and session security middleware.

use axum::{
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
    Extension, Json, Router,
};
use lumi_core::{
    decode_auth_bytes, AccountSummary, CompleteLoginRequest, CreateChallengeRequest, DeviceSummary,
    RegisterAccountRequest, SessionBootstrap, UpdateAccountProfileRequest,
};
use uuid::Uuid;

use crate::{
    account::{decode_registration, hash_token, AccountStoreError, AuthenticatedSession},
    AppError, AppState,
};

pub(crate) fn public_routes() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/challenges", post(create_challenge))
        .route("/auth/login", post(login))
        .route("/auth/recovery/challenges", post(create_challenge))
        .route("/auth/recovery", post(login))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

pub(crate) fn protected_account_routes() -> Router<AppState> {
    Router::new()
        .route("/account/me", get(account_me))
        .route("/account/profile", patch(update_profile))
        .route("/auth/logout", post(logout))
        .route("/auth/sessions/revoke-all", post(revoke_all_sessions))
        .route("/devices", get(list_devices))
        .route("/devices/{device_id}", delete(revoke_device))
        .layer(DefaultBodyLimit::max(64 * 1024))
}

pub(crate) async fn require_session(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, AppError> {
    let token = cookie_value(request.headers(), state.security().cookie_name())
        .ok_or(AppError::Unauthorized)?;
    let session = state
        .accounts()
        .authenticate(hash_token(token))
        .await
        .map_err(map_auth_error)?;
    if is_mutating(request.method()) {
        validate_same_origin(&state, request.headers())?;
        let csrf = request
            .headers()
            .get("x-lumi-csrf")
            .and_then(|value| value.to_str().ok())
            .ok_or(AppError::Forbidden("missing CSRF token"))?;
        if hash_token(csrf) != session.csrf_hash {
            return Err(AppError::Forbidden("invalid CSRF token"));
        }
    }
    request.extensions_mut().insert(session);
    Ok(next.run(request).await)
}

async fn register(
    State(state): State<AppState>,
    Json(request): Json<RegisterAccountRequest>,
) -> Result<Response, AppError> {
    validate_registration(&request)?;
    let (lookup_id, public_key) = decode_registration(&request).map_err(map_auth_error)?;
    let created = state
        .accounts()
        .register(&request, lookup_id, public_key)
        .await
        .map_err(map_auth_error)?;
    session_response(&state, created.bootstrap, &created.token)
}

async fn create_challenge(
    State(state): State<AppState>,
    Json(request): Json<CreateChallengeRequest>,
) -> Result<Json<lumi_core::ChallengeResponse>, AppError> {
    let lookup_id = decode_auth_bytes(&request.lookup_id)
        .map_err(|_| AppError::BadRequest("lookup_id must be 32-byte base64url".to_owned()))?;
    let challenge = state
        .accounts()
        .issue_challenge(lookup_id, state.security().audience())
        .await
        .map_err(map_auth_error)?;
    Ok(Json(challenge))
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<CompleteLoginRequest>,
) -> Result<Response, AppError> {
    let signature = decode_auth_bytes(&request.signature).map_err(|_| AppError::Unauthorized)?;
    let created = state
        .accounts()
        .login(&request, signature)
        .await
        .map_err(map_auth_error)?;
    session_response(&state, created.bootstrap, &created.token)
}

async fn account_me(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<AccountSummary>, AppError> {
    state
        .accounts()
        .account(session.user_id)
        .await
        .map(Json)
        .map_err(map_auth_error)
}

async fn update_profile(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<UpdateAccountProfileRequest>,
) -> Result<Json<AccountSummary>, AppError> {
    if request.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "idempotency_key must not be empty".to_owned(),
        ));
    }
    state
        .accounts()
        .update_profile(&session, &request)
        .await
        .map(Json)
        .map_err(map_auth_error)
}

async fn logout(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Response, AppError> {
    state
        .accounts()
        .revoke_session(session.user_id, session.session_id)
        .await
        .map_err(map_auth_error)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.security().expired_cookie())
            .map_err(|_| AppError::Internal("failed to build session cookie"))?,
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.security().expired_csrf_cookie())
            .map_err(|_| AppError::Internal("failed to build CSRF cookie"))?,
    );
    Ok((headers, StatusCode::NO_CONTENT).into_response())
}

async fn list_devices(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<DeviceSummary>>, AppError> {
    state
        .accounts()
        .list_devices(session.user_id)
        .await
        .map(Json)
        .map_err(map_auth_error)
}

async fn revoke_device(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(device_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    state
        .accounts()
        .revoke_device(session.user_id, device_id)
        .await
        .map_err(map_auth_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn revoke_all_sessions(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Response, AppError> {
    state
        .accounts()
        .revoke_all_sessions(session.user_id)
        .await
        .map_err(map_auth_error)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.security().expired_cookie())
            .map_err(|_| AppError::Internal("failed to build session cookie"))?,
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.security().expired_csrf_cookie())
            .map_err(|_| AppError::Internal("failed to build CSRF cookie"))?,
    );
    Ok((headers, StatusCode::NO_CONTENT).into_response())
}

fn validate_registration(request: &RegisterAccountRequest) -> Result<(), AppError> {
    if request.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "idempotency_key must not be empty".to_owned(),
        ));
    }
    if request
        .nickname
        .as_deref()
        .is_some_and(|value| value.chars().count() > 80)
    {
        return Err(AppError::BadRequest(
            "nickname must contain at most 80 characters".to_owned(),
        ));
    }
    Ok(())
}

fn session_response(
    state: &AppState,
    bootstrap: SessionBootstrap,
    token: &str,
) -> Result<Response, AppError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.security().session_cookie(token))
            .map_err(|_| AppError::Internal("failed to build session cookie"))?,
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&state.security().csrf_cookie(&bootstrap.csrf_token))
            .map_err(|_| AppError::Internal("failed to build CSRF cookie"))?,
    );
    Ok((headers, Json(bootstrap)).into_response())
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

fn validate_same_origin(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .or_else(|| {
            headers
                .get(header::REFERER)
                .and_then(|value| value.to_str().ok())
        })
        .ok_or(AppError::Forbidden("missing Origin or Referer"))?;
    let allowed_origin: &str = state.security().allowed_origin();
    if origin == allowed_origin
        || origin
            .strip_prefix(allowed_origin)
            .is_some_and(|suffix| suffix.starts_with('/'))
    {
        Ok(())
    } else {
        Err(AppError::Forbidden("cross-origin mutation rejected"))
    }
}

fn is_mutating(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn map_auth_error(error: AccountStoreError) -> AppError {
    match error {
        AccountStoreError::Conflict => AppError::Conflict("account command conflicts".to_owned()),
        AccountStoreError::NotFound | AccountStoreError::InvalidProof => AppError::Unauthorized,
        AccountStoreError::RevisionConflict => {
            AppError::Conflict("object revision conflict".to_owned())
        }
        AccountStoreError::Unavailable => AppError::Unavailable("account repository"),
    }
}
