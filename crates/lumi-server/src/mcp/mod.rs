//! MCP transport and connection-management composition seam.

use axum::Router;

use crate::AppState;

/// Return authenticated MCP connection-management routes under `/api/v1`.
///
/// Track C owns this contribution and the separate top-level `/mcp` transport.
pub(crate) fn management_routes() -> Router<AppState> {
    Router::new()
}

/// Return the top-level Streamable HTTP MCP transport contribution.
///
/// Contract Freeze 1 publishes DTO and schema snapshots only, so the transport
/// router remains empty until C1.
pub(crate) fn transport_routes() -> Router<AppState> {
    Router::new()
}
