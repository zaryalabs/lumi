//! Versioned, platform-independent AI contracts.
//!
//! This module is the Contract Freeze 1 boundary shared by the AI platform,
//! Web chat and MCP agent tracks. Persistence and provider-specific wire
//! formats intentionally remain outside `lumi-core`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use uuid::Uuid;
use zeroize::Zeroize;

use crate::{Anchor, DocumentRevisionId, MaterialId, TimestampMs, UserId};

/// Version of the frozen AI task and API contract.
pub const AI_CONTRACT_VERSION: &str = "ai.contract.v1";
/// Version of immutable explicit-context packs.
pub const AI_CONTEXT_PACK_SCHEMA_VERSION: &str = "ai-context-pack.v1";
/// Version of source citations.
pub const SOURCE_CITATION_SCHEMA_VERSION: &str = "source-citation.v1";
/// Version of the explicit-context limit policy.
pub const EXPLICIT_CONTEXT_LIMITS_VERSION: &str = "explicit-context-limits.v1";
/// Version of the first summary artifact payload.
pub const SUMMARY_ARTIFACT_SCHEMA_VERSION: &str = "summary-artifact.v1";
/// Version of the chapter brief summary prompt.
pub const SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION: &str = "summary.chapter.brief.v1";
/// Version of the chapter outline summary prompt.
pub const SUMMARY_CHAPTER_OUTLINE_PROMPT_VERSION: &str = "summary.chapter.outline.v1";
/// Version of the material brief summary prompt.
pub const SUMMARY_MATERIAL_BRIEF_PROMPT_VERSION: &str = "summary.material.brief.v1";
/// Version of the material outline summary prompt.
pub const SUMMARY_MATERIAL_OUTLINE_PROMPT_VERSION: &str = "summary.material.outline.v1";

/// Stable AI task identifier.
pub type AiTaskId = Uuid;
/// Stable AI execution attempt identifier.
pub type AiRunId = Uuid;
/// Stable immutable context-pack identifier.
pub type AiContextPackId = Uuid;
/// Stable AI artifact identifier.
pub type AiArtifactId = Uuid;
/// Stable conversation identifier.
pub type AiConversationId = Uuid;
/// Stable chat message identifier.
pub type AiMessageId = Uuid;
/// Stable provider generation identifier.
pub type AiGenerationId = Uuid;
/// Stable opaque claim identifier.
pub type AiClaimId = Uuid;

/// Validation failure at a frozen AI contract boundary.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AiContractError {
    /// A required string field is empty.
    #[error("field `{0}` must not be empty")]
    EmptyField(&'static str),
    /// A string field exceeds its contract limit.
    #[error("field `{field}` exceeds the {max_bytes} byte limit")]
    FieldTooLarge {
        /// Field name.
        field: &'static str,
        /// Maximum encoded UTF-8 byte length.
        max_bytes: usize,
    },
    /// A state transition is not part of the frozen state machine.
    #[error("invalid AI task transition from {from:?} to {to:?}")]
    InvalidTaskTransition {
        /// Current task state.
        from: AiTaskStatus,
        /// Requested task state.
        to: AiTaskStatus,
    },
    /// A contract or schema version is unsupported.
    #[error("unsupported contract version `{0}`")]
    UnsupportedVersion(String),
    /// A source scope is internally inconsistent.
    #[error("invalid source scope: {0}")]
    InvalidSourceScope(&'static str),
    /// A context pack violates a size, hash or citation invariant.
    #[error("invalid context pack: {0}")]
    InvalidContextPack(&'static str),
    /// A structured output does not match its declared schema.
    #[error("invalid structured output: {0}")]
    InvalidStructuredOutput(&'static str),
}

/// User-visible lifecycle of a durable AI task.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiTaskStatus {
    /// Waiting for an executor.
    Queued,
    /// Owned by an active execution attempt.
    Running,
    /// Waiting for explicit user input or configuration.
    NeedsInput,
    /// Completed with a published typed result.
    Succeeded,
    /// Exhausted or explicitly stopped after a typed failure.
    Failed,
    /// Cancelled by the user or policy.
    Cancelled,
}

impl AiTaskStatus {
    /// Return whether the transition belongs to the frozen task state machine.
    #[must_use]
    pub fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Running | Self::Cancelled)
                | (
                    Self::Running,
                    Self::Queued
                        | Self::NeedsInput
                        | Self::Succeeded
                        | Self::Failed
                        | Self::Cancelled
                )
                | (Self::NeedsInput, Self::Queued | Self::Cancelled)
                | (Self::Failed, Self::Queued)
        ) || self == next
    }

    /// Validate a requested task transition.
    ///
    /// # Errors
    ///
    /// Returns [`AiContractError::InvalidTaskTransition`] for an unsupported
    /// transition.
    pub fn validate_transition(self, next: Self) -> Result<(), AiContractError> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(AiContractError::InvalidTaskTransition {
                from: self,
                to: next,
            })
        }
    }
}

/// Lifecycle of one AI execution attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiRunStatus {
    /// Attempt was created but has not started.
    Pending,
    /// Attempt owns the current claim.
    Running,
    /// Attempt completed and published a result.
    Succeeded,
    /// Attempt failed without a published result.
    Failed,
    /// Attempt was cancelled.
    Cancelled,
    /// Claim expired or was explicitly released.
    Released,
}

/// Executor class for a task attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiExecutorKind {
    /// Lumi's internal provider worker.
    InternalProvider,
    /// An authenticated external MCP agent.
    McpAgent,
}

/// Lifecycle of a published typed artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiArtifactStatus {
    /// Newly generated result awaiting acceptance.
    Candidate,
    /// Result selected as the current value for its slot.
    Active,
    /// Candidate explicitly rejected by the user.
    Rejected,
    /// Former active result replaced by a newer revision.
    Superseded,
}

/// Author of an immutable artifact revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiArtifactAuthor {
    /// Generated by an AI executor.
    Ai,
    /// Edited or authored by the account user.
    User,
}

/// Summary scope supported by the first release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryScopeKind {
    /// One normalized content unit.
    Chapter,
    /// The full immutable material revision.
    Material,
}

