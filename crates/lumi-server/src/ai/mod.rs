//! Server-side AI contracts, replaceable interfaces and deterministic mocks.

use std::collections::HashMap;
use std::path::Path;

use sqlx_postgres::PgPool;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::secrets::{SecretStore, SecretStoreError};

pub mod chat;
pub mod context;
pub mod mock;
pub mod providers;
pub(crate) mod record_context;
pub mod repository;
pub(crate) mod routes;
pub(crate) mod tasks;

/// Production services and in-flight cancellation registry for E1.
pub struct AiRuntime {
    pool: PgPool,
    secrets: SecretStore,
    context: context::SourceContextResolver,
    provider_endpoint: String,
    transcription_endpoint: String,
    cancellations: Mutex<HashMap<Uuid, CancellationToken>>,
}

impl AiRuntime {
    /// Open account-scoped AI services and reconcile interrupted generations.
    ///
    /// # Errors
    ///
    /// Returns an error when the encrypted secret store or PostgreSQL state is
    /// unavailable.
    pub async fn open(
        pool: PgPool,
        secret_root: &Path,
        provider_endpoint: String,
        transcription_endpoint: String,
    ) -> Result<Self, SecretStoreError> {
        let secrets = SecretStore::open(pool.clone(), secret_root).await?;
        sqlx_core::query::query(
            "WITH interrupted AS (
                UPDATE ai_generations
                   SET status = 'failed',
                       error_code = 'server_restarted',
                       object_revision = object_revision + 1,
                       finished_at = now()
                 WHERE status IN ('pending', 'streaming')
                 RETURNING assistant_message_id, user_id
             )
             UPDATE ai_messages AS message
                SET status = 'failed'
               FROM interrupted
              WHERE message.message_id = interrupted.assistant_message_id
                AND message.user_id = interrupted.user_id",
        )
        .execute(&pool)
        .await
        .map_err(|_| SecretStoreError::Storage)?;
        Ok(Self {
            context: context::SourceContextResolver::new(pool.clone()),
            pool,
            secrets,
            provider_endpoint,
            transcription_endpoint,
            cancellations: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) fn secrets(&self) -> &SecretStore {
        &self.secrets
    }

    pub(crate) fn context(&self) -> &context::SourceContextResolver {
        &self.context
    }

    pub(crate) fn provider_endpoint(&self) -> &str {
        &self.provider_endpoint
    }

    pub(crate) fn transcription_endpoint(&self) -> &str {
        &self.transcription_endpoint
    }

    pub(crate) async fn register_cancellation(&self, generation_id: Uuid) -> CancellationToken {
        let token = CancellationToken::new();
        self.cancellations
            .lock()
            .await
            .insert(generation_id, token.clone());
        token
    }

    pub(crate) async fn cancel(&self, generation_id: Uuid) {
        if let Some(token) = self.cancellations.lock().await.get(&generation_id) {
            token.cancel();
        }
    }

    pub(crate) async fn clear_cancellation(&self, generation_id: Uuid) {
        self.cancellations.lock().await.remove(&generation_id);
    }
}

/// Readiness inputs used to fail closed when advertising AI capabilities.
///
/// A persistence-only stage deliberately advertises no product feature. Later
/// vertical slices set the delivery inputs only after their route, worker and
/// Web evidence exist.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AiCapabilityReadiness {
    /// Base AI tables and production repositories are available.
    pub persistence: bool,
    /// Common fenced job runtime is available.
    pub common_jobs: bool,
    /// Reusable encrypted secret store is available.
    pub secret_store: bool,
    /// Provider credential routes and adapter are production-ready.
    pub provider_delivery: bool,
    /// Explicit source context resolver is production-ready.
    pub explicit_context_delivery: bool,
    /// Task routes, worker and queue Web surface are production-ready.
    pub task_queue_delivery: bool,
    /// Summary workflow and Web surface are production-ready.
    pub summary_delivery: bool,
    /// Durable global chat route, persistence, provider path and Web surface
    /// are production-ready.
    pub chat_delivery: bool,
    /// Validated derived `.lum` workflow and ordinary-reader evidence exist.
    pub abridgement_delivery: bool,
    /// MCP transport and account authorization are production-ready.
    pub mcp_delivery: bool,
    /// MCP AI worker tools and fenced application-service integration are
    /// production-ready.
    pub mcp_worker_delivery: bool,
}

impl AiCapabilityReadiness {
    /// Readiness after `0.2.0/A1`: infrastructure exists, product features stay
    /// disabled.
    #[must_use]
    pub const fn a1_foundation() -> Self {
        Self {
            persistence: true,
            common_jobs: true,
            secret_store: true,
            provider_delivery: false,
            explicit_context_delivery: false,
            task_queue_delivery: false,
            summary_delivery: false,
            chat_delivery: false,
            abridgement_delivery: false,
            mcp_delivery: false,
            mcp_worker_delivery: false,
        }
    }

    /// Readiness after the complete `0.2.0/E1` vertical is wired.
    #[must_use]
    pub const fn e1_personal_assistant() -> Self {
        Self {
            persistence: true,
            common_jobs: true,
            secret_store: true,
            provider_delivery: true,
            explicit_context_delivery: true,
            task_queue_delivery: false,
            summary_delivery: false,
            chat_delivery: true,
            abridgement_delivery: false,
            mcp_delivery: false,
            mcp_worker_delivery: false,
        }
    }

    /// Readiness after the complete `0.2.0/E2` queue and summary vertical.
    #[must_use]
    pub const fn e2_tasks_and_summaries() -> Self {
        Self {
            persistence: true,
            common_jobs: true,
            secret_store: true,
            provider_delivery: true,
            explicit_context_delivery: true,
            task_queue_delivery: true,
            summary_delivery: true,
            chat_delivery: true,
            abridgement_delivery: false,
            mcp_delivery: false,
            mcp_worker_delivery: false,
        }
    }

