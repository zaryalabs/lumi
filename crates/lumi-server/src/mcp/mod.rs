//! Account-scoped MCP connection management and Streamable HTTP transport.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use axum::{
    extract::{DefaultBodyLimit, Extension, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use lumi_core::{
    mcp_tool_schema_contracts, AiExecutorKind, AiPage, AiTaskClaim, AiTaskClaimFence,
    CompleteAiTaskRequest, CreateFlashcardTaskRequest, CreateMcpConnectionRequest,
    CreateSharedChatMessageRequest, CreateSharedCommentRequest, CreateSharedThreadRequest,
    DeleteSharedCommentRequest, GenerateLearningItemsRequest, ListLearningItemsRequest,
    McpAiTaskProgressRequest, McpConnection, McpConnectionId, McpConnectionStatus,
    McpConnectionTokenResponse, RevokeMcpConnectionRequest, RotateMcpConnectionRequest,
    ShareMaterialRequest, SubmitLearningAnswerRequest, SubmitLearningAttemptCommand,
    UpdateSharedCommentRequest, UserId, MCP_CONTROL_REQUEST_MAX_BYTES, MCP_INLINE_RESULT_MAX_BYTES,
    MCP_LIST_PAGE_MAX_ITEMS, MCP_PROTOCOL_VERSION, MCP_TOOL_CONTRACT_VERSION,
};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx_core::row::Row;
use sqlx_postgres::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::account::AuthenticatedSession;
use crate::{AppError, AppState};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_CONNECTIONS_PER_ACCOUNT: i64 = 20;
const MCP_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct StoredConnection {
    projection: McpConnection,
    owner_id: UserId,
    device_id: Uuid,
    verifier: [u8; 32],
}

#[derive(Default)]
struct MemoryConnections {
    by_id: HashMap<McpConnectionId, StoredConnection>,
    delete_challenges: HashMap<[u8; 32], MemoryDeleteChallenge>,
}

struct MemoryDeleteChallenge {
    owner_id: UserId,
    connection_id: Uuid,
    material_id: Uuid,
    expires_at: u64,
    consumed: bool,
}

/// Durable or deterministic in-memory MCP connection repository.
pub(crate) struct McpRuntime {
    pool: Option<PgPool>,
    memory: Mutex<MemoryConnections>,
}

impl McpRuntime {
    pub(crate) fn memory() -> Self {
        Self {
            pool: None,
            memory: Mutex::new(MemoryConnections::default()),
        }
    }

    pub(crate) fn postgres(pool: PgPool) -> Self {
        Self {
            pool: Some(pool),
            memory: Mutex::new(MemoryConnections::default()),
        }
    }

    async fn create(
        &self,
        owner_id: UserId,
        request: &CreateMcpConnectionRequest,
    ) -> Result<McpConnectionTokenResponse, McpError> {
        validate_name_and_key(&request.name, &request.idempotency_key)?;
        let token = random_token();
        let verifier = token_verifier(&token);
        let id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let projection = McpConnection {
            id,
            name: request.name.trim().to_owned(),
            token_fingerprint: token_fingerprint(&token),
            status: McpConnectionStatus::Active,
            object_revision: 1,
            created_at: now_ms(),
            last_used_at: None,
            revoked_at: None,
        };
        if let Some(pool) = &self.pool {
            let mut transaction = pool.begin().await.map_err(storage)?;
            let count: i64 = sqlx::query(
                "SELECT count(*) AS count FROM mcp_connections WHERE user_id = $1 AND revoked_at IS NULL",
            )
            .bind(owner_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage)?
            .try_get("count")
            .map_err(storage)?;
            if count >= MAX_CONNECTIONS_PER_ACCOUNT {
                return Err(McpError::Limit);
            }
            let request_hash = hash_json(&json!({"name": projection.name}))?;
            let replay = sqlx::query(
                "SELECT connection_id, request_hash FROM mcp_connection_mutations WHERE user_id = $1 AND idempotency_key = $2",
            )
            .bind(owner_id)
            .bind(&request.idempotency_key)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(storage)?;
            if let Some(row) = replay {
                let same_hash: String = row.try_get("request_hash").map_err(storage)?;
                return Err(if same_hash == request_hash {
                    McpError::TokenAlreadyShown
                } else {
                    McpError::Conflict
                });
            }
            sqlx::query(
                "INSERT INTO mcp_connections (connection_id, user_id, device_id, name, token_verifier, token_fingerprint) VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(id)
            .bind(owner_id)
            .bind(device_id)
            .bind(&projection.name)
            .bind(verifier.to_vec())
            .bind(&projection.token_fingerprint)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            sqlx::query(
                "INSERT INTO mcp_connection_mutations (user_id, idempotency_key, action, connection_id, request_hash) VALUES ($1, $2, 'create', $3, $4)",
            )
            .bind(owner_id)
            .bind(&request.idempotency_key)
            .bind(id)
            .bind(request_hash)
            .execute(&mut *transaction)
            .await
            .map_err(storage)?;
            transaction.commit().await.map_err(storage)?;
        } else {
            let mut memory = self.memory.lock().map_err(|_| McpError::Storage)?;
            if memory
                .by_id
                .values()
                .filter(|connection| {
                    connection.owner_id == owner_id
                        && connection.projection.status == McpConnectionStatus::Active
                })
                .count()
                >= usize::try_from(MAX_CONNECTIONS_PER_ACCOUNT).unwrap_or(20)
            {
                return Err(McpError::Limit);
            }
            memory.by_id.insert(
                id,
                StoredConnection {
                    projection: projection.clone(),
                    owner_id,
                    device_id,
                    verifier,
                },
            );
        }
        Ok(McpConnectionTokenResponse {
            connection: projection,
            endpoint: "/mcp".to_owned(),
            token,
        })
    }

    async fn list(
        &self,
        owner_id: UserId,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> Result<AiPage<McpConnection>, McpError> {
        let limit = limit.clamp(1, MCP_LIST_PAGE_MAX_ITEMS);
        let mut items = if let Some(pool) = &self.pool {
            let rows = sqlx::query(
                "SELECT connection_id, name, token_fingerprint, object_revision, created_at, last_used_at, revoked_at FROM mcp_connections WHERE user_id = $1 AND ($2::uuid IS NULL OR connection_id < $2) ORDER BY connection_id DESC LIMIT $3",
            )
            .bind(owner_id)
            .bind(cursor)
            .bind(i64::try_from(limit.saturating_add(1)).map_err(|_| McpError::Invalid)?)
            .fetch_all(pool)
            .await
            .map_err(storage)?;
            rows.iter()
                .map(connection_from_row)
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let memory = self.memory.lock().map_err(|_| McpError::Storage)?;
            let mut values = memory
                .by_id
                .values()
                .filter(|connection| {
                    connection.owner_id == owner_id
                        && cursor.is_none_or(|cursor| connection.projection.id < cursor)
                })
                .map(|connection| connection.projection.clone())
                .collect::<Vec<_>>();
            values.sort_by(|left, right| right.id.cmp(&left.id));
            values.truncate(limit.saturating_add(1));
            values
        };
        let next_cursor = (items.len() > limit)
            .then(|| {
                items
                    .get(limit.saturating_sub(1))
                    .map(|item| item.id.to_string())
            })
            .flatten();
        items.truncate(limit);
        Ok(AiPage { items, next_cursor })
    }

    async fn revoke(
        &self,
        owner_id: UserId,
        connection_id: Uuid,
        request: &RevokeMcpConnectionRequest,
    ) -> Result<(), McpError> {
        validate_key(&request.idempotency_key)?;
        if let Some(pool) = &self.pool {
            let result = sqlx::query(
                "UPDATE mcp_connections SET revoked_at = COALESCE(revoked_at, now()), object_revision = object_revision + (revoked_at IS NULL)::int WHERE connection_id = $1 AND user_id = $2 AND object_revision = $3",
            )
            .bind(connection_id)
            .bind(owner_id)
            .bind(i64::try_from(request.expected_revision).map_err(|_| McpError::Conflict)?)
            .execute(pool)
            .await
            .map_err(storage)?;
            if result.rows_affected() != 1 {
                return Err(McpError::Conflict);
            }
        } else {
            let mut memory = self.memory.lock().map_err(|_| McpError::Storage)?;
            let stored = memory
                .by_id
                .get_mut(&connection_id)
                .filter(|connection| connection.owner_id == owner_id)
                .ok_or(McpError::NotFound)?;
            if stored.projection.object_revision != request.expected_revision {
                return Err(McpError::Conflict);
            }
            if stored.projection.status == McpConnectionStatus::Active {
                stored.projection.status = McpConnectionStatus::Revoked;
                stored.projection.revoked_at = Some(now_ms());
                stored.projection.object_revision += 1;
            }
        }
        Ok(())
    }

    async fn rotate(
        &self,
        owner_id: UserId,
        connection_id: Uuid,
        request: &RotateMcpConnectionRequest,
    ) -> Result<McpConnectionTokenResponse, McpError> {
        validate_key(&request.idempotency_key)?;
        let token = random_token();
        let verifier = token_verifier(&token);
        let fingerprint = token_fingerprint(&token);
        let connection = if let Some(pool) = &self.pool {
            let row = sqlx::query(
                "UPDATE mcp_connections SET token_verifier = $4, token_fingerprint = $5, revoked_at = NULL, object_revision = object_revision + 1 WHERE connection_id = $1 AND user_id = $2 AND object_revision = $3 RETURNING connection_id, name, token_fingerprint, object_revision, created_at, last_used_at, revoked_at",
            )
            .bind(connection_id)
            .bind(owner_id)
            .bind(i64::try_from(request.expected_revision).map_err(|_| McpError::Conflict)?)
            .bind(verifier.to_vec())
            .bind(&fingerprint)
            .fetch_optional(pool)
            .await
            .map_err(storage)?
            .ok_or(McpError::Conflict)?;
            connection_from_row(&row)?
        } else {
            let mut memory = self.memory.lock().map_err(|_| McpError::Storage)?;
            let stored = memory
                .by_id
                .get_mut(&connection_id)
                .filter(|connection| connection.owner_id == owner_id)
                .ok_or(McpError::NotFound)?;
            if stored.projection.object_revision != request.expected_revision {
                return Err(McpError::Conflict);
            }
            stored.verifier = verifier;
            stored.projection.token_fingerprint = fingerprint;
            stored.projection.status = McpConnectionStatus::Active;
            stored.projection.revoked_at = None;
            stored.projection.object_revision += 1;
            stored.projection.clone()
        };
        Ok(McpConnectionTokenResponse {
            connection,
            endpoint: "/mcp".to_owned(),
            token,
        })
    }

    async fn authenticate(&self, token: &str) -> Result<McpPrincipal, McpError> {
        if !token.starts_with("lumi_mcp_") || token.len() > 128 {
            return Err(McpError::Unauthorized);
        }
        let verifier = token_verifier(token);
        if let Some(pool) = &self.pool {
            let row = sqlx::query(
                "UPDATE mcp_connections SET last_used_at = now() WHERE token_verifier = $1 AND revoked_at IS NULL RETURNING connection_id, user_id, device_id",
            )
            .bind(verifier.to_vec())
            .fetch_optional(pool)
            .await
            .map_err(storage)?
            .ok_or(McpError::Unauthorized)?;
            Ok(McpPrincipal {
                connection_id: row.try_get("connection_id").map_err(storage)?,
                user_id: row.try_get("user_id").map_err(storage)?,
                device_id: row.try_get("device_id").map_err(storage)?,
            })
        } else {
            let mut memory = self.memory.lock().map_err(|_| McpError::Storage)?;
            let stored = memory
                .by_id
                .values_mut()
                .find(|connection| {
                    connection.verifier == verifier
                        && connection.projection.status == McpConnectionStatus::Active
                })
                .ok_or(McpError::Unauthorized)?;
            stored.projection.last_used_at = Some(now_ms());
            Ok(McpPrincipal {
                connection_id: stored.projection.id,
                user_id: stored.owner_id,
                device_id: stored.device_id,
            })
        }
    }

    async fn prepare_delete(
        &self,
        principal: McpPrincipal,
        material_id: Uuid,
        expected_revision: Option<u64>,
    ) -> Result<Value, McpError> {
        if expected_revision == Some(0) {
            return Err(McpError::Invalid);
        }
        let token = random_token();
        let verifier = token_verifier(&token);
        let expires_at = now_ms().saturating_add(5 * 60 * 1000);
        if let Some(pool) = &self.pool {
            let row = sqlx::query(
                "SELECT object_revision FROM materials WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL",
            )
            .bind(material_id)
            .bind(principal.user_id)
            .fetch_optional(pool)
            .await
            .map_err(storage)?
            .ok_or(McpError::NotFound)?;
            let material_revision: i64 = row.try_get("object_revision").map_err(storage)?;
            if expected_revision
                .is_some_and(|expected| i64::try_from(expected).ok() != Some(material_revision))
            {
                return Err(McpError::Conflict);
            }
            sqlx::query(
                "INSERT INTO mcp_delete_challenges (challenge_id, user_id, connection_id, material_id, material_revision, token_verifier, expires_at) VALUES ($1, $2, $3, $4, $5, $6, now() + interval '5 minutes')",
            )
            .bind(Uuid::now_v7())
            .bind(principal.user_id)
            .bind(principal.connection_id)
            .bind(material_id)
            .bind(material_revision)
            .bind(verifier.to_vec())
            .execute(pool)
            .await
            .map_err(storage)?;
        } else {
            self.memory
                .lock()
                .map_err(|_| McpError::Storage)?
                .delete_challenges
                .insert(
                    verifier,
                    MemoryDeleteChallenge {
                        owner_id: principal.user_id,
                        connection_id: principal.connection_id,
                        material_id,
                        expires_at,
                        consumed: false,
                    },
                );
        }
        Ok(json!({
            "confirmation_token": token,
            "material_id": material_id,
            "consequences": "Material, source and private annotations will be permanently deleted.",
            "expires_at": expires_at
        }))
    }

    async fn consume_delete(&self, principal: McpPrincipal, token: &str) -> Result<Uuid, McpError> {
        let verifier = token_verifier(token);
        if let Some(pool) = &self.pool {
            let row = sqlx::query(
                "UPDATE mcp_delete_challenges SET consumed_at = now() WHERE token_verifier = $1 AND user_id = $2 AND connection_id = $3 AND consumed_at IS NULL AND expires_at > now() RETURNING material_id",
            )
            .bind(verifier.to_vec())
            .bind(principal.user_id)
            .bind(principal.connection_id)
            .fetch_optional(pool)
            .await
            .map_err(storage)?
            .ok_or(McpError::Conflict)?;
            row.try_get("material_id").map_err(storage)
        } else {
            let mut memory = self.memory.lock().map_err(|_| McpError::Storage)?;
            let challenge = memory
                .delete_challenges
                .get_mut(&verifier)
                .filter(|challenge| {
                    challenge.owner_id == principal.user_id
                        && challenge.connection_id == principal.connection_id
                        && !challenge.consumed
                        && challenge.expires_at > now_ms()
                })
                .ok_or(McpError::Conflict)?;
            challenge.consumed = true;
            Ok(challenge.material_id)
        }
    }
}

#[derive(Clone, Copy)]
struct McpPrincipal {
    connection_id: Uuid,
    user_id: UserId,
    device_id: Uuid,
}

#[derive(Debug, thiserror::Error)]
enum McpError {
    #[error("invalid request")]
    Invalid,
    #[error("connection not found")]
    NotFound,
    #[error("authorization failed")]
    Unauthorized,
    #[error("revision or idempotency conflict")]
    Conflict,
    #[error("the one-time token was already shown")]
    TokenAlreadyShown,
    #[error("connection limit reached")]
    Limit,
    #[error("MCP storage unavailable")]
    Storage,
}

#[derive(Deserialize)]
struct ConnectionListQuery {
    cursor: Option<Uuid>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct CreateAnnotationToolArgs {
    material_id: Uuid,
    revision_id: Uuid,
    anchor: lumi_core::Anchor,
    idempotency_key: String,
    body: Option<String>,
    style: Option<lumi_core::HighlightStyle>,
}

#[derive(Deserialize)]
struct SocialPageToolArgs {
    space_id: Uuid,
    cursor: Option<String>,
    limit: Option<u16>,
}

#[derive(Deserialize)]
struct SharedDiscussionPageToolArgs {
    space_id: Uuid,
    shared_material_id: Uuid,
    cursor: Option<String>,
    limit: Option<u16>,
}

#[derive(Deserialize)]
struct ShareMaterialToolArgs {
    space_id: Uuid,
    material_id: Uuid,
    idempotency_key: String,
}

#[derive(Deserialize)]
struct CreateSharedCommentToolArgs {
    space_id: Uuid,
    shared_material_id: Option<Uuid>,
    thread_id: Option<Uuid>,
    parent_comment_id: Option<Uuid>,
    body_markdown: String,
    idempotency_key: String,
}

#[derive(Deserialize)]
struct UpdateSharedCommentToolArgs {
    space_id: Uuid,
    comment_id: Uuid,
    body_markdown: String,
    expected_revision: u64,
    idempotency_key: String,
}

#[derive(Deserialize)]
struct DeleteSharedCommentToolArgs {
    space_id: Uuid,
    comment_id: Uuid,
    expected_revision: u64,
    idempotency_key: String,
}

#[derive(Deserialize)]
struct CreateSpaceChatMessageToolArgs {
    space_id: Uuid,
    body_markdown: String,
    idempotency_key: String,
}

/// Return authenticated MCP connection-management routes under `/api/v1`.
pub(crate) fn management_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/mcp/connections",
            get(list_connections).post(create_connection),
        )
        .route(
            "/mcp/connections/{connection_id}",
            delete(revoke_connection),
        )
        .route(
            "/mcp/connections/{connection_id}/rotate",
            post(rotate_connection),
        )
}

/// Return the top-level stateless MCP Streamable HTTP route.
pub(crate) fn transport_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/mcp", post(mcp_request))
        .layer(DefaultBodyLimit::max(MCP_CONTROL_REQUEST_MAX_BYTES))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_mcp_bearer,
        ))
}