/// Summary presentation supported by the first release.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryForm {
    /// Compact prose summary.
    Brief,
    /// Structured outline.
    Outline,
}

/// Explicit, revision-bound source scope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiSourceScope {
    /// Exact user selection plus bounded neighbouring blocks.
    Selection {
        /// Source material.
        material_id: MaterialId,
        /// Immutable source revision.
        revision_id: DocumentRevisionId,
        /// Source-backed exact selection.
        anchor: Box<Anchor>,
    },
    /// One normalized content unit.
    Chapter {
        /// Source material.
        material_id: MaterialId,
        /// Immutable source revision.
        revision_id: DocumentRevisionId,
        /// Stable unit identifier.
        scope_ref: String,
    },
    /// Full immutable material revision, processed in bounded sections.
    Material {
        /// Source material.
        material_id: MaterialId,
        /// Immutable source revision.
        revision_id: DocumentRevisionId,
    },
}

impl AiSourceScope {
    /// Source material identifier.
    #[must_use]
    pub fn material_id(&self) -> MaterialId {
        match self {
            Self::Selection { material_id, .. }
            | Self::Chapter { material_id, .. }
            | Self::Material { material_id, .. } => *material_id,
        }
    }

    /// Immutable source revision identifier.
    #[must_use]
    pub fn revision_id(&self) -> DocumentRevisionId {
        match self {
            Self::Selection { revision_id, .. }
            | Self::Chapter { revision_id, .. }
            | Self::Material { revision_id, .. } => *revision_id,
        }
    }

    /// Validate the source-specific invariants.
    ///
    /// # Errors
    ///
    /// Returns an error for empty chapter identifiers, revision-mismatched
    /// selection anchors or empty selection quotes.
    pub fn validate(&self) -> Result<(), AiContractError> {
        match self {
            Self::Selection {
                revision_id,
                anchor,
                ..
            } if anchor.revision_id != *revision_id => Err(AiContractError::InvalidSourceScope(
                "selection anchor revision mismatch",
            )),
            Self::Selection { anchor, .. } if anchor.quote.trim().is_empty() => Err(
                AiContractError::InvalidSourceScope("selection quote must not be empty"),
            ),
            Self::Chapter { scope_ref, .. } if scope_ref.trim().is_empty() => {
                Err(AiContractError::EmptyField("scope_ref"))
            }
            _ => Ok(()),
        }
    }
}

/// Frozen durable task projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiTask {
    /// Contract version used by this task.
    pub contract_version: String,
    /// Task identifier.
    #[serde(rename = "task_id")]
    pub id: AiTaskId,
    /// Owning account.
    pub owner_id: UserId,
    /// Extensible task kind validated against capabilities.
    pub kind: String,
    /// Expected typed result kind.
    pub result_kind: String,
    /// Explicit source scope.
    pub source_scope: AiSourceScope,
    /// Canonical, versioned task parameters.
    pub parameters: Value,
    /// Prompt template version.
    pub prompt_version: String,
    /// Structured output schema version.
    pub output_schema_version: String,
    /// User-visible lifecycle.
    pub status: AiTaskStatus,
    /// Versioned active-task deduplication key.
    pub dedupe_key: String,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
}

impl AiTask {
    /// Validate the frozen task projection.
    ///
    /// # Errors
    ///
    /// Returns a typed error when a version, required field, source scope or
    /// schema reference violates Contract Freeze 1.
    pub fn validate(&self) -> Result<(), AiContractError> {
        require_version(&self.contract_version, AI_CONTRACT_VERSION)?;
        require_non_empty("kind", &self.kind, 64)?;
        require_non_empty("result_kind", &self.result_kind, 64)?;
        require_non_empty("dedupe_key", &self.dedupe_key, 256)?;
        self.source_scope.validate()?;
        prompt_by_id(&self.prompt_version)
            .ok_or_else(|| AiContractError::UnsupportedVersion(self.prompt_version.clone()))?;
        output_schema_by_id(&self.output_schema_version).ok_or_else(|| {
            AiContractError::UnsupportedVersion(self.output_schema_version.clone())
        })?;
        if self.object_revision == 0 {
            return Err(AiContractError::InvalidStructuredOutput(
                "object_revision must be positive",
            ));
        }
        Ok(())
    }
}

/// Frozen projection of one technical AI execution attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiRun {
    /// Attempt identifier.
    pub id: AiRunId,
    /// Parent durable task.
    pub task_id: AiTaskId,
    /// Executor class.
    pub executor_kind: AiExecutorKind,
    /// Non-secret executor reference.
    pub executor_ref: Option<String>,
    /// Attempt lifecycle.
    pub status: AiRunStatus,
    /// Current opaque claim, when running.
    pub claim_id: Option<AiClaimId>,
    /// Current monotonic fencing token.
    pub fence: Option<u64>,
    /// RFC 3339 lease expiry, when claimed.
    pub lease_expires_at: Option<String>,
    /// One-based attempt number.
    pub attempt: u32,
    /// Prompt template version.
    pub prompt_version: String,
    /// Expected output schema version.
    pub output_schema_version: String,
    /// Immutable context pack used by this attempt.
    pub context_pack_id: AiContextPackId,
    /// Provider kind used by an internal executor.
    pub provider_kind: Option<String>,
    /// Model used by an internal executor.
    pub model: Option<String>,
    /// Final provider usage.
    pub usage: Option<AiTokenUsage>,
    /// Normalized progress in the inclusive `0..=1` range.
    pub progress: f32,
    /// Redacted public error code.
    pub error_code: Option<String>,
    /// Start timestamp.
    pub started_at: TimestampMs,
    /// Finish timestamp.
    pub finished_at: Option<TimestampMs>,
}

/// Command used by HTTP, Web and MCP adapters to create one durable AI task.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateAiTaskCommand {
    /// Extensible task kind.
    pub kind: String,
    /// Explicit revision-bound source scope.
    pub source_scope: AiSourceScope,
    /// User instruction, treated as data by structured workflows.
    pub instruction: String,
    /// Canonical task parameters.
    pub parameters: Value,
    /// Prompt template version.
    pub prompt_version: String,
    /// Expected output schema version.
    pub output_schema_version: String,
    /// Request-level idempotency key.
    pub idempotency_key: String,
}

