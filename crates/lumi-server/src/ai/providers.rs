//! Provider-neutral streaming and structured completion boundary.

use async_trait::async_trait;
use lumi_core::{AiProviderChatRequest, AiProviderEvent};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

/// Provider-neutral failure class safe for application-layer decisions.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AiProviderError {
    /// Account has no usable provider credential.
    #[error("provider credential is missing")]
    MissingCredential,
    /// Provider rejected authentication.
    #[error("provider authentication failed")]
    Authentication,
    /// Provider throttled the request.
    #[error("provider rate limit exceeded")]
    RateLimited,
    /// Request or response exceeded a configured bound.
    #[error("provider payload exceeded configured limits")]
    LimitExceeded,
    /// Provider did not complete before the configured deadline.
    #[error("provider request timed out")]
    Timeout,
    /// Caller cancelled the operation.
    #[error("provider request was cancelled")]
    Cancelled,
    /// Provider returned malformed or schema-invalid output.
    #[error("provider returned an invalid response")]
    InvalidResponse,
    /// Retryable upstream or transport failure.
    #[error("provider is temporarily unavailable")]
    Unavailable,
}

/// Receiver for normalized provider events.
pub struct AiProviderEventStream {
    receiver: mpsc::Receiver<Result<AiProviderEvent, AiProviderError>>,
}

impl AiProviderEventStream {
    /// Build a stream from its bounded channel receiver.
    #[must_use]
    pub fn new(receiver: mpsc::Receiver<Result<AiProviderEvent, AiProviderError>>) -> Self {
        Self { receiver }
    }

    /// Receive the next normalized event or terminal provider error.
    pub async fn recv(&mut self) -> Option<Result<AiProviderEvent, AiProviderError>> {
        self.receiver.recv().await
    }
}

/// Provider interface shared by chat and background task execution.
#[async_trait]
pub trait AiProviderClient: Send + Sync {
    /// Start one provider-neutral streaming chat request.
    async fn stream_chat(
        &self,
        request: AiProviderChatRequest,
    ) -> Result<AiProviderEventStream, AiProviderError>;

    /// Produce and validate one strict structured result.
    async fn complete_structured(
        &self,
        request: AiProviderChatRequest,
        output_schema_version: &str,
    ) -> Result<Value, AiProviderError>;
}