async fn list_connections(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(query): Query<ConnectionListQuery>,
) -> Result<Json<AiPage<McpConnection>>, AppError> {
    state
        .mcp_runtime()
        .list(
            session.user_id,
            query.cursor,
            query.limit.unwrap_or(DEFAULT_PAGE_SIZE),
        )
        .await
        .map(Json)
        .map_err(map_management_error)
}

async fn create_connection(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<CreateMcpConnectionRequest>,
) -> Result<(StatusCode, Json<McpConnectionTokenResponse>), AppError> {
    state
        .mcp_runtime()
        .create(session.user_id, &request)
        .await
        .map(|response| (StatusCode::CREATED, Json(response)))
        .map_err(map_management_error)
}

async fn revoke_connection(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(connection_id): Path<Uuid>,
    Json(request): Json<RevokeMcpConnectionRequest>,
) -> Result<StatusCode, AppError> {
    state
        .mcp_runtime()
        .revoke(session.user_id, connection_id, &request)
        .await
        .map(|()| StatusCode::NO_CONTENT)
        .map_err(map_management_error)
}

async fn rotate_connection(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(connection_id): Path<Uuid>,
    Json(request): Json<RotateMcpConnectionRequest>,
) -> Result<Json<McpConnectionTokenResponse>, AppError> {
    state
        .mcp_runtime()
        .rotate(session.user_id, connection_id, &request)
        .await
        .map(Json)
        .map_err(map_management_error)
}