impl CreateAiTaskCommand {
    /// Maximum accepted instruction size.
    pub const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

    /// Validate the create command.
    ///
    /// # Errors
    ///
    /// Returns a typed contract error when a required field, scope or schema
    /// version is invalid.
    pub fn validate(&self) -> Result<(), AiContractError> {
        require_non_empty("kind", &self.kind, 64)?;
        require_non_empty(
            "instruction",
            &self.instruction,
            Self::MAX_INSTRUCTION_BYTES,
        )?;
        require_non_empty("idempotency_key", &self.idempotency_key, 256)?;
        self.source_scope.validate()?;
        prompt_by_id(&self.prompt_version)
            .ok_or_else(|| AiContractError::UnsupportedVersion(self.prompt_version.clone()))?;
        output_schema_by_id(&self.output_schema_version).ok_or_else(|| {
            AiContractError::UnsupportedVersion(self.output_schema_version.clone())
        })?;
        Ok(())
    }
}

/// Progress update accepted from either executor class.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateAiTaskProgressCommand {
    /// Task receiving the update.
    pub task_id: AiTaskId,
    /// Attempt receiving the update.
    pub run_id: AiRunId,
    /// Opaque current claim.
    pub claim_id: AiClaimId,
    /// Monotonic fencing token.
    pub fence: u64,
    /// Task revision observed at claim time.
    pub task_revision: u64,
    /// Normalized progress in the inclusive `0..=1` range.
    pub fraction: f32,
    /// Optional bounded stage label.
    pub stage: Option<String>,
}

/// Permission decision captured for audit in an immutable context pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiPermissionDecision {
    /// Actor was allowed to read the selected revision during pack creation.
    Allowed,
    /// Actor was denied; such a pack must never be issued to an executor.
    Denied,
}

/// Audit-only authorization snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiPermissionSnapshot {
    /// Actor checked while the pack was built.
    pub actor_id: UserId,
    /// Decision recorded at build time.
    pub decision: AiPermissionDecision,
    /// Version of the authorization policy.
    pub policy_version: String,
}

/// Format-specific citation locator independent of UI technology.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "snake_case")]
pub enum AiSourceLocator {
    /// EPUB source range.
    Epub {
        /// Canonical package-relative href.
        href: String,
        /// Optional UTF-8 byte start.
        byte_start: Option<usize>,
        /// Optional UTF-8 byte end.
        byte_end: Option<usize>,
    },
    /// Markdown source range.
    Markdown {
        /// Package-relative source path.
        file_path: String,
        /// UTF-8 byte start.
        byte_start: usize,
        /// UTF-8 byte end.
        byte_end: usize,
    },
    /// Portable `.lum` source range.
    Lum {
        /// Package-relative source path.
        file_path: String,
        /// UTF-8 byte start.
        byte_start: usize,
        /// UTF-8 byte end.
        byte_end: usize,
    },
    /// PDF page/text-layer range.
    Pdf {
        /// Zero-based physical page index.
        page_index: u32,
        /// User-visible page label.
        page_label: String,
        /// Optional first text block.
        text_block_start: Option<u32>,
        /// Optional last text block.
        text_block_end: Option<u32>,
    },
    /// Web snapshot locator.
    Web {
        /// Canonical source URL.
        canonical_url: String,
        /// Optional normalized block identifier.
        block_id: Option<String>,
    },
    /// Telegram message locator.
    Telegram {
        /// Stable source chat identifier encoded as text.
        chat_id: String,
        /// Source message identifier.
        message_id: i64,
    },
}

/// One bounded, ordered context fragment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiContextFragment {
    /// Citation identifier used by generated output.
    pub citation_id: String,
    /// Stable normalized unit identifier.
    pub unit_id: String,
    /// Stable normalized block identifier.
    pub block_id: String,
    /// Optional reader-facing label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Exact bounded UTF-8 content sent to the executor.
    pub text: String,
    /// Hash of exact fragment text.
    pub text_hash: String,
    /// Format-specific location in the immutable source.
    pub source_locator: AiSourceLocator,
}

/// Source-backed citation that a reader can reopen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceCitation {
    /// Citation contract version.
    pub schema_version: String,
    /// Opaque citation identifier unique within a context pack.
    pub citation_id: String,
    /// Source material.
    pub material_id: MaterialId,
    /// Immutable source revision.
    pub revision_id: DocumentRevisionId,
    /// Stable normalized unit identifier.
    pub unit_id: String,
    /// Stable normalized block identifier.
    pub block_id: String,
    /// Format-specific source location.
    pub source_locator: AiSourceLocator,
    /// Optional full source-backed reader anchor for exact navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Box<Anchor>>,
    /// Hash of the quoted text.
    pub quote_hash: String,
    /// Start byte within the corresponding context fragment.
    pub fragment_byte_start: usize,
    /// End byte within the corresponding context fragment.
    pub fragment_byte_end: usize,
}

/// Immutable explicit source context sent to an executor.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiContextPack {
    /// Context-pack schema version.
    pub schema_version: String,
    /// Pack identifier.
    pub context_pack_id: AiContextPackId,
    /// Owning account.
    pub owner_id: UserId,
    /// Durable task using this pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<AiTaskId>,
    /// Chat message using this pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<AiMessageId>,
    /// Explicit source scope.
    pub scope: AiSourceScope,
    /// Authorization audit snapshot.
    pub permission_snapshot: AiPermissionSnapshot,
    /// Applied limit profile.
    pub limits_version: String,
    /// Ordered exact fragments issued to the executor.
    pub fragments: Vec<AiContextFragment>,
    /// Optional expanded citation projections.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<SourceCitation>,
    /// Hash of the canonical pack content.
    pub pack_hash: String,
}

impl AiContextPack {
    /// Hard ceiling for exact fragment text in one pack.
    pub const MAX_TEXT_BYTES: usize = 256 * 1024;
    /// Hard ceiling for one exact fragment.
    pub const MAX_FRAGMENT_BYTES: usize = 16 * 1024;
    /// Hard ceiling for fragment count.
    pub const MAX_FRAGMENTS: usize = 128;

