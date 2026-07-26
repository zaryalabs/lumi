//! Server-side AI contracts, replaceable interfaces and deterministic mocks.

pub mod chat;
pub mod mock;
pub mod providers;
pub mod repository;
pub(crate) mod routes;

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
