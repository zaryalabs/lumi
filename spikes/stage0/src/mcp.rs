//! Stateless MCP Streamable HTTP and revocable-token risk probe.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use axum::body::{to_bytes, Body};
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const MAX_MCP_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
struct Connection {
    account_id: &'static str,
    revoked: bool,
}

#[derive(Clone, Default)]
struct McpState {
    connections: Arc<RwLock<HashMap<[u8; 32], Connection>>>,
}

/// Stable result of the MCP transport and revocation probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpProbeReport {
    /// Protocol version negotiated by the mock endpoint.
    pub protocol_version: String,
    /// Account id bound to the one successful capability call.
    pub account_id: String,
    /// Whether revocation was enforced on the next independent HTTP request.
    pub immediate_revocation: bool,
    /// Whether capability discovery exposed exactly the enabled Stage 0 tool.
    pub capability_tool_only: bool,
}

/// Run a deterministic Streamable HTTP request sequence with a revocable token.
pub async fn run_mcp_probe() -> McpProbeReport {
    let state = McpState::default();
    let token = "lumi_mcp_stage0_0123456789abcdef0123456789abcdef";
    state.add(token, "account-fixture");
    let app = Router::new()
        .route("/mcp", post(handle_mcp))
        .with_state(state.clone());

    let initialize = call(
        &app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"}
        }),
    )
    .await;
    let initialized: Value = response_json(initialize).await;

    let capabilities = call(
        &app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "get_lumi_capabilities", "arguments": {}}
        }),
    )
    .await;
    let capability_body: Value = response_json(capabilities).await;

    state.revoke(token);
    let revoked = call(
        &app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/list",
            "params": {}
        }),
    )
    .await;

    McpProbeReport {
        protocol_version: initialized
            .pointer("/result/protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        account_id: capability_body
            .pointer("/result/structuredContent/account_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        immediate_revocation: revoked.status() == StatusCode::UNAUTHORIZED,
        capability_tool_only: capability_body
            .pointer("/result/structuredContent/tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools.len() == 1
                    && tools.first().and_then(Value::as_str) == Some("get_lumi_capabilities")
            }),
    }
}

impl McpState {
    fn add(&self, token: &str, account_id: &'static str) {
        if let Ok(mut connections) = self.connections.write() {
            connections.insert(
                token_digest(token),
                Connection {
                    account_id,
                    revoked: false,
                },
            );
        }
    }

    fn revoke(&self, token: &str) {
        if let Ok(mut connections) = self.connections.write() {
            if let Some(connection) = connections.get_mut(&token_digest(token)) {
                connection.revoked = true;
            }
        }
    }

    fn authorize(&self, token: &str) -> Option<Connection> {
        self.connections
            .read()
            .ok()
            .and_then(|connections| connections.get(&token_digest(token)).cloned())
            .filter(|connection| !connection.revoked)
    }
}

async fn handle_mcp(State(state): State<McpState>, request: Request<Body>) -> Response {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let Some(connection) = token.and_then(|token| state.authorize(token)) else {
        return response(StatusCode::UNAUTHORIZED, json!({"error":"unauthorized"}));
    };

    let body = match to_bytes(request.into_body(), 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return response(
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({"error":"request_too_large"}),
            );
        }
    };
    let request: Value = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(_) => {
            return response(StatusCode::BAD_REQUEST, json!({"error":"invalid_json_rpc"}));
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let result = match request.get("method").and_then(Value::as_str) {
        Some("initialize") => json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {"listChanged": false}},
            "serverInfo": {"name": "lumi-stage0", "version": "0.1.0"}
        }),
        Some("tools/list") => json!({
            "tools": [{
                "name": "get_lumi_capabilities",
                "description": "Return enabled Lumi product capabilities.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            }]
        }),
        Some("tools/call")
            if request.pointer("/params/name").and_then(Value::as_str)
                == Some("get_lumi_capabilities") =>
        {
            json!({
                "content": [{"type":"text","text":"Lumi capabilities"}],
                "structuredContent": {
                    "account_id": connection.account_id,
                    "tools": ["get_lumi_capabilities"]
                }
            })
        }
        _ => {
            return response(
                StatusCode::OK,
                json!({
                    "jsonrpc":"2.0",
                    "id": id,
                    "error":{"code":-32601,"message":"Method not found"}
                }),
            );
        }
    };

    response(
        StatusCode::OK,
        json!({"jsonrpc":"2.0","id":id,"result":result}),
    )
}

fn response(status: StatusCode, body: Value) -> Response {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn call(app: &Router, token: &str, body: Value) -> Response {
    let request = Request::post("/mcp")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Request::new(Body::empty()));
    app.clone()
        .oneshot(request)
        .await
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn response_json(response: Response) -> Value {
    let body = to_bytes(response.into_body(), MAX_MCP_RESPONSE_BYTES)
        .await
        .unwrap_or_default();
    serde_json::from_slice(&body).unwrap_or(Value::Null)
}

fn token_digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn streamable_http_enforces_account_token_and_revocation() {
        let report = run_mcp_probe().await;

        assert_eq!(
            report,
            McpProbeReport {
                protocol_version: "2025-06-18".to_owned(),
                account_id: "account-fixture".to_owned(),
                immediate_revocation: true,
                capability_tool_only: true,
            }
        );
    }
}