    /// Validate pack identity, permissions, bounds and citation references.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the pack is not safe to issue.
    pub fn validate(&self) -> Result<(), AiContractError> {
        require_version(&self.schema_version, AI_CONTEXT_PACK_SCHEMA_VERSION)?;
        require_version(&self.limits_version, EXPLICIT_CONTEXT_LIMITS_VERSION)?;
        self.scope.validate()?;
        if self.task_id.is_some() == self.message_id.is_some() {
            return Err(AiContractError::InvalidContextPack(
                "exactly one task_id or message_id is required",
            ));
        }
        if self.permission_snapshot.decision != AiPermissionDecision::Allowed
            || self.permission_snapshot.actor_id != self.owner_id
        {
            return Err(AiContractError::InvalidContextPack(
                "permission snapshot does not authorize owner",
            ));
        }
        if self.fragments.is_empty() || self.fragments.len() > Self::MAX_FRAGMENTS {
            return Err(AiContractError::InvalidContextPack(
                "fragment count is outside allowed bounds",
            ));
        }
        let mut total_bytes = 0usize;
        let mut citation_ids = std::collections::HashSet::new();
        for fragment in &self.fragments {
            require_non_empty("citation_id", &fragment.citation_id, 256)?;
            require_non_empty("unit_id", &fragment.unit_id, 512)?;
            require_non_empty("block_id", &fragment.block_id, 512)?;
            require_non_empty("text_hash", &fragment.text_hash, 256)?;
            if fragment.text.is_empty() || fragment.text.len() > Self::MAX_FRAGMENT_BYTES {
                return Err(AiContractError::InvalidContextPack(
                    "fragment text is empty or oversized",
                ));
            }
            total_bytes = total_bytes.saturating_add(fragment.text.len());
            if !citation_ids.insert(fragment.citation_id.as_str()) {
                return Err(AiContractError::InvalidContextPack(
                    "duplicate citation identifier",
                ));
            }
        }
        if total_bytes > Self::MAX_TEXT_BYTES {
            return Err(AiContractError::InvalidContextPack(
                "total text exceeds hard ceiling",
            ));
        }
        for citation in &self.citations {
            require_version(&citation.schema_version, SOURCE_CITATION_SCHEMA_VERSION)?;
            if !citation_ids.contains(citation.citation_id.as_str()) {
                return Err(AiContractError::InvalidContextPack(
                    "citation does not reference a fragment",
                ));
            }
            if citation.revision_id != self.scope.revision_id()
                || citation.material_id != self.scope.material_id()
            {
                return Err(AiContractError::InvalidContextPack(
                    "citation source does not match pack scope",
                ));
            }
        }
        require_non_empty("pack_hash", &self.pack_hash, 256)
    }
}

/// Provider credential state visible to a client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiCredentialState {
    /// No credential has been stored.
    Missing,
    /// Credential exists but has not been validated.
    Unvalidated,
    /// Last bounded validation succeeded.
    Valid,
    /// Last bounded validation failed.
    Invalid,
}

/// Provider capability projection without provider secrets.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiProviderDescriptor {
    /// Extensible provider kind.
    pub provider_kind: String,
    /// Reader-facing provider name.
    pub display_name: String,
    /// Enabled provider features.
    pub capabilities: Vec<String>,
    /// Server-approved models.
    pub allowed_models: Vec<String>,
    /// Credential lifecycle state.
    pub credential_state: AiCredentialState,
    /// Non-secret key fingerprint when configured.
    pub credential_fingerprint: Option<String>,
}

/// Write-only provider credential request.
#[derive(Serialize, Deserialize)]
pub struct PutProviderCredentialRequest {
    /// Plaintext provider credential accepted only on the authenticated write.
    pub credential: String,
    /// Model used for the bounded validation call.
    pub validation_model: String,
    /// Request-level idempotency key.
    pub idempotency_key: String,
}

impl std::fmt::Debug for PutProviderCredentialRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PutProviderCredentialRequest")
            .field("credential", &"[REDACTED]")
            .field("validation_model", &self.validation_model)
            .field("idempotency_key", &self.idempotency_key)
            .finish()
    }
}

impl Drop for PutProviderCredentialRequest {
    fn drop(&mut self) {
        self.credential.zeroize();
    }
}

/// Non-secret account preferences for one provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderPreferences {
    /// Default model selected for new operations.
    pub default_model: String,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
}

/// Revision-checked provider preference update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateProviderPreferencesRequest {
    /// New default model.
    pub default_model: String,
    /// Expected preference revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Bounded provider credential/model validation request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ValidateProviderRequest {
    /// Model to validate against.
    pub model: String,
}

/// Safe result of a bounded provider validation call.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderValidationResult {
    /// Whether authentication/model validation succeeded.
    pub valid: bool,
    /// Stable public result/error code.
    pub code: String,
    /// Validation timestamp.
    pub validated_at: TimestampMs,
}

/// Safe credential state returned by provider write/delete/validation routes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProviderCredentialState {
    /// Provider kind.
    pub provider_kind: String,
    /// Current safe lifecycle state.
    pub credential_state: AiCredentialState,
    /// Non-secret fingerprint, when configured.
    pub credential_fingerprint: Option<String>,
    /// Last validation timestamp, when attempted.
    pub last_validated_at: Option<TimestampMs>,
}

/// Provider-neutral chat request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiProviderChatRequest {
    /// Generation identifier used for event reconciliation.
    pub generation_id: AiGenerationId,
    /// Provider-neutral model identifier.
    pub model: String,
    /// Ordered messages.
    pub messages: Vec<AiProviderMessage>,
    /// Explicit context pack, if attached.
    pub context_pack: Option<AiContextPack>,
    /// Maximum generated token count.
    pub max_output_tokens: u32,
    /// Strictly validated provider-specific options.
    #[serde(default)]
    pub provider_options: Value,
}

/// One provider-neutral message.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiProviderMessage {
    /// Message role.
    pub role: AiMessageRole,
    /// Plain UTF-8 message content.
    pub content: String,
}