async fn require_mcp_bearer(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(token) = token else {
        return mcp_http_error(StatusCode::UNAUTHORIZED, "invalid_token");
    };
    let principal = match state.mcp_runtime().authenticate(token).await {
        Ok(principal) => principal,
        Err(_) => return mcp_http_error(StatusCode::UNAUTHORIZED, "invalid_token"),
    };
    request.extensions_mut().insert(principal);
    next.run(request).await
}

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

async fn mcp_request(
    State(state): State<AppState>,
    Extension(principal): Extension<McpPrincipal>,
    headers: HeaderMap,
    Json(request): Json<JsonRpcRequest>,
) -> Response {
    if request.jsonrpc != "2.0" {
        return rpc_error(request.id, -32600, "Invalid Request", None).into_response();
    }
    if let Some(version) = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
    {
        if version != MCP_PROTOCOL_VERSION {
            return rpc_error(
                request.id,
                -32602,
                "Unsupported protocol version",
                Some(json!({"supported": MCP_PROTOCOL_VERSION})),
            )
            .into_response();
        }
    }
    let result = tokio::time::timeout(
        MCP_CALL_TIMEOUT,
        dispatch(&state, principal, &request.method, request.params),
    )
    .await;
    match result {
        Err(_) => rpc_error(request.id, -32001, "Request timed out", None).into_response(),
        Ok(Err(error)) => rpc_error(
            request.id,
            error.code(),
            error.message(),
            Some(json!({"kind": error.kind()})),
        )
        .into_response(),
        Ok(Ok(None)) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(Some(value))) => {
            if serde_json::to_vec(&value)
                .is_ok_and(|bytes| bytes.len() > MCP_INLINE_RESULT_MAX_BYTES)
            {
                return rpc_error(request.id, -32003, "Tool result is too large", None)
                    .into_response();
            }
            Json(JsonRpcResponse {
                jsonrpc: "2.0",
                id: request.id,
                result: Some(value),
                error: None,
            })
            .into_response()
        }
    }
}

#[derive(Debug)]
enum ToolError {
    Invalid,
    NotFound,
    Conflict,
    StaleClaim,
    RateLimited,
    Unavailable,
}

impl ToolError {
    fn code(&self) -> i32 {
        match self {
            Self::Invalid => -32602,
            Self::NotFound => -32004,
            Self::Conflict => -32009,
            Self::StaleClaim => -32010,
            Self::RateLimited => -32029,
            Self::Unavailable => -32000,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::Invalid => "Invalid tool arguments",
            Self::NotFound => "Resource not found",
            Self::Conflict => "Object conflict",
            Self::StaleClaim => "Stale task claim",
            Self::RateLimited => "Tool rate limit exceeded",
            Self::Unavailable => "Tool unavailable",
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_arguments",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
            Self::StaleClaim => "stale_claim",
            Self::RateLimited => "rate_limited",
            Self::Unavailable => "unavailable",
        }
    }
}

async fn dispatch(
    state: &AppState,
    principal: McpPrincipal,
    method: &str,
    params: Value,
) -> Result<Option<Value>, ToolError> {
    match method {
        "initialize" => Ok(Some(json!({
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "lumi", "version": env!("CARGO_PKG_VERSION")}
        }))),
        "notifications/initialized" => Ok(None),
        "tools/list" => Ok(Some(json!({"tools": enabled_tools(state)}))),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or(ToolError::Invalid)?;
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let value = call_tool(state, principal, name, arguments).await?;
            Ok(Some(json!({
                "content": [{"type": "text", "text": serde_json::to_string(&value).map_err(|_| ToolError::Unavailable)?}],
                "structuredContent": value,
                "isError": false
            })))
        }
        _ => Err(ToolError::NotFound),
    }
}

fn enabled_tools(state: &AppState) -> Vec<Value> {
    mcp_tool_schema_contracts()
        .iter()
        .filter(|tool| tool_enabled(state, tool.name))
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": format!("Lumi tool contract {}", tool.output_schema),
                "inputSchema": {"type": "object", "additionalProperties": true},
                "outputSchema": {"type": "object"},
                "_meta": {
                    "lumi/contractVersion": MCP_TOOL_CONTRACT_VERSION,
                    "lumi/inputSchemaVersion": tool.input_schema,
                    "lumi/outputSchemaVersion": tool.output_schema
                }
            })
        })
        .collect()
}

fn tool_enabled(state: &AppState, name: &str) -> bool {
    let capabilities = crate::service_capabilities(state);
    let has_feature = |feature: &str| {
        capabilities
            .features
            .iter()
            .any(|candidate| candidate == feature)
    };
    match name {
        "get_lumi_capabilities"
        | "list_materials"
        | "get_material"
        | "get_material_toc"
        | "read_material_chunks"
        | "download_material"
        | "archive_material"
        | "restore_material"
        | "prepare_delete_material"
        | "confirm_delete_material"
        | "list_annotations"
        | "create_highlight"
        | "create_note"
        | "update_annotation"
        | "delete_annotation"
        | "list_learning_items"
        | "submit_learning_answer" => true,
        "import_url" | "import_text" | "get_import_status" => state.imports.is_some(),
        "get_source_context"
        | "create_summary_task"
        | "create_abridgement_task"
        | "get_summary"
        | "list_ai_tasks"
        | "claim_ai_task"
        | "get_ai_task_context"
        | "update_ai_task_progress"
        | "complete_ai_task"
        | "fail_ai_task"
        | "release_ai_task"
        | "get_ai_artifact" => state.ai.is_some(),
        "create_flashcard_task" => state.ai.is_some() && has_feature("learning-ai"),
        "list_community_spaces" | "get_community_space" => has_feature("community-spaces"),
        "share_material_to_space" => has_feature("material-sharing"),
        "list_shared_comments"
        | "create_shared_comment"
        | "update_shared_comment"
        | "delete_shared_comment" => has_feature("material-discussions"),
        "list_space_chat_messages" | "create_space_chat_message" => {
            has_feature("community-communications")
        }
        _ => false,
    }
}

