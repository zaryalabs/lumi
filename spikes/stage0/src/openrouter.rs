//! OpenRouter-compatible streaming and structured-output risk probe.

use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::time::{sleep, timeout};
use tower::ServiceExt;

const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Stable result of the local OpenRouter compatibility probe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenRouterProbeReport {
    /// Text reconstructed from ordinary SSE delta frames.
    pub streamed_text: String,
    /// Whether keep-alive comments and the terminal sentinel were accepted.
    pub framing_supported: bool,
    /// Whether strict structured output validation rejected extra fields.
    pub strict_schema_enforced: bool,
    /// Whether a slow request was cancelled by the caller deadline.
    pub cancellation_observed: bool,
    /// Stable error class mapped from a provider rate limit.
    pub mapped_rate_limit: &'static str,
}

/// Failures exposed by the OpenRouter compatibility probe.
#[derive(Debug, Error)]
pub enum OpenRouterProbeError {
    /// The local mock returned an unexpected HTTP status.
    #[error("unexpected mock HTTP status {0}")]
    Http(StatusCode),
    /// A response exceeded the configured output limit.
    #[error("provider response exceeds the configured output limit")]
    OutputTooLarge,
    /// An SSE data frame was malformed.
    #[error("malformed provider SSE frame")]
    MalformedFrame,
    /// The provider emitted an in-band terminal error.
    #[error("provider stream failed with {0}")]
    Provider(String),
    /// The structured result did not match the strict summary schema.
    #[error("structured output does not match summary.v1")]
    InvalidStructuredOutput,
}

/// Run deterministic requests against an in-process OpenRouter-compatible mock.
///
/// # Errors
///
/// Returns [`OpenRouterProbeError`] when framing, limits, error mapping or
/// structured output validation does not meet the accepted adapter boundary.
pub async fn run_openrouter_probe() -> Result<OpenRouterProbeReport, OpenRouterProbeError> {
    let app = mock_router();
    let stream = request_body(&app, "/stream").await?;
    let events = parse_chat_completion_sse(&stream)?;
    let streamed_text = events
        .iter()
        .filter_map(|event| event.get("choices"))
        .filter_map(Value::as_array)
        .filter_map(|choices| choices.first())
        .filter_map(|choice| choice.pointer("/delta/content"))
        .filter_map(Value::as_str)
        .collect::<String>();

    let structured = request_body(&app, "/structured").await?;
    let payload: Value =
        serde_json::from_slice(&structured).map_err(|_| OpenRouterProbeError::MalformedFrame)?;
    validate_summary(&payload)?;
    let strict_schema_enforced =
        validate_summary(&json!({"summary":"ok","citations":[],"extra":true})).is_err();

    let cancellation_observed = timeout(Duration::from_millis(10), request_body(&app, "/slow"))
        .await
        .is_err();
    let rate_limit = request(&app, "/rate-limit").await;
    let mapped_rate_limit = match rate_limit.status() {
        StatusCode::TOO_MANY_REQUESTS => "rate_limited",
        _ => return Err(OpenRouterProbeError::Http(rate_limit.status())),
    };

    Ok(OpenRouterProbeReport {
        streamed_text,
        framing_supported: events.len() == 2,
        strict_schema_enforced,
        cancellation_observed,
        mapped_rate_limit,
    })
}

fn mock_router() -> Router {
    Router::new()
        .route("/stream", post(stream_response))
        .route("/structured", post(structured_response))
        .route("/slow", post(slow_response))
        .route("/rate-limit", post(rate_limit_response))
}

async fn stream_response() -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .body(Body::from(
            ": OPENROUTER PROCESSING\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"При\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"вет\"},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn structured_response() -> Response {
    Response::new(Body::from(
        json!({
            "summary": "Краткий итог",
            "citations": ["ctx:fixture:1"]
        })
        .to_string(),
    ))
}

async fn slow_response() -> Response {
    sleep(Duration::from_secs(5)).await;
    Response::new(Body::from("{}"))
}

async fn rate_limit_response() -> Response {
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("retry-after", "1")
        .body(Body::from(
            json!({
                "error": {
                    "code": 429,
                    "message": "rate limit",
                    "metadata": {"error_type": "rate_limit_exceeded"}
                }
            })
            .to_string(),
        ))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn request(app: &Router, path: &str) -> Response {
    let request = Request::post(path)
        .body(Body::empty())
        .unwrap_or_else(|_| Request::new(Body::empty()));
    app.clone()
        .oneshot(request)
        .await
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

async fn request_body(app: &Router, path: &str) -> Result<Vec<u8>, OpenRouterProbeError> {
    let response = request(app, path).await;
    if !response.status().is_success() {
        return Err(OpenRouterProbeError::Http(response.status()));
    }
    let bytes = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
        .await
        .map_err(|_| OpenRouterProbeError::OutputTooLarge)?;
    Ok(bytes.to_vec())
}

fn parse_chat_completion_sse(source: &[u8]) -> Result<Vec<Value>, OpenRouterProbeError> {
    if source.len() > MAX_RESPONSE_BYTES {
        return Err(OpenRouterProbeError::OutputTooLarge);
    }
    let text = std::str::from_utf8(source).map_err(|_| OpenRouterProbeError::MalformedFrame)?;
    let mut events = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let data = line
            .strip_prefix("data:")
            .map(str::trim)
            .ok_or(OpenRouterProbeError::MalformedFrame)?;
        if data == "[DONE]" {
            break;
        }
        let event: Value =
            serde_json::from_str(data).map_err(|_| OpenRouterProbeError::MalformedFrame)?;
        if let Some(error_type) = event.pointer("/error/metadata/error_type") {
            return Err(OpenRouterProbeError::Provider(
                error_type.as_str().unwrap_or("unmapped").to_owned(),
            ));
        }
        events.push(event);
    }
    Ok(events)
}

fn validate_summary(payload: &Value) -> Result<(), OpenRouterProbeError> {
    let Some(object) = payload.as_object() else {
        return Err(OpenRouterProbeError::InvalidStructuredOutput);
    };
    if object.len() != 2
        || object.get("summary").and_then(Value::as_str).is_none()
        || object
            .get("citations")
            .and_then(Value::as_array)
            .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    {
        return Err(OpenRouterProbeError::InvalidStructuredOutput);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_mock_supports_required_adapter_behaviour() -> Result<(), OpenRouterProbeError> {
        let report = run_openrouter_probe().await?;

        assert_eq!(
            report,
            OpenRouterProbeReport {
                streamed_text: "Привет".to_owned(),
                framing_supported: true,
                strict_schema_enforced: true,
                cancellation_observed: true,
                mapped_rate_limit: "rate_limited",
            }
        );
        Ok(())
    }

    #[test]
    fn stream_parser_rejects_mid_stream_provider_error() {
        let source = br#"data: {"error":{"metadata":{"error_type":"provider_unavailable"}}}"#;

        let result = parse_chat_completion_sse(source);

        assert!(matches!(
            result,
            Err(OpenRouterProbeError::Provider(error)) if error == "provider_unavailable"
        ));
    }

    #[test]
    fn stream_parser_rejects_oversized_output() {
        let source = vec![b'x'; MAX_RESPONSE_BYTES + 1];

        let result = parse_chat_completion_sse(&source);

        assert!(matches!(result, Err(OpenRouterProbeError::OutputTooLarge)));
    }
}