/// Chat message role.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiMessageRole {
    /// System policy supplied by Lumi.
    System,
    /// Account user input.
    User,
    /// Provider-generated response.
    Assistant,
}

/// Normalized finish reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiFinishReason {
    /// Provider reached a natural stop.
    Stop,
    /// Configured output bound was reached.
    Length,
    /// Generation was cancelled.
    Cancelled,
    /// Provider policy refused the request.
    ContentFilter,
}

/// Normalized provider usage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiTokenUsage {
    /// Tokens sent to the provider.
    pub input_tokens: u64,
    /// Tokens generated by the provider.
    pub output_tokens: u64,
}

/// Provider-neutral streaming event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AiProviderEvent {
    /// Provider accepted the request.
    ResponseStarted {
        /// Stable generation identifier.
        generation_id: String,
    },
    /// Ordered generated text fragment.
    TextDelta {
        /// Monotonic event sequence.
        sequence: u64,
        /// UTF-8 text fragment.
        text: String,
    },
    /// Provider usage update.
    Usage {
        /// Input tokens.
        input_tokens: u64,
        /// Output tokens.
        output_tokens: u64,
    },
    /// Successful terminal event.
    ResponseCompleted {
        /// Normalized finish reason.
        finish_reason: AiFinishReason,
    },
    /// Typed terminal failure.
    ResponseFailed {
        /// Stable public error code.
        code: String,
        /// Redacted user-safe message.
        message: String,
        /// Whether a later attempt may succeed.
        retryable: bool,
    },
}

/// Frozen summary artifact projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SummaryArtifact {
    /// Structured payload schema.
    pub schema_version: String,
    /// Artifact identifier.
    pub artifact_id: AiArtifactId,
    /// Originating task.
    pub task_id: AiTaskId,
    /// Originating attempt.
    pub run_id: AiRunId,
    /// Frozen artifact kind.
    pub kind: String,
    /// Artifact lifecycle.
    pub status: AiArtifactStatus,
    /// Immutable source revision.
    pub source_revision_id: DocumentRevisionId,
    /// Summary scope.
    pub scope_kind: SummaryScopeKind,
    /// Stable source scope reference.
    pub scope_ref: String,
    /// Summary presentation.
    pub form: SummaryForm,
    /// Validated summary content.
    pub content: String,
    /// Citations from the attempt's immutable context pack.
    pub citation_ids: Vec<String>,
    /// Artifact revision author.
    pub authored_by: AiArtifactAuthor,
    /// Immutable artifact revision number.
    pub artifact_revision: u64,
}

impl SummaryArtifact {
    /// Validate the first summary artifact schema.
    ///
    /// # Errors
    ///
    /// Returns an error for wrong versions/kinds or an incomplete source-backed
    /// result.
    pub fn validate(&self) -> Result<(), AiContractError> {
        require_version(&self.schema_version, SUMMARY_ARTIFACT_SCHEMA_VERSION)?;
        if self.kind != "summary_artifact" {
            return Err(AiContractError::InvalidStructuredOutput(
                "summary kind must be summary_artifact",
            ));
        }
        require_non_empty("scope_ref", &self.scope_ref, 512)?;
        require_non_empty("content", &self.content, 256 * 1024)?;
        if self.citation_ids.is_empty() {
            return Err(AiContractError::InvalidStructuredOutput(
                "source-backed summary requires citations",
            ));
        }
        if self.artifact_revision == 0 {
            return Err(AiContractError::InvalidStructuredOutput(
                "artifact_revision must be positive",
            ));
        }
        Ok(())
    }
}

/// Conversation lifecycle projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AiConversation {
    /// Conversation identifier.
    pub id: AiConversationId,
    /// Owning account.
    pub owner_id: UserId,
    /// User-visible title.
    pub title: String,
    /// Active model selection.
    pub active_model: String,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
    /// Soft deletion timestamp.
    pub deleted_at: Option<TimestampMs>,
}

/// Create a durable conversation without starting a generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateConversationRequest {
    /// Initial user-visible title.
    pub title: String,
    /// Initial provider-neutral model.
    pub active_model: String,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Revision-checked conversation metadata update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateConversationRequest {
    /// New title, when changed.
    pub title: Option<String>,
    /// New active model, when changed.
    pub active_model: Option<String>,
    /// Expected conversation revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Message persistence state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiMessageStatus {
    /// User message is durably stored.
    Committed,
    /// Assistant response is being generated.
    Streaming,
    /// Assistant response is complete.
    Completed,
    /// Assistant response ended with a normalized error.
    Failed,
    /// Assistant response was stopped.
    Cancelled,
}

/// Durable chat message projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiMessage {
    /// Message identifier.
    pub id: AiMessageId,
    /// Parent conversation.
    pub conversation_id: AiConversationId,
    /// Message role.
    pub role: AiMessageRole,
    /// Plain UTF-8 content.
    pub content: String,
    /// Explicit context attachments.
    pub attachments: Vec<AiContextAttachment>,
    /// Persistence/generation state.
    pub status: AiMessageStatus,
    /// Deterministic conversation-local sequence.
    pub sequence: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// Create one user message and its separate generation attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateMessageRequest {
    /// User-authored plain text.
    pub content: String,
    /// Explicit context attachments visible before send.
    pub attachments: Vec<AiContextAttachment>,
    /// Expected parent conversation revision.
    pub expected_conversation_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Durable result of message/generation creation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateMessageResponse {
    /// Committed user message.
    pub message: AiMessage,
    /// New generation attempt.
    pub generation: AiGeneration,
}

/// One conversation page with its ordered durable message history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConversationDetail {
    /// Conversation metadata.
    pub conversation: AiConversation,
    /// Ordered first/selected page of messages.
    pub messages: AiPage<AiMessage>,
    /// Current non-terminal generation, when present.
    pub active_generation: Option<AiGeneration>,
}