async fn call_tool(
    state: &AppState,
    principal: McpPrincipal,
    name: &str,
    arguments: Value,
) -> Result<Value, ToolError> {
    if !tool_enabled(state, name) {
        return Err(ToolError::NotFound);
    }
    let session = AuthenticatedSession {
        user_id: principal.user_id,
        session_id: principal.connection_id,
        device_id: principal.device_id,
        csrf_hash: [0; 32],
        instance_role: lumi_core::InstanceRole::User,
    };
    match name {
        "get_lumi_capabilities" => Ok(json!({
            "instance": {
                "protocol_version": MCP_PROTOCOL_VERSION,
                "tool_contract_version": MCP_TOOL_CONTRACT_VERSION,
                "features": crate::service_capabilities(state).features,
                "limits": {
                    "control_request_bytes": MCP_CONTROL_REQUEST_MAX_BYTES,
                    "inline_result_bytes": MCP_INLINE_RESULT_MAX_BYTES,
                    "list_page_items": MCP_LIST_PAGE_MAX_ITEMS
                }
            },
            "account": {"connection_id": principal.connection_id}
        })),
        "list_materials" => {
            let limit = page_limit(&arguments)?;
            let cursor = arguments.get("cursor").and_then(Value::as_str);
            let mut items = if let Some(imports) = &state.imports {
                imports
                    .list(principal.user_id)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let repository =
                    crate::read_repository(state).map_err(|_| ToolError::Unavailable)?;
                repository
                    .materials
                    .values()
                    .filter(|material| {
                        material.owner_id == principal.user_id
                            && material.library_state != lumi_core::LibraryState::Deleted
                    })
                    .map(|material| crate::fixture_library_entry(&repository, material))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| ToolError::Unavailable)?
            };
            items.sort_by_key(|item| item.id);
            if let Some(cursor) = cursor.and_then(|value| Uuid::parse_str(value).ok()) {
                items.retain(|item| item.id > cursor);
            }
            let next_cursor = (items.len() > limit)
                .then(|| {
                    items
                        .get(limit.saturating_sub(1))
                        .map(|item| item.id.to_string())
                })
                .flatten();
            items.truncate(limit);
            Ok(json!(AiPage { items, next_cursor }))
        }
        "get_material" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let item = if let Some(imports) = &state.imports {
                imports
                    .material(principal.user_id, material_id)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let repository =
                    crate::read_repository(state).map_err(|_| ToolError::Unavailable)?;
                let material = repository
                    .material_owned_by(principal.user_id, material_id)
                    .map_err(|_| ToolError::NotFound)?;
                crate::fixture_library_entry(&repository, material)
                    .map_err(|_| ToolError::Unavailable)?
            };
            Ok(json!(item))
        }
        "get_material_toc" => {
            let revision_id = uuid_arg(&arguments, "revision_id")?;
            let document = reading_document(state, principal.user_id, revision_id).await?;
            Ok(json!({
                "revision_id": revision_id,
                "items": document.nodes.iter().map(|node| json!({
                    "id": node.id,
                    "kind": node.kind,
                    "text": node.text
                })).collect::<Vec<_>>()
            }))
        }
        "read_material_chunks" => {
            let revision_id = uuid_arg(&arguments, "revision_id")?;
            let limit = page_limit(&arguments)?;
            let offset = arguments
                .get("cursor")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let document = reading_document(state, principal.user_id, revision_id).await?;
            let items = document
                .nodes
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            let consumed = offset.saturating_add(items.len());
            Ok(json!({
                "items": items,
                "next_cursor": (consumed < document.nodes.len()).then(|| consumed.to_string())
            }))
        }
        "get_source_context" => {
            let scope: lumi_core::AiSourceScope =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let pack = state
                .ai_runtime()
                .map_err(|_| ToolError::Unavailable)?
                .context()
                .resolve_for_task(principal.user_id, Uuid::now_v7(), scope)
                .await
                .map_err(|error| match error {
                    crate::ai::context::SourceContextError::NotFound => ToolError::NotFound,
                    crate::ai::context::SourceContextError::StaleSelection => ToolError::Conflict,
                    _ => ToolError::Unavailable,
                })?;
            Ok(json!(pack))
        }
        "download_material" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let (file_name, media_type, bytes) = if let Some(imports) = &state.imports {
                imports
                    .source(principal.user_id, material_id)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let repository =
                    crate::read_repository(state).map_err(|_| ToolError::Unavailable)?;
                repository
                    .ensure_material_owned(principal.user_id, material_id)
                    .map_err(|_| ToolError::NotFound)?;
                let source = repository
                    .source_downloads
                    .get(&material_id)
                    .ok_or(ToolError::NotFound)?;
                (
                    source.file_name.clone(),
                    source.media_type.clone(),
                    source.bytes.clone(),
                )
            };
            Ok(json!({
                "file_name": file_name,
                "media_type": media_type,
                "size": bytes.len(),
                "sha256": hex_digest(&bytes),
                "inline_base64": (bytes.len() <= MCP_INLINE_RESULT_MAX_BYTES / 2)
                    .then(|| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes))
            }))
        }
        "archive_material" | "restore_material" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let idempotency_key = string_arg(&arguments, "idempotency_key")?;
            let library_state = if name == "archive_material" {
                lumi_core::LibraryState::Archived
            } else {
                lumi_core::LibraryState::Active
            };
            let item = if let Some(imports) = &state.imports {
                imports
                    .update_library_state(&session, material_id, library_state, idempotency_key)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let mut repository =
                    crate::write_repository(state).map_err(|_| ToolError::Unavailable)?;
                let material = repository
                    .materials
                    .get_mut(&material_id)
                    .filter(|material| material.owner_id == principal.user_id)
                    .ok_or(ToolError::NotFound)?;
                material.library_state = library_state;
                let material = material.clone();
                crate::fixture_library_entry(&repository, &material)
                    .map_err(|_| ToolError::Unavailable)?
            };
            Ok(json!(item))
        }
        "prepare_delete_material" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let material_revision = arguments.get("expected_revision").and_then(Value::as_u64);
            if let Some(imports) = &state.imports {
                imports
                    .material(principal.user_id, material_id)
                    .await
                    .map_err(map_import_tool)?;
            } else {
                crate::read_repository(state)
                    .map_err(|_| ToolError::Unavailable)?
                    .ensure_material_owned(principal.user_id, material_id)
                    .map_err(|_| ToolError::NotFound)?;
            }
            state
                .mcp_runtime()
                .prepare_delete(principal, material_id, material_revision)
                .await
                .map_err(map_mcp_tool)
        }
        "confirm_delete_material" => {
            let confirmation_token = string_arg(&arguments, "confirmation_token")?;
            let idempotency_key = string_arg(&arguments, "idempotency_key")?;
            let material_id = state
                .mcp_runtime()
                .consume_delete(principal, confirmation_token)
                .await
                .map_err(map_mcp_tool)?;
            if let Some(imports) = &state.imports {
                imports
                    .delete_material(&session, material_id, idempotency_key)
                    .await
                    .map_err(map_import_tool)?;
            } else {
                let mut repository =
                    crate::write_repository(state).map_err(|_| ToolError::Unavailable)?;
                let material = repository
                    .materials
                    .get_mut(&material_id)
                    .filter(|material| material.owner_id == principal.user_id)
                    .ok_or(ToolError::NotFound)?;
                material.library_state = lumi_core::LibraryState::Deleted;
            }
            Ok(json!({}))
        }
        "list_annotations" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let items = if let Some(imports) = &state.imports {
                imports
                    .annotations(principal.user_id, material_id)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let repository =
                    crate::read_repository(state).map_err(|_| ToolError::Unavailable)?;
                repository
                    .ensure_material_owned(principal.user_id, material_id)
                    .map_err(|_| ToolError::NotFound)?;
                repository
                    .annotations_by_material
                    .get(&material_id)
                    .cloned()
                    .unwrap_or_default()
            };
            Ok(json!(AiPage {
                items,
                next_cursor: None
            }))
        }
        "create_highlight" | "create_note" => {
            let input: CreateAnnotationToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let kind = if name == "create_highlight" {
                let style = input.style.unwrap_or(lumi_core::HighlightStyle::Yellow);
                lumi_core::AnnotationKind::Highlight { style }
            } else {
                lumi_core::AnnotationKind::Note {
                    body: input
                        .body
                        .filter(|body| !body.trim().is_empty() && body.len() <= 256 * 1024)
                        .ok_or(ToolError::Invalid)?,
                }
            };
            let command = lumi_core::CreateAnnotationCommand {
                material_id: input.material_id,
                revision_id: input.revision_id,
                anchor: input.anchor,
                kind,
            };
            let idempotency_key = input.idempotency_key;
            let annotation = if let Some(imports) = &state.imports {
                imports
                    .create_annotation(&session, command, &idempotency_key)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let mut repository =
                    crate::write_repository(state).map_err(|_| ToolError::Unavailable)?;
                repository
                    .ensure_material_owned(principal.user_id, command.material_id)
                    .map_err(|_| ToolError::NotFound)?;
                repository
                    .ensure_revision_owned(principal.user_id, command.revision_id)
                    .map_err(|_| ToolError::NotFound)?;
                let annotation =
                    lumi_core::Annotation::create(command, lumi_core::now_timestamp_ms());
                repository
                    .annotations_by_material
                    .entry(annotation.material_id)
                    .or_default()
                    .push(annotation.clone());
                annotation
            };
            Ok(json!(annotation))
        }
        "update_annotation" => {
            let command: lumi_core::UpdateAnnotationCommand =
                serde_json::from_value(arguments.clone()).map_err(|_| ToolError::Invalid)?;
            let idempotency_key = string_arg(&arguments, "idempotency_key")?;
            let annotation = if let Some(imports) = &state.imports {
                imports
                    .update_annotation(&session, command, idempotency_key)
                    .await
                    .map_err(map_import_tool)?
            } else {
                let mut repository =
                    crate::write_repository(state).map_err(|_| ToolError::Unavailable)?;
                repository
                    .ensure_material_owned(principal.user_id, command.material_id)
                    .map_err(|_| ToolError::NotFound)?;
                let annotations = repository
                    .annotations_by_material
                    .get_mut(&command.material_id)
                    .ok_or(ToolError::NotFound)?;
                let annotation = annotations
                    .iter_mut()
                    .find(|annotation| annotation.id == command.annotation_id)
                    .ok_or(ToolError::NotFound)?;
                if annotation.revision != command.expected_revision {
                    return Err(ToolError::Conflict);
                }
                annotation.update_kind(command.kind, lumi_core::now_timestamp_ms());
                annotation.clone()
            };
            Ok(json!(annotation))
        }
        "delete_annotation" => {
            let command: lumi_core::DeleteAnnotationCommand =
                serde_json::from_value(arguments.clone()).map_err(|_| ToolError::Invalid)?;
            let idempotency_key = string_arg(&arguments, "idempotency_key")?;
            if let Some(imports) = &state.imports {
                imports
                    .delete_annotation(&session, command, idempotency_key)
                    .await
                    .map_err(map_import_tool)?;
            } else {
                let mut repository =
                    crate::write_repository(state).map_err(|_| ToolError::Unavailable)?;
                repository
                    .ensure_material_owned(principal.user_id, command.material_id)
                    .map_err(|_| ToolError::NotFound)?;
                let annotations = repository
                    .annotations_by_material
                    .get_mut(&command.material_id)
                    .ok_or(ToolError::NotFound)?;
                let index = annotations
                    .iter()
                    .position(|annotation| annotation.id == command.annotation_id)
                    .ok_or(ToolError::NotFound)?;
                if annotations[index].revision != command.expected_revision {
                    return Err(ToolError::Conflict);
                }
                annotations.remove(index);
            }
            Ok(json!({}))
        }
        "import_url" => {
            let url = string_arg(&arguments, "url")?;
            let idempotency_key = string_arg(&arguments, "idempotency_key")?;
            let accepted = state
                .imports
                .as_ref()
                .ok_or(ToolError::Unavailable)?
                .accept_web(&session, url, idempotency_key)
                .await
                .map_err(map_import_tool)?;
            Ok(json!(accepted))
        }
        "import_text" => {
            let text = arguments
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty() && text.len() <= 512 * 1024)
                .ok_or(ToolError::Invalid)?;
            let idempotency_key = string_arg(&arguments, "idempotency_key")?;
            let accepted = state
                .imports
                .as_ref()
                .ok_or(ToolError::Unavailable)?
                .accept(
                    &session,
                    "agent-import.md",
                    idempotency_key,
                    text.as_bytes().to_vec(),
                )
                .await
                .map_err(map_import_tool)?;
            Ok(json!(accepted))
        }
        "get_import_status" => {
            let job_id = uuid_arg(&arguments, "job_id")?;
            let job = state
                .imports
                .as_ref()
                .ok_or(ToolError::Unavailable)?
                .job(principal.user_id, job_id)
                .await
                .map_err(map_import_tool)?;
            Ok(json!(job))
        }
        "list_ai_tasks" => {
            let repository = ai_repository(state)?;
            let mut items = repository
                .list_tasks(principal.user_id)
                .await
                .map_err(map_ai_tool)?;
            let limit = page_limit(&arguments)?;
            items.truncate(limit);
            Ok(json!(AiPage {
                items,
                next_cursor: None
            }))
        }
        "claim_ai_task" => {
            let task_id = uuid_arg(&arguments, "task_id")?;
            let claim = ai_repository(state)?
                .claim_task(
                    principal.user_id,
                    task_id,
                    AiExecutorKind::McpAgent,
                    Some(&principal.connection_id.to_string()),
                )
                .await
                .map_err(map_ai_tool)?;
            Ok(json!(claim))
        }
        "get_ai_task_context" => {
            let fence: AiTaskClaimFence =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let repository = ai_repository(state)?;
            let run = repository
                .run(principal.user_id, fence.run_id)
                .await
                .map_err(map_ai_tool)?;
            validate_run_fence(&run, &fence)?;
            let context = repository
                .context_pack(principal.user_id, run.context_pack_id)
                .await
                .map_err(map_ai_tool)?;
            Ok(json!(context))
        }
        "update_ai_task_progress" => {
            let progress: McpAiTaskProgressRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let repository = ai_repository(state)?;
            let run = repository
                .run(principal.user_id, progress.claim.run_id)
                .await
                .map_err(map_ai_tool)?;
            validate_run_fence(&run, &progress.claim)?;
            let claim = claim_from_run(&run, &progress.claim);
            let cancelled = repository
                .heartbeat(
                    principal.user_id,
                    &claim,
                    progress.fraction,
                    progress.stage.as_deref(),
                    Duration::from_secs(15 * 60),
                )
                .await
                .map_err(map_ai_tool)?;
            Ok(json!({"claim": claim, "cancelled": cancelled}))
        }
        "complete_ai_task" => {
            let request: CompleteAiTaskRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let abridgement = request.result.get("schema_version").and_then(Value::as_str)
                == Some(lumi_core::ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION);
            if abridgement {
                state
                    .imports
                    .as_ref()
                    .ok_or(ToolError::Unavailable)?
                    .preflight_abridgement_completion(principal.user_id, &request)
                    .await
                    .map_err(map_import_tool)?;
            }
            let result = ai_repository(state)?
                .complete_task(principal.user_id, request)
                .await
                .map_err(map_ai_tool)?;
            if result.result_schema_version == lumi_core::ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION {
                state
                    .imports
                    .as_ref()
                    .ok_or(ToolError::Unavailable)?
                    .publish_abridgement_artifact(principal.user_id, result.artifact_id)
                    .await
                    .map_err(map_import_tool)?;
            }
            Ok(json!(result))
        }
        "fail_ai_task" | "release_ai_task" => {
            let fence: AiTaskClaimFence =
                serde_json::from_value(arguments.clone()).map_err(|_| ToolError::Invalid)?;
            let repository = ai_repository(state)?;
            let run = repository
                .run(principal.user_id, fence.run_id)
                .await
                .map_err(map_ai_tool)?;
            validate_run_fence(&run, &fence)?;
            let claim = claim_from_run(&run, &fence);
            let retryable = name == "release_ai_task"
                || arguments
                    .get("retryable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
            let code = arguments
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or(if retryable {
                    "mcp_released"
                } else {
                    "mcp_failed"
                });
            let task = repository
                .fail_task(principal.user_id, &claim, code, retryable)
                .await
                .map_err(map_ai_tool)?;
            Ok(json!(task))
        }
        "get_ai_artifact" => {
            let artifact_id = uuid_arg(&arguments, "artifact_id")?;
            let artifact = ai_repository(state)?
                .artifact(principal.user_id, artifact_id)
                .await
                .map_err(map_ai_tool)?;
            Ok(json!(artifact))
        }
        "create_summary_task" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let request: lumi_core::CreateSummaryTaskRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let task = crate::ai::tasks::create_summary_task_for_owner(
                state,
                principal.user_id,
                material_id,
                request,
            )
            .await
            .map_err(|_| ToolError::Unavailable)?;
            Ok(json!(task))
        }
        "create_abridgement_task" => {
            let material_id = uuid_arg(&arguments, "material_id")?;
            let request: lumi_core::CreateAbridgementTaskRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let task = crate::ai::tasks::create_abridgement_task_for_owner(
                state,
                principal.user_id,
                material_id,
                request,
            )
            .await
            .map_err(|_| ToolError::Unavailable)?;
            Ok(json!(task))
        }
        "get_summary" => {
            let summary_id = uuid_arg(&arguments, "summary_id")?;
            let summary = ai_repository(state)?
                .summary(principal.user_id, summary_id)
                .await
                .map_err(map_ai_tool)?;
            Ok(json!(summary))
        }
        "list_learning_items" => {
            let request: ListLearningItemsRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let limit = request.limit.unwrap_or(DEFAULT_PAGE_SIZE);
            if limit == 0 || limit > MCP_LIST_PAGE_MAX_ITEMS {
                return Err(ToolError::Invalid);
            }
            let page = state
                .learning_runtime()
                .list_items(
                    principal.user_id,
                    request.material_id,
                    request.source_id,
                    request.status,
                    request.cursor,
                    limit,
                )
                .await
                .map_err(map_learning_tool)?;
            Ok(json!(page))
        }
        "create_flashcard_task" => {
            let request: CreateFlashcardTaskRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let task = crate::learning::generation::create_learning_generation_task(
                state,
                principal.user_id,
                request.source_id,
                &GenerateLearningItemsRequest {
                    item_count: request.item_count,
                    execution_mode: request.execution_mode,
                    idempotency_key: request.idempotency_key,
                },
                crate::learning::generation::LearningGenerationTarget::Flashcards,
            )
            .await
            .map_err(map_learning_generation_tool)?;
            Ok(json!(task))
        }
        "submit_learning_answer" => {
            let request: SubmitLearningAnswerRequest =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            if request.idempotency_key.trim().is_empty() || request.idempotency_key.len() > 256 {
                return Err(ToolError::Invalid);
            }
            let attempt = state
                .learning_runtime()
                .submit_attempt(
                    principal.user_id,
                    principal.device_id,
                    request.session_id,
                    request.item_id,
                    &SubmitLearningAttemptCommand {
                        answer: request.answer,
                        self_check: request.self_check,
                        elapsed_ms: request.elapsed_ms,
                        review_rating: request.review_rating,
                    },
                    &request.idempotency_key,
                )
                .await
                .map_err(map_learning_tool)?;
            Ok(json!(attempt))
        }
        "list_community_spaces" => {
            let mut spaces = state
                .social_runtime()
                .list(principal.user_id)
                .await
                .map_err(map_social_tool)?;
            let limit = page_limit(&arguments)?;
            let cursor = arguments
                .get("cursor")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok());
            spaces.sort_by_key(|space| space.id);
            if let Some(cursor) = cursor {
                spaces.retain(|space| space.id > cursor);
            }
            let next_cursor = (spaces.len() > limit)
                .then(|| {
                    spaces
                        .get(limit.saturating_sub(1))
                        .map(|space| space.id.to_string())
                })
                .flatten();
            spaces.truncate(limit);
            Ok(json!(AiPage {
                items: spaces,
                next_cursor
            }))
        }
        "get_community_space" => {
            let space_id = uuid_arg(&arguments, "space_id")?;
            let detail = state
                .social_runtime()
                .detail(principal.user_id, space_id)
                .await
                .map_err(map_social_tool)?;
            Ok(json!(detail))
        }
        "share_material_to_space" => {
            let input: ShareMaterialToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let shared = state
                .social_runtime()
                .share_material(
                    principal.user_id,
                    principal.device_id,
                    input.space_id,
                    &input.idempotency_key,
                    ShareMaterialRequest {
                        material_id: input.material_id,
                    },
                )
                .await
                .map_err(map_social_tool)?;
            Ok(json!(shared))
        }
        "list_shared_comments" => {
            let input: SharedDiscussionPageToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let page = state
                .social_runtime()
                .list_discussions(
                    principal.user_id,
                    input.space_id,
                    input.shared_material_id,
                    input.cursor.as_deref(),
                    input.limit.unwrap_or(DEFAULT_PAGE_SIZE as u16),
                )
                .await
                .map_err(map_social_tool)?;
            Ok(json!(page))
        }
        "create_shared_comment" => {
            let input: CreateSharedCommentToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let response = match (input.shared_material_id, input.thread_id) {
                (Some(shared_material_id), None) if input.parent_comment_id.is_none() => {
                    let thread = state
                        .social_runtime()
                        .create_thread(
                            principal.user_id,
                            principal.device_id,
                            input.space_id,
                            shared_material_id,
                            &input.idempotency_key,
                            CreateSharedThreadRequest {
                                body_markdown: input.body_markdown,
                            },
                        )
                        .await
                        .map_err(map_social_tool)?;
                    json!({"thread": thread})
                }
                (None, Some(thread_id)) => {
                    let comment = state
                        .social_runtime()
                        .add_comment(
                            principal.user_id,
                            principal.device_id,
                            input.space_id,
                            thread_id,
                            &input.idempotency_key,
                            CreateSharedCommentRequest {
                                parent_comment_id: input.parent_comment_id,
                                body_markdown: input.body_markdown,
                            },
                        )
                        .await
                        .map_err(map_social_tool)?;
                    json!({"comment": comment})
                }
                _ => return Err(ToolError::Invalid),
            };
            Ok(response)
        }
        "update_shared_comment" => {
            let input: UpdateSharedCommentToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let comment = state
                .social_runtime()
                .update_comment(
                    principal.user_id,
                    principal.device_id,
                    input.space_id,
                    input.comment_id,
                    &input.idempotency_key,
                    UpdateSharedCommentRequest {
                        body_markdown: input.body_markdown,
                        expected_revision: input.expected_revision,
                    },
                )
                .await
                .map_err(map_social_tool)?;
            Ok(json!(comment))
        }
        "delete_shared_comment" => {
            let input: DeleteSharedCommentToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let comment = state
                .social_runtime()
                .delete_comment(
                    principal.user_id,
                    principal.device_id,
                    input.space_id,
                    input.comment_id,
                    &input.idempotency_key,
                    DeleteSharedCommentRequest {
                        expected_revision: input.expected_revision,
                    },
                )
                .await
                .map_err(map_social_tool)?;
            Ok(json!(comment))
        }
        "list_space_chat_messages" => {
            let input: SocialPageToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let page = state
                .social_runtime()
                .list_chat(
                    principal.user_id,
                    input.space_id,
                    input.cursor.as_deref(),
                    input.limit.unwrap_or(DEFAULT_PAGE_SIZE as u16),
                )
                .await
                .map_err(map_social_tool)?;
            Ok(json!(page))
        }
        "create_space_chat_message" => {
            let input: CreateSpaceChatMessageToolArgs =
                serde_json::from_value(arguments).map_err(|_| ToolError::Invalid)?;
            let message = state
                .social_runtime()
                .create_chat_message(
                    principal.user_id,
                    principal.device_id,
                    input.space_id,
                    &input.idempotency_key,
                    CreateSharedChatMessageRequest {
                        body_markdown: input.body_markdown,
                    },
                )
                .await
                .map_err(map_social_tool)?;
            Ok(json!(message))
        }
        _ => Err(ToolError::NotFound),
    }
}