    /// Readiness after `0.2.0/E3`: revocable MCP transport and worker tools.
    #[must_use]
    pub const fn e3_external_agents() -> Self {
        Self {
            persistence: true,
            common_jobs: true,
            secret_store: true,
            provider_delivery: true,
            explicit_context_delivery: true,
            task_queue_delivery: true,
            summary_delivery: true,
            chat_delivery: true,
            abridgement_delivery: false,
            mcp_delivery: true,
            mcp_worker_delivery: true,
        }
    }

    /// Readiness after `0.2.0/E4`: validated derived materials and release
    /// hardening complete the public AI/MCP vertical.
    #[must_use]
    pub const fn e4_release() -> Self {
        Self {
            persistence: true,
            common_jobs: true,
            secret_store: true,
            provider_delivery: true,
            explicit_context_delivery: true,
            task_queue_delivery: true,
            summary_delivery: true,
            chat_delivery: true,
            abridgement_delivery: true,
            mcp_delivery: true,
            mcp_worker_delivery: true,
        }
    }

    /// Return only product feature ids whose complete vertical prerequisites
    /// are present.
    #[must_use]
    pub fn advertised_feature_ids(self) -> Vec<String> {
        let mut features = Vec::new();
        if self.persistence && self.secret_store && self.provider_delivery {
            features.push("ai-provider-openrouter".to_owned());
            features.push("ai-provider-byok".to_owned());
        }
        if self.persistence && self.explicit_context_delivery {
            features.push("ai-explicit-context".to_owned());
        }
        if self.persistence && self.secret_store && self.provider_delivery && self.chat_delivery {
            features.push("ai-global-chat".to_owned());
        }
        if self.persistence && self.common_jobs && self.task_queue_delivery {
            features.push("ai-task-queue".to_owned());
        }
        if self.persistence
            && self.common_jobs
            && self.explicit_context_delivery
            && self.task_queue_delivery
            && self.summary_delivery
        {
            features.push("ai-summary-artifacts".to_owned());
        }
        if self.persistence
            && self.common_jobs
            && self.explicit_context_delivery
            && self.task_queue_delivery
            && self.abridgement_delivery
        {
            features.push("ai-abridged-lum".to_owned());
        }
        if self.mcp_delivery {
            features.push("mcp-account-agent".to_owned());
        }
        if self.persistence
            && self.common_jobs
            && self.explicit_context_delivery
            && self.task_queue_delivery
            && self.mcp_delivery
            && self.mcp_worker_delivery
        {
            features.push("mcp-ai-worker".to_owned());
        }
        features
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn a1_foundation_does_not_advertise_incomplete_ai_features() {
        let readiness = AiCapabilityReadiness::a1_foundation();

        assert!(readiness.persistence);
        assert!(readiness.common_jobs);
        assert!(readiness.secret_store);
        assert!(readiness.advertised_feature_ids().is_empty());
    }

    #[test]
    fn e1_advertises_only_the_complete_personal_assistant_vertical() {
        let readiness = AiCapabilityReadiness::e1_personal_assistant();

        assert_eq!(
            readiness.advertised_feature_ids(),
            vec![
                "ai-provider-openrouter",
                "ai-provider-byok",
                "ai-explicit-context",
                "ai-global-chat",
            ]
        );
    }

    #[test]
    fn e2_advertises_queue_and_summary_without_future_epics() {
        assert_eq!(
            AiCapabilityReadiness::e2_tasks_and_summaries().advertised_feature_ids(),
            vec![
                "ai-provider-openrouter",
                "ai-provider-byok",
                "ai-explicit-context",
                "ai-global-chat",
                "ai-task-queue",
                "ai-summary-artifacts",
            ]
        );
    }

    #[test]
    fn e3_advertises_external_agent_without_abridgement() {
        assert_eq!(
            AiCapabilityReadiness::e3_external_agents().advertised_feature_ids(),
            vec![
                "ai-provider-openrouter",
                "ai-provider-byok",
                "ai-explicit-context",
                "ai-global-chat",
                "ai-task-queue",
                "ai-summary-artifacts",
                "mcp-account-agent",
                "mcp-ai-worker",
            ]
        );
    }

    #[test]
    fn e4_advertises_validated_abridged_lum() {
        let features = AiCapabilityReadiness::e4_release().advertised_feature_ids();

        assert!(features.iter().any(|feature| feature == "ai-abridged-lum"));
        assert!(features.iter().any(|feature| feature == "mcp-ai-worker"));
    }

    #[test]
    fn rollout_requires_each_vertical_delivery_gate() {
        let mut readiness = AiCapabilityReadiness::a1_foundation();
        readiness.provider_delivery = true;
        assert_eq!(
            readiness.advertised_feature_ids(),
            vec!["ai-provider-openrouter", "ai-provider-byok"]
        );

        readiness.explicit_context_delivery = true;
        readiness.task_queue_delivery = true;
        readiness.summary_delivery = true;
        readiness.chat_delivery = true;
        readiness.abridgement_delivery = true;
        readiness.mcp_delivery = true;
        readiness.mcp_worker_delivery = true;
        assert_eq!(
            readiness.advertised_feature_ids(),
            vec![
                "ai-provider-openrouter",
                "ai-provider-byok",
                "ai-explicit-context",
                "ai-global-chat",
                "ai-task-queue",
                "ai-summary-artifacts",
                "ai-abridged-lum",
                "mcp-account-agent",
                "mcp-ai-worker",
            ]
        );
    }
}