/// Explicit chat attachment preview.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiContextAttachment {
    /// Attachment kind such as `selection`, `chapter` or `material`.
    pub kind: String,
    /// Source material.
    pub material_id: MaterialId,
    /// Immutable source revision.
    pub revision_id: DocumentRevisionId,
    /// Explicit scope.
    pub scope: AiSourceScope,
    /// Reader-facing label.
    pub display_label: String,
}

/// Chat generation lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiGenerationStatus {
    /// Generation is queued behind the conversation's active attempt.
    Pending,
    /// Provider events are being relayed.
    Streaming,
    /// Generation completed.
    Completed,
    /// Generation failed.
    Failed,
    /// Generation was stopped.
    Cancelled,
}

/// One durable chat generation attempt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiGeneration {
    /// Generation identifier.
    pub id: AiGenerationId,
    /// Parent conversation.
    pub conversation_id: AiConversationId,
    /// Triggering user message.
    pub user_message_id: AiMessageId,
    /// Generated assistant message.
    pub assistant_message_id: AiMessageId,
    /// Attempt state.
    pub status: AiGenerationStatus,
    /// Provider kind.
    pub provider_kind: String,
    /// Model identifier.
    pub model: String,
    /// Final usage, when available.
    pub usage: Option<AiTokenUsage>,
    /// Redacted normalized error code.
    pub error_code: Option<String>,
    /// Start timestamp.
    pub started_at: TimestampMs,
    /// Finish timestamp.
    pub finished_at: Option<TimestampMs>,
}

/// Idempotent generation stop/retry/regenerate command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenerationMutationRequest {
    /// Expected generation status revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Generic immutable artifact envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiArtifact {
    /// Artifact identifier.
    pub id: AiArtifactId,
    /// Originating task.
    pub task_id: AiTaskId,
    /// Originating run.
    pub run_id: AiRunId,
    /// Extensible artifact kind.
    pub kind: String,
    /// Payload schema version.
    pub schema_version: String,
    /// Validated typed payload.
    pub payload: Value,
    /// Artifact lifecycle.
    pub status: AiArtifactStatus,
    /// Optimistic concurrency revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last update timestamp.
    pub updated_at: TimestampMs,
}

/// Revision-checked task execute/cancel/retry command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TaskMutationRequest {
    /// Expected task revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Bulk execute command with deterministic per-task results.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BulkExecuteTasksRequest {
    /// Ordered tasks to execute.
    pub task_ids: Vec<AiTaskId>,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// One deterministic result within a bulk task command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BulkTaskItemResult {
    /// Target task.
    pub task_id: AiTaskId,
    /// Stable success/error outcome code.
    pub outcome: String,
    /// Resulting task revision, when changed.
    pub object_revision: Option<u64>,
}

/// Deterministic response for a bulk task command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BulkTaskResult {
    /// Results in the same order as requested task ids.
    pub results: Vec<BulkTaskItemResult>,
}

/// Revision-checked artifact accept/reject command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactMutationRequest {
    /// Expected artifact revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Execution preference for summary/abridgement task creation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiExecutionMode {
    /// Leave the task for any eligible internal or MCP executor.
    Enqueue,
    /// Atomically hand the same durable task to the internal worker.
    ExecuteNow,
}

/// Material-scoped summary task request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateSummaryTaskRequest {
    /// Immutable source revision.
    pub source_revision_id: DocumentRevisionId,
    /// Chapter or material scope.
    pub scope_kind: SummaryScopeKind,
    /// Stable unit id for chapter scope; `material` for material scope.
    pub scope_ref: String,
    /// Brief prose or outline.
    pub form: SummaryForm,
    /// Queue or immediate internal execution.
    pub execution_mode: AiExecutionMode,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Revision-checked immutable user edit of a summary.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateSummaryRequest {
    /// New user-authored summary content.
    pub content: String,
    /// Expected artifact revision.
    pub expected_revision: u64,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Material-scoped abridgement task request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateAbridgementTaskRequest {
    /// Immutable source revision.
    pub source_revision_id: DocumentRevisionId,
    /// Target compression profile.
    pub profile: String,
    /// Queue or immediate internal execution.
    pub execution_mode: AiExecutionMode,
    /// Request idempotency key.
    pub idempotency_key: String,
}

/// Cursor-based page shared by AI list routes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiPage<T> {
    /// Deterministically ordered items.
    pub items: Vec<T>,
    /// Opaque cursor for the next page.
    pub next_cursor: Option<String>,
}

/// HTTP route metadata frozen for independent route-module implementation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiHttpRouteContract {
    /// HTTP method.
    pub method: &'static str,
    /// Versioned API path.
    pub path: &'static str,
    /// Stable request contract name, if a body exists.
    pub request: Option<&'static str>,
    /// Stable response contract name.
    pub response: &'static str,
}

/// Return the complete Contract Freeze 1 AI/MCP-management HTTP surface.
#[must_use]
pub fn ai_http_route_contracts() -> &'static [AiHttpRouteContract] {
    &AI_HTTP_ROUTES
}

/// Versioned prompt descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AiPromptDescriptor {
    /// Stable prompt version identifier.
    pub version: &'static str,
    /// Task kind using the prompt.
    pub task_kind: &'static str,
    /// Supported source scope.
    pub scope: SummaryScopeKind,
    /// Supported summary presentation.
    pub form: SummaryForm,
}

/// Versioned structured-output schema descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct AiOutputSchemaDescriptor {
    /// Stable output schema version.
    pub version: &'static str,
    /// Stable artifact kind produced by the schema.
    pub artifact_kind: &'static str,
    /// Strict JSON Schema supplied to compatible providers.
    pub schema: Value,
    /// Maximum canonical serialized payload size.
    pub max_payload_bytes: usize,
}

/// Return the frozen prompt registry.
#[must_use]
pub fn ai_prompt_registry() -> &'static [AiPromptDescriptor] {
    &PROMPT_REGISTRY
}