async fn reading_document(
    state: &AppState,
    owner_id: UserId,
    revision_id: Uuid,
) -> Result<lumi_core::ReadingDocument, ToolError> {
    if let Some(imports) = &state.imports {
        imports
            .reading_document(owner_id, revision_id)
            .await
            .map_err(map_import_tool)
    } else {
        let repository = crate::read_repository(state).map_err(|_| ToolError::Unavailable)?;
        repository
            .ensure_revision_owned(owner_id, revision_id)
            .map_err(|_| ToolError::NotFound)?;
        repository
            .reading_documents_by_revision
            .get(&revision_id)
            .cloned()
            .ok_or(ToolError::NotFound)
    }
}

fn ai_repository(state: &AppState) -> Result<crate::ai::repository::PgAiRepository, ToolError> {
    Ok(crate::ai::repository::PgAiRepository::new(
        state
            .ai_runtime()
            .map_err(|_| ToolError::Unavailable)?
            .pool()
            .clone(),
    ))
}

fn validate_run_fence(run: &lumi_core::AiRun, fence: &AiTaskClaimFence) -> Result<(), ToolError> {
    if run.id != fence.run_id
        || run.task_id != fence.task_id
        || run.claim_id != Some(fence.claim_id)
        || run.fence != Some(fence.fence)
        || !matches!(run.status, lumi_core::AiRunStatus::Running)
    {
        return Err(ToolError::StaleClaim);
    }
    Ok(())
}