/// Return the frozen structured-output registry.
#[must_use]
pub fn ai_output_schema_registry() -> Vec<AiOutputSchemaDescriptor> {
    vec![AiOutputSchemaDescriptor {
        version: SUMMARY_ARTIFACT_SCHEMA_VERSION,
        artifact_kind: "summary_artifact",
        schema: json!({
            "$id": SUMMARY_ARTIFACT_SCHEMA_VERSION,
            "type": "object",
            "required": ["schema_version", "content", "citation_ids"],
            "properties": {
                "schema_version": {"const": SUMMARY_ARTIFACT_SCHEMA_VERSION},
                "content": {"type": "string", "minLength": 1},
                "citation_ids": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"type": "string", "minLength": 1}
                }
            },
            "additionalProperties": false
        }),
        max_payload_bytes: 256 * 1024,
    }]
}

/// Validate a provider/MCP structured result against a frozen output schema.
///
/// # Errors
///
/// Returns a typed error when the schema version is unknown, the serialized
/// payload is oversized or the required shape is invalid.
pub fn validate_ai_output(schema_version: &str, payload: &Value) -> Result<(), AiContractError> {
    let descriptor = output_schema_by_id(schema_version)
        .ok_or_else(|| AiContractError::UnsupportedVersion(schema_version.to_owned()))?;
    let encoded = serde_json::to_vec(payload)
        .map_err(|_| AiContractError::InvalidStructuredOutput("payload is not JSON"))?;
    if encoded.len() > descriptor.max_payload_bytes {
        return Err(AiContractError::InvalidStructuredOutput(
            "payload exceeds schema limit",
        ));
    }
    let object = payload
        .as_object()
        .ok_or(AiContractError::InvalidStructuredOutput(
            "payload must be an object",
        ))?;
    if object.len() != 3
        || object.get("schema_version").and_then(Value::as_str)
            != Some(SUMMARY_ARTIFACT_SCHEMA_VERSION)
        || object
            .get("content")
            .and_then(Value::as_str)
            .is_none_or(|content| content.trim().is_empty())
        || object
            .get("citation_ids")
            .and_then(Value::as_array)
            .is_none_or(|citations| {
                citations.is_empty()
                    || citations
                        .iter()
                        .any(|citation| citation.as_str().is_none_or(str::is_empty))
            })
    {
        return Err(AiContractError::InvalidStructuredOutput(
            "payload does not match summary-artifact.v1",
        ));
    }
    Ok(())
}

fn prompt_by_id(version: &str) -> Option<&'static AiPromptDescriptor> {
    PROMPT_REGISTRY
        .iter()
        .find(|descriptor| descriptor.version == version)
}

fn output_schema_by_id(version: &str) -> Option<AiOutputSchemaDescriptor> {
    ai_output_schema_registry()
        .into_iter()
        .find(|descriptor| descriptor.version == version)
}

fn require_version(actual: &str, expected: &str) -> Result<(), AiContractError> {
    if actual == expected {
        Ok(())
    } else {
        Err(AiContractError::UnsupportedVersion(actual.to_owned()))
    }
}

fn require_non_empty(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), AiContractError> {
    if value.trim().is_empty() {
        Err(AiContractError::EmptyField(field))
    } else if value.len() > max_bytes {
        Err(AiContractError::FieldTooLarge { field, max_bytes })
    } else {
        Ok(())
    }
}

const PROMPT_REGISTRY: [AiPromptDescriptor; 4] = [
    AiPromptDescriptor {
        version: SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION,
        task_kind: "summary",
        scope: SummaryScopeKind::Chapter,
        form: SummaryForm::Brief,
    },
    AiPromptDescriptor {
        version: SUMMARY_CHAPTER_OUTLINE_PROMPT_VERSION,
        task_kind: "summary",
        scope: SummaryScopeKind::Chapter,
        form: SummaryForm::Outline,
    },
    AiPromptDescriptor {
        version: SUMMARY_MATERIAL_BRIEF_PROMPT_VERSION,
        task_kind: "summary",
        scope: SummaryScopeKind::Material,
        form: SummaryForm::Brief,
    },
    AiPromptDescriptor {
        version: SUMMARY_MATERIAL_OUTLINE_PROMPT_VERSION,
        task_kind: "summary",
        scope: SummaryScopeKind::Material,
        form: SummaryForm::Outline,
    },
];

const AI_HTTP_ROUTES: [AiHttpRouteContract; 31] = [
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/providers",
        request: None,
        response: "AiPage<AiProviderDescriptor>",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/providers/openrouter",
        request: None,
        response: "AiProviderDescriptor",
    },
    AiHttpRouteContract {
        method: "PUT",
        path: "/api/v1/providers/openrouter/credential",
        request: Some("PutProviderCredentialRequest"),
        response: "ProviderCredentialState",
    },
    AiHttpRouteContract {
        method: "DELETE",
        path: "/api/v1/providers/openrouter/credential",
        request: None,
        response: "NoContent",
    },
    AiHttpRouteContract {
        method: "PATCH",
        path: "/api/v1/providers/openrouter/preferences",
        request: Some("UpdateProviderPreferencesRequest"),
        response: "ProviderPreferences",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/providers/openrouter/validate",
        request: Some("ValidateProviderRequest"),
        response: "ProviderValidationResult",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/ai/conversations",
        request: None,
        response: "AiPage<AiConversation>",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/conversations",
        request: Some("CreateConversationRequest"),
        response: "AiConversation",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/ai/conversations/{conversation_id}",
        request: None,
        response: "ConversationDetail",
    },
    AiHttpRouteContract {
        method: "PATCH",
        path: "/api/v1/ai/conversations/{conversation_id}",
        request: Some("UpdateConversationRequest"),
        response: "AiConversation",
    },
    AiHttpRouteContract {
        method: "DELETE",
        path: "/api/v1/ai/conversations/{conversation_id}",
        request: None,
        response: "NoContent",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/conversations/{conversation_id}/messages",
        request: Some("CreateMessageRequest"),
        response: "CreateMessageResponse",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/ai/generations/{generation_id}/events",
        request: None,
        response: "AiProviderEventStream",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/generations/{generation_id}/stop",
        request: Some("GenerationMutationRequest"),
        response: "AiGeneration",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/generations/{generation_id}/retry",
        request: Some("GenerationMutationRequest"),
        response: "AiGeneration",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/generations/{generation_id}/regenerate",
        request: Some("GenerationMutationRequest"),
        response: "AiGeneration",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/ai/tasks",
        request: None,
        response: "AiPage<AiTask>",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/tasks",
        request: Some("CreateAiTaskCommand"),
        response: "AiTask",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/ai/tasks/{task_id}",
        request: None,
        response: "AiTask",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/tasks/{task_id}/execute",
        request: Some("TaskMutationRequest"),
        response: "AiTask",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/tasks/{task_id}/cancel",
        request: Some("TaskMutationRequest"),
        response: "AiTask",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/tasks/{task_id}/retry",
        request: Some("TaskMutationRequest"),
        response: "AiTask",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/tasks/bulk-execute",
        request: Some("BulkExecuteTasksRequest"),
        response: "BulkTaskResult",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/ai/artifacts/{artifact_id}",
        request: None,
        response: "AiArtifact",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/artifacts/{artifact_id}/accept",
        request: Some("ArtifactMutationRequest"),
        response: "AiArtifact",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/ai/artifacts/{artifact_id}/reject",
        request: Some("ArtifactMutationRequest"),
        response: "AiArtifact",
    },
    AiHttpRouteContract {
        method: "GET",
        path: "/api/v1/materials/{material_id}/summaries",
        request: None,
        response: "AiPage<SummaryArtifact>",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/materials/{material_id}/summary-tasks",
        request: Some("CreateSummaryTaskRequest"),
        response: "AiTask",
    },
    AiHttpRouteContract {
        method: "PATCH",
        path: "/api/v1/ai/summaries/{summary_id}",
        request: Some("UpdateSummaryRequest"),
        response: "SummaryArtifact",
    },
    AiHttpRouteContract {
        method: "DELETE",
        path: "/api/v1/ai/summaries/{summary_id}",
        request: None,
        response: "NoContent",
    },
    AiHttpRouteContract {
        method: "POST",
        path: "/api/v1/materials/{material_id}/abridgement-tasks",
        request: Some("CreateAbridgementTaskRequest"),
        response: "AiTask",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_scope() -> AiSourceScope {
        AiSourceScope::Chapter {
            material_id: MaterialId::now_v7(),
            revision_id: DocumentRevisionId::now_v7(),
            scope_ref: "chapter-1".to_owned(),
        }
    }

    #[test]
    fn task_state_machine_rejects_terminal_transition() {
        assert!(matches!(
            AiTaskStatus::Succeeded.validate_transition(AiTaskStatus::Running),
            Err(AiContractError::InvalidTaskTransition { .. })
        ));
    }

    #[test]
    fn create_task_rejects_unknown_schema_version() {
        let command = CreateAiTaskCommand {
            kind: "summary".to_owned(),
            source_scope: fixture_scope(),
            instruction: "Сделай краткое резюме.".to_owned(),
            parameters: json!({"form": "brief"}),
            prompt_version: SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION.to_owned(),
            output_schema_version: "summary-artifact.v999".to_owned(),
            idempotency_key: "fixture-create".to_owned(),
        };

        assert!(matches!(
            command.validate(),
            Err(AiContractError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn output_validator_rejects_unreferenced_shape() {
        let payload = json!({
            "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
            "content": "Без ссылок",
            "citation_ids": []
        });

        assert!(matches!(
            validate_ai_output(SUMMARY_ARTIFACT_SCHEMA_VERSION, &payload),
            Err(AiContractError::InvalidStructuredOutput(_))
        ));
    }

    #[test]
    fn registries_have_unique_version_ids() {
        let prompt_versions = ai_prompt_registry()
            .iter()
            .map(|descriptor| descriptor.version)
            .collect::<std::collections::HashSet<_>>();
        let output_versions = ai_output_schema_registry()
            .iter()
            .map(|descriptor| descriptor.version)
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(prompt_versions.len(), ai_prompt_registry().len());
        assert_eq!(output_versions.len(), ai_output_schema_registry().len());
    }

    #[test]
    fn http_contracts_do_not_duplicate_method_and_path() {
        let unique = ai_http_route_contracts()
            .iter()
            .map(|route| (route.method, route.path))
            .collect::<std::collections::HashSet<_>>();

        assert_eq!(unique.len(), ai_http_route_contracts().len());
    }

    #[test]
    fn frozen_ai_json_fixtures_parse_and_validate() -> Result<(), Box<dyn std::error::Error>> {
        let task: AiTask = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/ai-task.json"
        ))?;
        let context: AiContextPack = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/context-pack.json"
        ))?;
        let events: Vec<AiProviderEvent> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/provider-events.json"
        ))?;
        let summary: SummaryArtifact = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/summary-artifact.json"
        ))?;

        task.validate()?;
        context.validate()?;
        summary.validate()?;
        assert_eq!(events.len(), 5);
        Ok(())
    }

    #[test]
    fn frozen_registry_and_http_snapshots_match_rust_catalogs(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let registry: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/schema-registry.json"
        ))?;
        let routes: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/ai/contracts/v1/http-routes.json"
        ))?;
        let prompt_versions = registry["prompts"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("prompt registry must be an array"))?
            .iter()
            .filter_map(|entry| entry["version"].as_str())
            .collect::<Vec<_>>();
        let output_versions = registry["outputs"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("output registry must be an array"))?
            .iter()
            .filter_map(|entry| entry["version"].as_str())
            .collect::<Vec<_>>();
        let route_specs = routes["routes"]
            .as_array()
            .ok_or_else(|| std::io::Error::other("route registry must be an array"))?
            .iter()
            .filter_map(|entry| {
                Some((
                    entry["method"].as_str()?,
                    entry["path"].as_str()?,
                    entry["request"].as_str(),
                    entry["response"].as_str()?,
                ))
            })
            .collect::<Vec<_>>();

        assert_eq!(
            prompt_versions,
            ai_prompt_registry()
                .iter()
                .map(|entry| entry.version)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            output_versions,
            ai_output_schema_registry()
                .iter()
                .map(|entry| entry.version)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            route_specs,
            ai_http_route_contracts()
                .iter()
                .map(|entry| (entry.method, entry.path, entry.request, entry.response))
                .collect::<Vec<_>>()
        );
        Ok(())
    }
}