fn claim_from_run(run: &lumi_core::AiRun, fence: &AiTaskClaimFence) -> AiTaskClaim {
    AiTaskClaim {
        task_id: fence.task_id,
        run_id: fence.run_id,
        claim_id: fence.claim_id,
        fence: fence.fence,
        lease_expires_at: run.lease_expires_at.clone().unwrap_or_default(),
        task_revision: fence.task_revision,
        result_kind: String::new(),
        result_schema_version: run.output_schema_version.clone(),
        context_pack_id: run.context_pack_id,
    }
}

fn page_limit(arguments: &Value) -> Result<usize, ToolError> {
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_PAGE_SIZE as u64);
    let limit = usize::try_from(limit).map_err(|_| ToolError::Invalid)?;
    if limit == 0 || limit > MCP_LIST_PAGE_MAX_ITEMS {
        return Err(ToolError::Invalid);
    }
    Ok(limit)
}

fn uuid_arg(arguments: &Value, name: &str) -> Result<Uuid, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(ToolError::Invalid)
}

fn string_arg<'a>(arguments: &'a Value, name: &str) -> Result<&'a str, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && value.len() <= 256)
        .ok_or(ToolError::Invalid)
}

fn map_import_tool(error: crate::imports::ImportServiceError) -> ToolError {
    match error {
        crate::imports::ImportServiceError::NotFound => ToolError::NotFound,
        crate::imports::ImportServiceError::Conflict => ToolError::Conflict,
        crate::imports::ImportServiceError::BadRequest(_) => ToolError::Invalid,
        _ => ToolError::Unavailable,
    }
}

fn map_ai_tool(error: crate::ai::repository::AiRepositoryError) -> ToolError {
    match error {
        crate::ai::repository::AiRepositoryError::NotFound => ToolError::NotFound,
        crate::ai::repository::AiRepositoryError::Conflict => ToolError::Conflict,
        crate::ai::repository::AiRepositoryError::StaleClaim => ToolError::StaleClaim,
        crate::ai::repository::AiRepositoryError::Invalid(_) => ToolError::Invalid,
        _ => ToolError::Unavailable,
    }
}

fn map_learning_tool(error: crate::learning::repository::LearningStoreError) -> ToolError {
    match error {
        crate::learning::repository::LearningStoreError::NotFound => ToolError::NotFound,
        crate::learning::repository::LearningStoreError::Conflict => ToolError::Conflict,
        crate::learning::repository::LearningStoreError::Invalid(_)
        | crate::learning::repository::LearningStoreError::InvalidDetail(_) => ToolError::Invalid,
        crate::learning::repository::LearningStoreError::Unavailable => ToolError::Unavailable,
    }
}

fn map_learning_generation_tool(
    error: crate::learning::generation::LearningGenerationError,
) -> ToolError {
    match error {
        crate::learning::generation::LearningGenerationError::Invalid(_) => ToolError::Invalid,
        crate::learning::generation::LearningGenerationError::NotFound => ToolError::NotFound,
        crate::learning::generation::LearningGenerationError::Conflict => ToolError::Conflict,
        crate::learning::generation::LearningGenerationError::Unavailable => ToolError::Unavailable,
        crate::learning::generation::LearningGenerationError::Limited => ToolError::RateLimited,
    }
}

fn map_social_tool(error: crate::social::SocialStoreError) -> ToolError {
    match error {
        crate::social::SocialStoreError::NotFound | crate::social::SocialStoreError::Forbidden => {
            ToolError::NotFound
        }
        crate::social::SocialStoreError::Conflict => ToolError::Conflict,
        crate::social::SocialStoreError::Invalid(_) => ToolError::Invalid,
        crate::social::SocialStoreError::RateLimited => ToolError::RateLimited,
        crate::social::SocialStoreError::Unavailable => ToolError::Unavailable,
    }
}

fn map_mcp_tool(error: McpError) -> ToolError {
    match error {
        McpError::Invalid => ToolError::Invalid,
        McpError::NotFound => ToolError::NotFound,
        McpError::Conflict | McpError::TokenAlreadyShown => ToolError::Conflict,
        McpError::Unauthorized | McpError::Limit | McpError::Storage => ToolError::Unavailable,
    }
}

fn map_management_error(error: McpError) -> AppError {
    match error {
        McpError::Invalid => AppError::BadRequest("invalid MCP connection request".to_owned()),
        McpError::NotFound => AppError::NotFound("MCP connection"),
        McpError::Unauthorized => AppError::Unauthorized,
        McpError::Conflict => AppError::Conflict("MCP connection revision conflict".to_owned()),
        McpError::TokenAlreadyShown => AppError::Conflict(
            "the one-time MCP token was already shown; rotate the connection".to_owned(),
        ),
        McpError::Limit => AppError::TooManyRequests("MCP connection limit reached"),
        McpError::Storage => AppError::Unavailable("MCP repository"),
    }
}

fn validate_name_and_key(name: &str, key: &str) -> Result<(), McpError> {
    if name.trim().is_empty() || name.trim().len() > 120 {
        return Err(McpError::Invalid);
    }
    validate_key(key)
}

fn validate_key(key: &str) -> Result<(), McpError> {
    if key.trim().is_empty() || key.len() > 256 {
        return Err(McpError::Invalid);
    }
    Ok(())
}

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut bytes);
    format!("lumi_mcp_{}", lumi_core::encode_auth_bytes(&bytes))
}

fn token_verifier(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

fn token_fingerprint(token: &str) -> String {
    let digest = token_verifier(token);
    format!("mcp_{}", encode_hex(&digest[..6]))
}

fn hex_digest(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn hash_json(value: &Value) -> Result<String, McpError> {
    serde_json::to_vec(value)
        .map(|bytes| hex_digest(&bytes))
        .map_err(|_| McpError::Invalid)
}

fn now_ms() -> u64 {
    let now = OffsetDateTime::now_utc();
    let value = now
        .unix_timestamp()
        .saturating_mul(1000)
        .saturating_add(i64::from(now.millisecond()));
    u64::try_from(value).unwrap_or_default()
}

fn connection_from_row(row: &sqlx_postgres::PgRow) -> Result<McpConnection, McpError> {
    let object_revision: i64 = row.try_get("object_revision").map_err(storage)?;
    let created_at: OffsetDateTime = row.try_get("created_at").map_err(storage)?;
    let last_used_at: Option<OffsetDateTime> = row.try_get("last_used_at").map_err(storage)?;
    let revoked_at: Option<OffsetDateTime> = row.try_get("revoked_at").map_err(storage)?;
    Ok(McpConnection {
        id: row.try_get("connection_id").map_err(storage)?,
        name: row.try_get("name").map_err(storage)?,
        token_fingerprint: row.try_get("token_fingerprint").map_err(storage)?,
        status: if revoked_at.is_some() {
            McpConnectionStatus::Revoked
        } else {
            McpConnectionStatus::Active
        },
        object_revision: u64::try_from(object_revision).map_err(|_| McpError::Storage)?,
        created_at: timestamp_ms(created_at),
        last_used_at: last_used_at.map(timestamp_ms),
        revoked_at: revoked_at.map(timestamp_ms),
    })
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    let value = value
        .unix_timestamp()
        .saturating_mul(1000)
        .saturating_add(i64::from(value.millisecond()));
    u64::try_from(value).unwrap_or_default()
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

fn storage<E>(_: E) -> McpError {
    McpError::Storage
}

fn rpc_error(
    id: Option<Value>,
    code: i32,
    message: &'static str,
    data: Option<Value>,
) -> Json<JsonRpcResponse> {
    Json(JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message,
            data,
        }),
    })
}

fn mcp_http_error(status: StatusCode, code: &'static str) -> Response {
    (
        status,
        [(header::WWW_AUTHENTICATE, "Bearer realm=\"lumi-mcp\"")],
        Json(json!({"error": code})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    #[tokio::test]
    async fn revoked_connection_is_rejected_on_next_authentication(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let runtime = McpRuntime::memory();
        let owner_id = Uuid::now_v7();
        let created = runtime
            .create(
                owner_id,
                &CreateMcpConnectionRequest {
                    name: "Codex".to_owned(),
                    idempotency_key: "create-1".to_owned(),
                },
            )
            .await?;
        runtime.authenticate(&created.token).await?;
        runtime
            .revoke(
                owner_id,
                created.connection.id,
                &RevokeMcpConnectionRequest {
                    expected_revision: created.connection.object_revision,
                    idempotency_key: "revoke-1".to_owned(),
                },
            )
            .await?;

        assert!(matches!(
            runtime.authenticate(&created.token).await,
            Err(McpError::Unauthorized)
        ));
        Ok(())
    }

    #[test]
    fn tool_catalog_does_not_expose_credentials_or_personal_ai_chat() {
        let state = AppState::seeded();
        let names = enabled_tools(&state)
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();

        assert!(!names.iter().any(|name| name.contains("credential")));
        assert!(!names.iter().any(|name| name == "read_ai_chat"));
    }

    #[test]
    fn learning_tool_catalog_hides_ai_mutation_without_learning_ai_capability() {
        let state = AppState::seeded();
        let names = enabled_tools(&state)
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "list_learning_items"));
        assert!(names.iter().any(|name| name == "submit_learning_answer"));
        assert!(!names.iter().any(|name| name == "create_flashcard_task"));
    }

    #[test]
    fn social_tool_catalog_fails_closed_by_capability_slice() {
        let state = AppState::seeded();
        let names = enabled_tools(&state)
            .into_iter()
            .filter_map(|tool| tool["name"].as_str().map(ToOwned::to_owned))
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == "list_community_spaces"));
        assert!(names.iter().any(|name| name == "get_community_space"));
        assert!(!names.iter().any(|name| name == "share_material_to_space"));
        assert!(!names.iter().any(|name| name == "list_shared_comments"));
        assert!(!names.iter().any(|name| name == "list_space_chat_messages"));
    }

    #[test]
    fn social_forbidden_is_indistinguishable_from_missing_resource() {
        assert!(matches!(
            map_social_tool(crate::social::SocialStoreError::Forbidden),
            ToolError::NotFound
        ));
    }

    #[tokio::test]
    async fn community_list_and_detail_tools_reuse_social_runtime(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let state = AppState::seeded();
        let owner_id = Uuid::now_v7();
        let principal = McpPrincipal {
            connection_id: Uuid::now_v7(),
            user_id: owner_id,
            device_id: Uuid::now_v7(),
        };
        let created = state
            .social_runtime()
            .create(
                owner_id,
                principal.device_id,
                "mcp-community-create",
                lumi_core::CreateCommunitySpaceRequest {
                    name: "MCP клуб".to_owned(),
                    description: None,
                },
            )
            .await?;

        let page = call_tool(
            &state,
            principal,
            "list_community_spaces",
            json!({"limit": 50}),
        )
        .await
        .map_err(|_| std::io::Error::other("community list tool failed"))?;
        let detail = call_tool(
            &state,
            principal,
            "get_community_space",
            json!({"space_id": created.space.id}),
        )
        .await
        .map_err(|_| std::io::Error::other("community detail tool failed"))?;

        assert_eq!(page["items"][0]["id"], json!(created.space.id));
        assert_eq!(detail["space"]["id"], json!(created.space.id));
        Ok(())
    }

    #[tokio::test]
    async fn learning_tools_reuse_owner_scoped_application_service(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let state = AppState::seeded();
        let (owner_id, material_id, revision_id, content_unit_id) = {
            let repository = crate::read_repository(&state)
                .map_err(|_| std::io::Error::other("seeded repository unavailable"))?;
            let material = repository
                .materials
                .values()
                .next()
                .ok_or("seeded material missing")?;
            let package = repository
                .packages_by_revision
                .get(&material.active_revision_id)
                .ok_or("seeded package missing")?;
            (
                material.owner_id,
                material.id,
                material.active_revision_id,
                package
                    .units
                    .first()
                    .ok_or("seeded content unit missing")?
                    .id
                    .clone(),
            )
        };
        let device_id = Uuid::now_v7();
        let completion_command = lumi_core::CompleteReadingScopeCommand {
            material_id,
            revision_id,
            scope_kind: lumi_core::LearningScopeKind::ContentUnit,
            content_unit_id: Some(content_unit_id),
            anchor: None,
            trigger: lumi_core::ReadingCompletionTrigger::ExplicitUserAction,
        };
        let context = state
            .learning_material_context(owner_id, &completion_command)
            .map_err(|_| std::io::Error::other("learning material context unavailable"))?;
        let completion = state
            .learning_runtime()
            .complete_reading(
                owner_id,
                device_id,
                context,
                &completion_command,
                "mcp-learning-completion",
            )
            .await?;
        let item = state
            .learning_runtime()
            .create_item(
                owner_id,
                device_id,
                &lumi_core::CreateLearningItemCommand {
                    source_id: completion.offer.source.id,
                    kind: lumi_core::LearningItemKind::QuizTrueFalse,
                    status: lumi_core::LearningItemStatus::Active,
                    prompt: "Revision хранит immutable content?".to_owned(),
                    answer_spec: lumi_core::LearningAnswerSpec::TrueFalse { correct: true },
                    explanation: "Да.".to_owned(),
                    hints: Vec::new(),
                    source_anchor: None,
                },
                "mcp-learning-item",
            )
            .await?;
        let learning_session = state
            .learning_runtime()
            .create_session(
                owner_id,
                device_id,
                lumi_core::CreateLearningSessionCommand {
                    source_id: completion.offer.source.id,
                    kind: lumi_core::LearningSessionKind::ManualPractice,
                },
                "mcp-learning-session",
            )
            .await?;
        state
            .learning_runtime()
            .transition_session(
                owner_id,
                device_id,
                learning_session.id,
                crate::learning::repository::SessionTransition::Start,
                "mcp-learning-start",
            )
            .await?;
        let principal = McpPrincipal {
            connection_id: Uuid::now_v7(),
            user_id: owner_id,
            device_id,
        };

        let page = call_tool(
            &state,
            principal,
            "list_learning_items",
            json!({"source_id": completion.offer.source.id, "limit": 10}),
        )
        .await
        .map_err(|_| std::io::Error::other("MCP learning list failed"))?;
        assert_eq!(page["items"].as_array().map(Vec::len), Some(1));
        let attempt = call_tool(
            &state,
            principal,
            "submit_learning_answer",
            json!({
                "session_id": learning_session.id,
                "item_id": item.id,
                "answer": {"type": "true_false", "value": true},
                "self_check": null,
                "elapsed_ms": 1200,
                "review_rating": "good",
                "idempotency_key": "mcp-learning-answer"
            }),
        )
        .await
        .map_err(|_| std::io::Error::other("MCP learning submit failed"))?;
        assert_eq!(attempt["feedback"]["outcome"], "correct");
        let replayed_attempt = call_tool(
            &state,
            principal,
            "submit_learning_answer",
            json!({
                "session_id": learning_session.id,
                "item_id": item.id,
                "answer": {"type": "true_false", "value": true},
                "self_check": null,
                "elapsed_ms": 1200,
                "review_rating": "good",
                "idempotency_key": "mcp-learning-answer"
            }),
        )
        .await
        .map_err(|_| std::io::Error::other("MCP learning replay failed"))?;
        assert_eq!(replayed_attempt["id"], attempt["id"]);

        assert!(matches!(
            call_tool(
                &state,
                principal,
                "list_learning_items",
                json!({"limit": MCP_LIST_PAGE_MAX_ITEMS + 1}),
            )
            .await,
            Err(ToolError::Invalid)
        ));

        let foreign = McpPrincipal {
            connection_id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            device_id: Uuid::now_v7(),
        };
        let foreign_page = call_tool(
            &state,
            foreign,
            "list_learning_items",
            json!({"source_id": completion.offer.source.id}),
        )
        .await
        .map_err(|_| std::io::Error::other("foreign MCP learning list failed"))?;
        assert_eq!(foreign_page["items"].as_array().map(Vec::len), Some(0));
        assert!(matches!(
            call_tool(
                &state,
                foreign,
                "submit_learning_answer",
                json!({
                    "session_id": learning_session.id,
                    "item_id": item.id,
                    "answer": {"type": "true_false", "value": true},
                    "self_check": null,
                    "elapsed_ms": 1,
                    "idempotency_key": "foreign-learning-answer"
                }),
            )
            .await,
            Err(ToolError::NotFound)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn streamable_http_initialize_matches_frozen_protocol(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let state = AppState::seeded();
        let owner_id = crate::read_repository(&state)
            .map_err(|_| std::io::Error::other("seeded repository unavailable"))?
            .materials
            .values()
            .next()
            .map(|material| material.owner_id)
            .ok_or("seeded material missing")?;
        let created = state
            .mcp_runtime()
            .create(
                owner_id,
                &CreateMcpConnectionRequest {
                    name: "Protocol test".to_owned(),
                    idempotency_key: "protocol-create-1".to_owned(),
                },
            )
            .await?;
        let response = crate::build_router_with_state(state)
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri("/mcp")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {}", created.token))
                    .body(Body::from(
                        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
                    ))?,
            )
            .await?;
        let bytes = axum::body::to_bytes(response.into_body(), MCP_INLINE_RESULT_MAX_BYTES).await?;
        let body: Value = serde_json::from_slice(&bytes)?;

        assert_eq!(body["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        Ok(())
    }
}
