//! Platform-independent contracts for deterministic learning after reading.
//!
//! The module owns source identity, versioned items, immutable session
//! snapshots, append-only attempts and deterministic grading. Persistence,
//! HTTP and browser concerns stay outside `lumi-core`.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    content_hash, now_timestamp_ms, Anchor, DocumentRevisionId, MaterialId, TimestampMs, UserId,
};

/// Stable learning source identifier.
pub type LearningSourceId = Uuid;
/// Stable reading completion identifier.
pub type ReadingCompletionId = Uuid;
/// Stable learning item identifier.
pub type LearningItemId = Uuid;
/// Stable immutable item revision identifier.
pub type LearningItemRevisionId = Uuid;
/// Stable learning session identifier.
pub type LearningSessionId = Uuid;
/// Stable learning attempt identifier.
pub type LearningAttemptId = Uuid;
/// Stable generic audio attachment identifier.
pub type AudioAttachmentId = Uuid;
/// Stable bounded upload identifier.
pub type AudioUploadId = Uuid;
/// Stable immutable transcript revision identifier.
pub type TranscriptArtifactId = Uuid;

/// Maximum original audio accepted by the first voice contour (25 MiB).
pub const MAX_LEARNING_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
/// Supported browser and portable audio media types.
pub const LEARNING_AUDIO_MEDIA_TYPES: &[&str] = &[
    "audio/webm",
    "audio/ogg",
    "audio/mp4",
    "audio/mpeg",
    "audio/wav",
    "audio/x-wav",
];

/// Policy controlling the lifetime of original audio independently of transcripts.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioRetentionPolicy {
    /// Remove the source audio as soon as a reviewed transcript is accepted.
    DeleteAfterTranscript,
    /// Preserve source audio until an explicit owner deletion.
    KeepUntilDeleted,
}

/// Lifecycle of a bounded audio upload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioUploadStatus {
    /// Metadata exists but bytes have not been finalized.
    Pending,
    /// Bytes matched the declared checksum and length.
    Completed,
}

/// Owner-scoped upload metadata without raw audio bytes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AudioUpload {
    /// Stable upload identifier.
    pub id: AudioUploadId,
    /// Validated IANA media type.
    pub media_type: String,
    /// Exact expected byte length.
    pub byte_length: u64,
    /// Lowercase SHA-256 checksum.
    pub checksum_sha256: String,
    /// Current upload lifecycle.
    pub status: AudioUploadStatus,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// Command reserving an audio upload before bytes are transferred.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateAudioUploadCommand {
    /// Declared audio media type.
    pub media_type: String,
    /// Exact byte length.
    pub byte_length: u64,
    /// Lowercase SHA-256 checksum.
    pub checksum_sha256: String,
}

impl CreateAudioUploadCommand {
    /// Validate the bounded media type, length and checksum.
    pub fn validate(&self) -> Result<(), LearningValidationError> {
        if !LEARNING_AUDIO_MEDIA_TYPES.contains(&self.media_type.as_str()) {
            return Err(LearningValidationError::UnsupportedAudioMediaType);
        }
        if self.byte_length == 0 || self.byte_length > MAX_LEARNING_AUDIO_BYTES {
            return Err(LearningValidationError::InvalidAudioSize);
        }
        if self.checksum_sha256.len() != 64
            || !self
                .checksum_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(LearningValidationError::InvalidAudioChecksum);
        }
        Ok(())
    }
}

/// Generic original audio attachment metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AudioAttachment {
    /// Stable attachment identifier.
    pub id: AudioAttachmentId,
    /// Validated media type.
    pub media_type: String,
    /// Original byte length.
    pub byte_length: u64,
    /// Content checksum.
    pub checksum_sha256: String,
    /// Owner-selected retention policy.
    pub retention: AudioRetentionPolicy,
    /// Timestamp of logical source-audio deletion.
    pub audio_deleted_at: Option<TimestampMs>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// Command linking a completed generic upload to a learning session item.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateLearningAttachmentCommand {
    /// Completed owner-scoped upload.
    pub upload_id: AudioUploadId,
    /// Learning session receiving the answer.
    pub session_id: LearningSessionId,
    /// Immutable session item identity.
    pub item_id: LearningItemId,
    /// Original-audio retention choice.
    pub retention: AudioRetentionPolicy,
    /// Retry-safe mutation key.
    pub idempotency_key: String,
}

/// Retry-safe request for one new provider transcript revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscribeAudioCommand {
    /// Optional BCP 47 language hint passed to the transcription provider.
    #[serde(default)]
    pub language: Option<String>,
    /// Retry-safe mutation key.
    pub idempotency_key: String,
}

/// User-visible durable transcription lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    /// Waiting for an executor.
    Pending,
    /// Claimed by an active executor.
    Processing,
    /// Provider output is ready for user review.
    NeedsReview,
    /// A reviewed revision is approved for grading.
    Accepted,
    /// Provider execution failed safely.
    Failed,
    /// The owner cancelled transcription.
    Cancelled,
}

/// Immutable provider or user-edited transcript revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscriptArtifact {
    /// Stable revision identifier.
    pub id: TranscriptArtifactId,
    /// Source audio attachment.
    pub attachment_id: AudioAttachmentId,
    /// Monotonic attachment-local revision.
    pub revision: u32,
    /// Review lifecycle.
    pub status: TranscriptStatus,
    /// Transcript content; empty while pending.
    pub text: String,
    /// Provenance provider.
    pub provider: Option<String>,
    /// Provenance model.
    pub model: Option<String>,
    /// Detected or requested language.
    pub language: Option<String>,
    /// Revision creation timestamp.
    pub created_at: TimestampMs,
    /// Review acceptance timestamp.
    pub accepted_at: Option<TimestampMs>,
}

/// Command accepting a reviewed, optionally edited transcript revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceptTranscriptCommand {
    /// Reviewed transcript text used as the learning answer.
    pub text: String,
    /// Retry-safe mutation key.
    pub idempotency_key: String,
}

/// Request to generate source-backed learning drafts through the common AI queue.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GenerateLearningItemsRequest {
    /// Requested number of drafts, bounded by the server.
    pub item_count: u16,
    /// Whether to enqueue or immediately schedule the common internal worker.
    pub execution_mode: crate::AiExecutionMode,
    /// Request-level idempotency key.
    pub idempotency_key: String,
}

/// Request to evaluate one free-form answer without overwriting its attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EvaluateOpenAnswerRequest {
    /// Exact session containing the immutable item revision.
    pub session_id: LearningSessionId,
    /// Item answered by the user.
    pub item_id: LearningItemId,
    /// Submitted answer, treated as untrusted data by the prompt.
    pub answer: String,
    /// Queue or immediately schedule the common internal worker.
    pub execution_mode: crate::AiExecutionMode,
    /// Request-level idempotency key.
    pub idempotency_key: String,
}

/// Immutable source-backed AI evaluation linked to a learning session turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningAiEvaluation {
    /// Stable evaluation record.
    pub id: Uuid,
    /// Session containing the evaluated turn.
    pub session_id: LearningSessionId,
    /// Immutable session item identity.
    pub item_id: LearningItemId,
    /// Common AI task that produced the result.
    pub task_id: crate::AiTaskId,
    /// Typed artifact containing provider provenance.
    pub artifact_id: crate::AiArtifactId,
    /// Validated source-backed feedback.
    pub feedback: crate::OpenAnswerEvaluationPayload,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// One ordered assistance level attached to an immutable item revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningHint {
    /// Stable one-based reveal position.
    pub position: u16,
    /// User-visible assistance text.
    pub text: String,
    /// Optional exact source target for this assistance level.
    pub source_anchor: Option<Anchor>,
}

impl LearningHint {
    /// Validate the ordered assistance payload.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] when the position or text is empty.
    pub fn validate(&self) -> Result<(), LearningValidationError> {
        if self.position == 0 || self.text.trim().is_empty() {
            return Err(LearningValidationError::InvalidHint);
        }
        Ok(())
    }
}

/// Immutable source scope used by learning items and sessions.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningScopeKind {
    /// The complete immutable document revision.
    Material,
    /// One normalized content unit such as a chapter.
    ContentUnit,
    /// One exact source-backed anchor.
    Anchor,
}

/// Immutable source identity for learning data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningSource {
    /// Stable source id.
    pub id: LearningSourceId,
    /// Personal space containing the source.
    pub space_id: Uuid,
    /// Material containing the source.
    pub material_id: MaterialId,
    /// Exact immutable document revision.
    pub document_revision_id: DocumentRevisionId,
    /// Scope discriminator.
    pub scope_kind: LearningScopeKind,
    /// Deterministic deduplication key derived from revision and scope.
    pub scope_key: String,
    /// Normalized unit id for a content-unit scope.
    pub content_unit_id: Option<String>,
    /// Exact source anchor for an anchor scope.
    pub anchor: Option<Anchor>,
    /// Source revision hash.
    pub source_hash: String,
    /// Reader-facing source title.
    pub title: String,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

impl LearningSource {
    /// Build the stable hash used to deduplicate one revision-bound scope.
    #[must_use]
    pub fn scope_key(
        revision_id: DocumentRevisionId,
        scope_kind: LearningScopeKind,
        content_unit_id: Option<&str>,
        anchor: Option<&Anchor>,
    ) -> String {
        let anchor_json = anchor
            .and_then(|value| serde_json::to_string(value).ok())
            .unwrap_or_default();
        content_hash(
            format!(
                "{revision_id}:{scope_kind:?}:{}:{anchor_json}",
                content_unit_id.unwrap_or_default()
            )
            .as_bytes(),
        )
    }
}

/// Cause of an explicit reading completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadingCompletionTrigger {
    /// A reader adapter reached the semantic end of a scope.
    ReaderBoundary,
    /// The user explicitly marked the scope complete.
    ExplicitUserAction,
}

/// Durable completion fixed before any optional learning offer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadingCompletion {
    /// Stable completion id.
    pub id: ReadingCompletionId,
    /// Personal space containing this completion.
    pub space_id: Uuid,
    /// Account that completed the scope.
    pub user_id: UserId,
    /// Immutable source that was completed.
    pub source_id: LearningSourceId,
    /// Revision-bound completion generation.
    pub completion_generation: u32,
    /// Cause of completion.
    pub trigger: ReadingCompletionTrigger,
    /// Completion timestamp.
    pub completed_at: TimestampMs,
    /// Timestamp at which the optional offer was dismissed.
    pub offer_dismissed_at: Option<TimestampMs>,
    /// Timestamp at which this generation was first delivered to a reader.
    pub offer_presented_at: Option<TimestampMs>,
}

/// Command emitted by a semantic reader boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompleteReadingScopeCommand {
    /// Material containing the completed scope.
    pub material_id: MaterialId,
    /// Exact active revision rendered by the reader.
    pub revision_id: DocumentRevisionId,
    /// Completed scope kind.
    pub scope_kind: LearningScopeKind,
    /// Normalized unit id for a content-unit scope.
    pub content_unit_id: Option<String>,
    /// Source-backed anchor for an exact anchor scope.
    pub anchor: Option<Anchor>,
    /// Cause of completion.
    pub trigger: ReadingCompletionTrigger,
}

impl CompleteReadingScopeCommand {
    /// Validate the scope shape without relying on a DOM or platform handle.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] when scope-specific fields are
    /// missing, contradictory or target another revision.
    pub fn validate(&self) -> Result<(), LearningValidationError> {
        match self.scope_kind {
            LearningScopeKind::Material => {
                if self.content_unit_id.is_some() || self.anchor.is_some() {
                    return Err(LearningValidationError::InvalidScope);
                }
            }
            LearningScopeKind::ContentUnit => {
                if self
                    .content_unit_id
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                    || self.anchor.is_some()
                {
                    return Err(LearningValidationError::InvalidScope);
                }
            }
            LearningScopeKind::Anchor => {
                let anchor = self
                    .anchor
                    .as_ref()
                    .ok_or(LearningValidationError::InvalidScope)?;
                if self.content_unit_id.is_some()
                    || anchor.revision_id != self.revision_id
                    || anchor.node_path.is_empty()
                {
                    return Err(LearningValidationError::InvalidScope);
                }
            }
        }
        Ok(())
    }
}

/// One answer option with a stable identifier.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningOption {
    /// Stable option identifier within one item revision.
    pub id: String,
    /// User-visible option label.
    pub label: String,
}

/// Typed authoritative answer definition for one item revision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LearningAnswerSpec {
    /// Exactly one option is correct.
    SingleChoice {
        /// Available options.
        options: Vec<LearningOption>,
        /// Correct option id.
        correct_option_id: String,
    },
    /// One or more options are correct.
    MultipleChoice {
        /// Available options.
        options: Vec<LearningOption>,
        /// Complete set of correct option ids.
        correct_option_ids: Vec<String>,
    },
    /// Boolean answer.
    TrueFalse {
        /// Authoritative boolean value.
        correct: bool,
    },
    /// Open answer graded by explicit user self-check in deterministic mode.
    OpenSelfCheck {
        /// Sample answer shown only after recall.
        sample_answer: String,
    },
    /// Flashcard back shown only after recall.
    Flashcard {
        /// Card back.
        back: String,
    },
    /// Text with one or more accepted normalized answers.
    Cloze {
        /// Accepted answers after whitespace and case normalization.
        accepted_answers: Vec<String>,
    },
    /// Hinted question using deterministic self-check in the E1 slice.
    HintedSelfCheck {
        /// Sample answer shown after recall.
        sample_answer: String,
    },
    /// Explain-back prompt reserved for the later AI vertical.
    ExplainBack,
    /// Non-graded reflection prompt.
    Reflection,
}

impl LearningAnswerSpec {
    /// Validate internal option and answer invariants.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] for empty, duplicate or dangling
    /// answer definitions.
    pub fn validate(&self) -> Result<(), LearningValidationError> {
        match self {
            Self::SingleChoice {
                options,
                correct_option_id,
            } => {
                validate_options(options)?;
                if !options.iter().any(|option| option.id == *correct_option_id) {
                    return Err(LearningValidationError::UnknownCorrectOption);
                }
            }
            Self::MultipleChoice {
                options,
                correct_option_ids,
            } => {
                validate_options(options)?;
                let correct = correct_option_ids.iter().collect::<HashSet<_>>();
                if correct.is_empty()
                    || correct.len() != correct_option_ids.len()
                    || correct
                        .iter()
                        .any(|id| !options.iter().any(|option| option.id == ***id))
                {
                    return Err(LearningValidationError::UnknownCorrectOption);
                }
            }
            Self::OpenSelfCheck { sample_answer }
            | Self::Flashcard {
                back: sample_answer,
            }
            | Self::HintedSelfCheck { sample_answer } => {
                if sample_answer.trim().is_empty() {
                    return Err(LearningValidationError::EmptyAnswer);
                }
            }
            Self::Cloze { accepted_answers } => {
                if accepted_answers.is_empty()
                    || accepted_answers
                        .iter()
                        .any(|answer| answer.trim().is_empty())
                {
                    return Err(LearningValidationError::EmptyAnswer);
                }
            }
            Self::TrueFalse { .. } | Self::ExplainBack | Self::Reflection => {}
        }
        Ok(())
    }

    /// Return a presentation-safe answer shape without authoritative answers.
    #[must_use]
    pub fn presentation(&self) -> LearningAnswerPresentation {
        match self {
            Self::SingleChoice { options, .. } => LearningAnswerPresentation::SingleChoice {
                options: options.clone(),
            },
            Self::MultipleChoice { options, .. } => LearningAnswerPresentation::MultipleChoice {
                options: options.clone(),
            },
            Self::TrueFalse { .. } => LearningAnswerPresentation::TrueFalse,
            Self::OpenSelfCheck { .. } => LearningAnswerPresentation::OpenText,
            Self::Flashcard { .. } => LearningAnswerPresentation::Flashcard,
            Self::Cloze { .. } => LearningAnswerPresentation::Cloze,
            Self::HintedSelfCheck { .. } => LearningAnswerPresentation::OpenText,
            Self::ExplainBack => LearningAnswerPresentation::ExplainBack,
            Self::Reflection => LearningAnswerPresentation::Reflection,
        }
    }
}

fn validate_options(options: &[LearningOption]) -> Result<(), LearningValidationError> {
    let ids = options
        .iter()
        .map(|option| &option.id)
        .collect::<HashSet<_>>();
    if options.len() < 2
        || ids.len() != options.len()
        || options
            .iter()
            .any(|option| option.id.trim().is_empty() || option.label.trim().is_empty())
    {
        return Err(LearningValidationError::InvalidOptions);
    }
    Ok(())
}

/// Presentation-safe answer input rendered by a session client.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LearningAnswerPresentation {
    /// Radio-style option input.
    SingleChoice {
        /// Available options.
        options: Vec<LearningOption>,
    },
    /// Checkbox-style option input.
    MultipleChoice {
        /// Available options.
        options: Vec<LearningOption>,
    },
    /// Boolean input.
    TrueFalse,
    /// Free-form text followed by explicit self-check.
    OpenText,
    /// Flashcard reveal and self-check.
    Flashcard,
    /// Bounded text input.
    Cloze,
    /// Explain-back input reserved for the AI vertical.
    ExplainBack,
    /// Non-graded text reflection.
    Reflection,
}

/// Supported learning item family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningItemKind {
    /// Single-choice quiz.
    QuizSingleChoice,
    /// Multiple-choice quiz.
    QuizMultipleChoice,
    /// True/false quiz.
    QuizTrueFalse,
    /// Open question.
    OpenQuestion,
    /// Front/back flashcard.
    Flashcard,
    /// Cloze item.
    Cloze,
    /// Question with ordered hints.
    HintedQuestion,
    /// Explain-back prompt.
    ExplainBackPrompt,
    /// Non-graded reflection.
    ReflectionPrompt,
}

/// Lifecycle state of a learning item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningItemStatus {
    /// Editable item not selected for ordinary sessions.
    Draft,
    /// Item available to sessions.
    Active,
    /// Item retained in history but not selected.
    Archived,
    /// Invalid or explicitly rejected generated item.
    Rejected,
}

/// Origin of a learning item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningItemOrigin {
    /// Created by the account owner.
    User,
    /// Supplied by a material author.
    Author,
    /// Generated by an AI artifact workflow.
    AiGenerated,
    /// Converted from a personal annotation.
    ConvertedAnnotation,
}

/// Immutable editable payload shown in one session snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningItemRevision {
    /// Stable revision id.
    pub id: LearningItemRevisionId,
    /// Parent item id.
    pub item_id: LearningItemId,
    /// Monotonic revision number.
    pub revision: u64,
    /// User-visible prompt.
    pub prompt: String,
    /// Typed answer definition.
    pub answer_spec: LearningAnswerSpec,
    /// Feedback explanation shown after submission.
    pub explanation: String,
    /// Ordered assistance levels hidden until explicitly revealed.
    #[serde(default)]
    pub hints: Vec<LearningHint>,
    /// Optional exact source anchor.
    pub source_anchor: Option<Anchor>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

impl LearningItemRevision {
    /// Validate prompt, answer and source revision invariants.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] when the revision cannot be shown or
    /// graded safely.
    pub fn validate(
        &self,
        source_revision_id: DocumentRevisionId,
    ) -> Result<(), LearningValidationError> {
        if self.prompt.trim().is_empty() {
            return Err(LearningValidationError::EmptyPrompt);
        }
        self.answer_spec.validate()?;
        for (index, hint) in self.hints.iter().enumerate() {
            hint.validate()?;
            if usize::from(hint.position) != index.saturating_add(1)
                || hint
                    .source_anchor
                    .as_ref()
                    .is_some_and(|anchor| anchor.revision_id != source_revision_id)
            {
                return Err(LearningValidationError::InvalidHint);
            }
        }
        if self
            .source_anchor
            .as_ref()
            .is_some_and(|anchor| anchor.revision_id != source_revision_id)
        {
            return Err(LearningValidationError::SourceRevisionMismatch);
        }
        Ok(())
    }
}

/// Versioned learning item identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningItem {
    /// Stable item id.
    pub id: LearningItemId,
    /// Immutable source id.
    pub source_id: LearningSourceId,
    /// Exercise family.
    pub kind: LearningItemKind,
    /// Lifecycle state.
    pub status: LearningItemStatus,
    /// Item origin.
    pub origin: LearningItemOrigin,
    /// Current immutable revision.
    pub current_revision: LearningItemRevision,
    /// Optimistic object revision.
    pub object_revision: u64,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last metadata or revision update timestamp.
    pub updated_at: TimestampMs,
}

/// Command for creating a versioned item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateLearningItemCommand {
    /// Existing immutable source id.
    pub source_id: LearningSourceId,
    /// Exercise family.
    pub kind: LearningItemKind,
    /// Initial lifecycle state.
    pub status: LearningItemStatus,
    /// User-visible prompt.
    pub prompt: String,
    /// Typed answer definition.
    pub answer_spec: LearningAnswerSpec,
    /// Feedback explanation.
    pub explanation: String,
    /// Ordered assistance levels.
    #[serde(default)]
    pub hints: Vec<LearningHint>,
    /// Optional source anchor.
    pub source_anchor: Option<Anchor>,
}

/// Command for creating a new immutable revision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateLearningItemCommand {
    /// Expected optimistic object revision.
    pub expected_revision: u64,
    /// Replacement prompt.
    pub prompt: String,
    /// Replacement typed answer definition.
    pub answer_spec: LearningAnswerSpec,
    /// Replacement feedback explanation.
    pub explanation: String,
    /// Replacement ordered assistance levels.
    #[serde(default)]
    pub hints: Vec<LearningHint>,
    /// Replacement optional source anchor.
    pub source_anchor: Option<Anchor>,
}

/// Cursor-paginated item list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningItemPage {
    /// Items in stable id order.
    pub items: Vec<LearningItem>,
    /// Opaque cursor for the next page.
    pub next_cursor: Option<String>,
}

/// Item lifecycle mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ChangeLearningItemStatusCommand {
    /// Expected optimistic object revision.
    pub expected_revision: u64,
}

/// Optional learning offer projected from a durable completion.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningOffer {
    /// Completed immutable source.
    pub source: LearningSource,
    /// Durable completion created before this projection.
    pub completion: ReadingCompletion,
    /// Number of active source-matching items.
    pub active_item_count: usize,
    /// Whether the reader should display this generation automatically.
    pub should_offer: bool,
    /// Whether automatic offers remain enabled for the material.
    pub offers_enabled: bool,
}

/// Completion response returned after the completion write commits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompleteReadingResponse {
    /// Optional offer projected after the completion commit.
    pub offer: LearningOffer,
    /// Whether this command created the completion instead of deduplicating it.
    pub created: bool,
}

/// Reader offer action that does not affect reading progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningOfferAction {
    /// Dismiss this completion generation.
    NotNow,
    /// Dismiss this generation while preserving later scheduling options.
    RemindLater,
    /// Disable automatic completion offers for this material.
    DisableMaterialOffers,
}

/// Command for dismissing or disabling a learning offer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateLearningOfferCommand {
    /// Completion whose offer is being handled.
    pub completion_id: ReadingCompletionId,
    /// Selected action.
    pub action: LearningOfferAction,
}

/// Supported learning session kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningSessionKind {
    /// Immediate recall after reading.
    ImmediateRecall,
    /// Scheduled review reserved for E2.
    ScheduledReview,
    /// Explain-back reserved for E3.
    ExplainBack,
    /// User-launched practice.
    ManualPractice,
}

/// Durable learning session lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningSessionState {
    /// Session exists but has not started.
    Offered,
    /// Session has at least been opened.
    InProgress,
    /// User completed the bounded session.
    Completed,
    /// Optional offered session was dismissed.
    Dismissed,
    /// User explicitly stopped the session.
    Abandoned,
}

/// Immutable presentation snapshot for one session position.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningSessionItem {
    /// Stable item identity.
    pub item_id: LearningItemId,
    /// Exact immutable item revision shown.
    pub item_revision_id: LearningItemRevisionId,
    /// Stable zero-based order.
    pub position: u16,
    /// Exercise family.
    pub kind: LearningItemKind,
    /// User-visible prompt.
    pub prompt: String,
    /// Presentation-safe answer input.
    pub answer: LearningAnswerPresentation,
    /// Ordered assistance levels, revealed progressively by the client.
    #[serde(default)]
    pub hints: Vec<LearningHint>,
    /// Number of assistance levels already revealed in this session.
    #[serde(default)]
    pub revealed_hint_count: u16,
    /// Exact source anchor available for source navigation.
    pub source_anchor: Option<Anchor>,
}

/// User answer payload submitted to deterministic grading.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LearningAnswer {
    /// One selected option id.
    SingleChoice {
        /// Selected option id.
        option_id: String,
    },
    /// Selected option ids.
    MultipleChoice {
        /// Selected option ids.
        option_ids: Vec<String>,
    },
    /// Boolean answer.
    TrueFalse {
        /// Selected value.
        value: bool,
    },
    /// Free-form text answer.
    Text {
        /// Submitted text.
        text: String,
    },
    /// Explicit reveal without a textual answer, used by flashcards.
    Revealed,
}

/// Explicit deterministic self-check rating.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelfCheckRating {
    /// The recalled answer matched the revealed answer.
    Recalled,
    /// The recalled answer was materially incomplete.
    Partial,
    /// The answer was not recalled.
    NotRecalled,
}

/// Explicit FSRS review rating selected by the user.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningReviewRating {
    /// Recall failed; relearn soon.
    Again,
    /// Recall succeeded with substantial effort.
    Hard,
    /// Recall succeeded with expected effort.
    Good,
    /// Recall was immediate and unassisted.
    Easy,
}

/// Deterministic attempt outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningAttemptOutcome {
    /// Authoritative closed answer matched.
    Correct,
    /// Authoritative closed answer did not match.
    Incorrect,
    /// User supplied an explicit self-check rating.
    SelfChecked,
    /// Reflection or a later-capability item was intentionally not graded.
    NotGraded,
}

/// Feedback returned only after an answer has been submitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningFeedback {
    /// Deterministic outcome.
    pub outcome: LearningAttemptOutcome,
    /// Concise user-facing result.
    pub message: String,
    /// Correct option labels or sample answers revealed after recall.
    pub correct_answers: Vec<String>,
    /// Item explanation.
    pub explanation: String,
}

/// Append-only durable learning attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningAttempt {
    /// Stable attempt id.
    pub id: LearningAttemptId,
    /// Parent session.
    pub session_id: LearningSessionId,
    /// Item identity.
    pub item_id: LearningItemId,
    /// Exact immutable item revision shown.
    pub item_revision_id: LearningItemRevisionId,
    /// Submitted answer.
    pub answer: LearningAnswer,
    /// Optional explicit self-check rating.
    pub self_check: Option<SelfCheckRating>,
    /// Deterministic feedback.
    pub feedback: LearningFeedback,
    /// Client-measured elapsed time, bounded by the server.
    pub elapsed_ms: u64,
    /// Whether the source was opened before submission.
    pub source_opened: bool,
    /// Ordered hint positions revealed before submission.
    #[serde(default)]
    pub hints_used: Vec<u16>,
    /// Explicit or conservatively inferred scheduler rating.
    #[serde(default)]
    pub review_rating: Option<LearningReviewRating>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
}

/// Durable learning session with immutable items and append-only attempts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningSession {
    /// Stable session id.
    pub id: LearningSessionId,
    /// Account owning the session.
    pub user_id: UserId,
    /// Immutable learning source.
    pub source: LearningSource,
    /// Session kind.
    pub kind: LearningSessionKind,
    /// Lifecycle state.
    pub state: LearningSessionState,
    /// Immutable ordered item snapshots.
    pub items: Vec<LearningSessionItem>,
    /// Append-only attempts made in this session.
    pub attempts: Vec<LearningAttempt>,
    /// Creation timestamp.
    pub created_at: TimestampMs,
    /// Last lifecycle or attempt update timestamp.
    pub updated_at: TimestampMs,
}

impl LearningSession {
    /// Start an offered session idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] for terminal sessions.
    pub fn start(&mut self) -> Result<(), LearningValidationError> {
        match self.state {
            LearningSessionState::Offered | LearningSessionState::InProgress => {
                self.state = LearningSessionState::InProgress;
                self.updated_at = now_timestamp_ms();
                Ok(())
            }
            LearningSessionState::Completed
            | LearningSessionState::Dismissed
            | LearningSessionState::Abandoned => {
                Err(LearningValidationError::InvalidSessionTransition)
            }
        }
    }

    /// Complete a started session without grading unanswered items.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] unless the session is in progress.
    pub fn complete(&mut self) -> Result<(), LearningValidationError> {
        if self.state != LearningSessionState::InProgress {
            return Err(LearningValidationError::InvalidSessionTransition);
        }
        self.state = LearningSessionState::Completed;
        self.updated_at = now_timestamp_ms();
        Ok(())
    }

    /// Stop a non-terminal session without grading unanswered items.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] for terminal sessions.
    pub fn abandon(&mut self) -> Result<(), LearningValidationError> {
        if matches!(
            self.state,
            LearningSessionState::Completed
                | LearningSessionState::Dismissed
                | LearningSessionState::Abandoned
        ) {
            return Err(LearningValidationError::InvalidSessionTransition);
        }
        self.state = LearningSessionState::Abandoned;
        self.updated_at = now_timestamp_ms();
        Ok(())
    }
}

/// Command for creating a bounded source-backed session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateLearningSessionCommand {
    /// Source whose active items should be snapshotted.
    pub source_id: LearningSourceId,
    /// Session kind.
    pub kind: LearningSessionKind,
}

/// Command for one idempotent answer submission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubmitLearningAttemptCommand {
    /// Submitted answer.
    pub answer: LearningAnswer,
    /// Explicit rating required by self-check item families.
    pub self_check: Option<SelfCheckRating>,
    /// Client-measured elapsed time.
    pub elapsed_ms: u64,
    /// Explicit recall rating used by the replaceable scheduler.
    #[serde(default)]
    pub review_rating: Option<LearningReviewRating>,
}

/// Evidence command recorded when the user opens the source before answering.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordLearningSourceOpenedCommand {
    /// Item whose source was opened.
    pub item_id: LearningItemId,
}

/// Command for revealing exactly the next ordered assistance level.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RevealLearningHintCommand {
    /// Item whose next hint is being revealed.
    pub item_id: LearningItemId,
    /// Expected one-based position.
    pub position: u16,
}

/// Result of an ordered hint reveal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningHintReveal {
    /// Revealed assistance level.
    pub hint: LearningHint,
    /// Total assistance levels revealed for the item in this session.
    pub revealed_count: u16,
}

/// Deterministically grade one answer against an immutable item revision.
///
/// # Errors
///
/// Returns [`LearningValidationError`] for a mismatched answer shape, missing
/// self-check rating or empty text.
pub fn grade_learning_answer(
    revision: &LearningItemRevision,
    answer: &LearningAnswer,
    self_check: Option<SelfCheckRating>,
) -> Result<LearningFeedback, LearningValidationError> {
    revision.answer_spec.validate()?;
    let (outcome, correct_answers, message) = match (&revision.answer_spec, answer) {
        (
            LearningAnswerSpec::SingleChoice {
                options,
                correct_option_id,
            },
            LearningAnswer::SingleChoice { option_id },
        ) => {
            if !options.iter().any(|option| option.id == *option_id) {
                return Err(LearningValidationError::InvalidAnswer);
            }
            let correct = option_id == correct_option_id;
            (
                correctness_outcome(correct),
                option_labels(options, std::slice::from_ref(correct_option_id)),
                correctness_message(correct),
            )
        }
        (
            LearningAnswerSpec::MultipleChoice {
                options,
                correct_option_ids,
            },
            LearningAnswer::MultipleChoice { option_ids },
        ) => {
            let selected = option_ids.iter().collect::<HashSet<_>>();
            if selected.len() != option_ids.len()
                || selected
                    .iter()
                    .any(|id| !options.iter().any(|option| option.id == ***id))
            {
                return Err(LearningValidationError::InvalidAnswer);
            }
            let expected = correct_option_ids.iter().collect::<HashSet<_>>();
            let correct = selected == expected;
            (
                correctness_outcome(correct),
                option_labels(options, correct_option_ids),
                correctness_message(correct),
            )
        }
        (LearningAnswerSpec::TrueFalse { correct }, LearningAnswer::TrueFalse { value }) => {
            let is_correct = correct == value;
            (
                correctness_outcome(is_correct),
                vec![if *correct {
                    "Верно"
                } else {
                    "Неверно"
                }
                .to_owned()],
                correctness_message(is_correct),
            )
        }
        (LearningAnswerSpec::Cloze { accepted_answers }, LearningAnswer::Text { text }) => {
            let normalized = normalize_answer(text)?;
            let correct = accepted_answers
                .iter()
                .filter_map(|candidate| normalize_answer(candidate).ok())
                .any(|candidate| candidate == normalized);
            (
                correctness_outcome(correct),
                accepted_answers.clone(),
                correctness_message(correct),
            )
        }
        (
            LearningAnswerSpec::OpenSelfCheck { sample_answer }
            | LearningAnswerSpec::HintedSelfCheck { sample_answer },
            LearningAnswer::Text { text },
        ) => {
            normalize_answer(text)?;
            (
                self_checked_outcome(self_check)?,
                vec![sample_answer.clone()],
                "Самопроверка сохранена.".to_owned(),
            )
        }
        (LearningAnswerSpec::Flashcard { back }, LearningAnswer::Revealed) => (
            self_checked_outcome(self_check)?,
            vec![back.clone()],
            "Самопроверка сохранена.".to_owned(),
        ),
        (LearningAnswerSpec::Reflection, LearningAnswer::Text { text }) => {
            normalize_answer(text)?;
            (
                LearningAttemptOutcome::NotGraded,
                Vec::new(),
                "Ответ сохранён без оценки.".to_owned(),
            )
        }
        (LearningAnswerSpec::ExplainBack, LearningAnswer::Text { text }) => {
            normalize_answer(text)?;
            (
                LearningAttemptOutcome::NotGraded,
                Vec::new(),
                "Ответ сохранён без AI-оценки.".to_owned(),
            )
        }
        _ => return Err(LearningValidationError::InvalidAnswer),
    };

    Ok(LearningFeedback {
        outcome,
        message,
        correct_answers,
        explanation: revision.explanation.clone(),
    })
}

fn correctness_outcome(correct: bool) -> LearningAttemptOutcome {
    if correct {
        LearningAttemptOutcome::Correct
    } else {
        LearningAttemptOutcome::Incorrect
    }
}

fn correctness_message(correct: bool) -> String {
    if correct {
        "Верно.".to_owned()
    } else {
        "Ответ не совпал. Сверьтесь с источником и объяснением.".to_owned()
    }
}

fn self_checked_outcome(
    self_check: Option<SelfCheckRating>,
) -> Result<LearningAttemptOutcome, LearningValidationError> {
    self_check
        .map(|_| LearningAttemptOutcome::SelfChecked)
        .ok_or(LearningValidationError::SelfCheckRequired)
}

fn option_labels(options: &[LearningOption], ids: &[String]) -> Vec<String> {
    ids.iter()
        .filter_map(|id| {
            options
                .iter()
                .find(|option| option.id == *id)
                .map(|option| option.label.clone())
        })
        .collect()
}

fn normalize_answer(answer: &str) -> Result<String, LearningValidationError> {
    let normalized = answer.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        Err(LearningValidationError::EmptyAnswer)
    } else {
        Ok(normalized.to_lowercase())
    }
}

/// Durable scheduler lifecycle state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LearningScheduleState {
    /// Item has not completed its first recall.
    New,
    /// Item is in initial learning.
    Learning,
    /// Item is in ordinary spaced review.
    Review,
    /// Item is being relearned after a lapse.
    Relearning,
    /// Scheduling is paused without deleting history.
    Paused,
}

/// Versioned per-user schedule for one stable learning item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LearningSchedule {
    /// Stable item identity.
    pub item_id: LearningItemId,
    /// Current scheduler lifecycle state.
    pub state: LearningScheduleState,
    /// Next actionable UTC timestamp.
    pub due_at: TimestampMs,
    /// FSRS memory stability in days.
    pub stability: f64,
    /// FSRS difficulty on the 1–10 scale.
    pub difficulty: f64,
    /// Timestamp of the latest scheduled review.
    pub last_review_at: Option<TimestampMs>,
    /// Successful and failed scheduler updates.
    pub repetitions: u32,
    /// Count of `again` reviews after first exposure.
    pub lapses: u32,
    /// Stable algorithm identifier.
    pub algorithm: String,
    /// Exact parameter/mapping version.
    pub algorithm_version: String,
    /// Opaque versioned algorithm state for future migration.
    pub algorithm_payload: serde_json::Value,
    /// Optimistic object revision.
    pub object_revision: u64,
    /// Timestamp at which this item was paused.
    pub paused_at: Option<TimestampMs>,
}

/// Scheduling input contract implemented by the FSRS adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct SchedulerReview {
    /// Existing durable state, absent for the first exposure.
    pub previous: Option<LearningSchedule>,
    /// Explicit or conservatively inferred review rating.
    pub rating: LearningReviewRating,
    /// Attempt outcome.
    pub outcome: LearningAttemptOutcome,
    /// Explicit self-check rating, when present.
    pub self_check: Option<SelfCheckRating>,
    /// Whether source assistance was used.
    pub source_opened: bool,
    /// Ordered hint positions revealed before submission.
    pub hints_used: Vec<u16>,
    /// UTC timestamp at which recall was submitted.
    pub reviewed_at: TimestampMs,
}

/// Versioned scheduler output stored outside core.
#[derive(Clone, Debug, PartialEq)]
pub struct SchedulerDecision {
    /// Complete next durable schedule state.
    pub schedule: LearningSchedule,
    /// Conservative rating suggested from correctness and assistance evidence.
    pub suggested_rating: LearningReviewRating,
}

/// Replaceable scheduling boundary without PostgreSQL or UI dependencies.
pub trait Scheduler: Send + Sync {
    /// Produce the next schedule state from explicit review evidence.
    ///
    /// # Errors
    ///
    /// Implementations return [`LearningValidationError`] when evidence or
    /// algorithm state cannot be processed safely.
    fn review(&self, review: SchedulerReview)
        -> Result<SchedulerDecision, LearningValidationError>;
}

/// Account-wide learning scheduling settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningSettings {
    /// Whether automatic scheduling is enabled.
    pub scheduling_enabled: bool,
    /// Maximum number of items projected into `Сегодня`.
    pub daily_limit: u16,
    /// Whether due items are hidden until an explicit manual launch.
    pub manual_only: bool,
    /// Optimistic object revision.
    pub object_revision: u64,
}

impl Default for LearningSettings {
    fn default() -> Self {
        Self {
            scheduling_enabled: true,
            daily_limit: 20,
            manual_only: false,
            object_revision: 1,
        }
    }
}

/// Account-wide scheduling settings mutation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateLearningSettingsCommand {
    /// Expected optimistic object revision.
    pub expected_revision: u64,
    /// Whether automatic scheduling is enabled.
    pub scheduling_enabled: bool,
    /// Bounded daily item limit.
    pub daily_limit: u16,
    /// Whether review is available only through a manual launch.
    pub manual_only: bool,
}

impl UpdateLearningSettingsCommand {
    /// Validate safe projection bounds.
    ///
    /// # Errors
    ///
    /// Returns [`LearningValidationError`] when the daily limit is outside
    /// the supported lightweight session range.
    pub fn validate(&self) -> Result<(), LearningValidationError> {
        if !(1..=100).contains(&self.daily_limit) {
            return Err(LearningValidationError::InvalidDailyLimit);
        }
        Ok(())
    }
}

/// Per-source scheduling override and pause state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningSourceScheduleSettings {
    /// Immutable source identity.
    pub source_id: LearningSourceId,
    /// Whether this source participates in automatic scheduling.
    pub scheduling_enabled: bool,
    /// Pause marker that preserves history and excludes overdue projection.
    pub paused_at: Option<TimestampMs>,
    /// Optimistic object revision.
    pub object_revision: u64,
}

/// One source group in the bounded Challenges projection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningChallengeGroup {
    /// Immutable source identity.
    pub source_id: LearningSourceId,
    /// Material identity used by navigation and filtering.
    pub material_id: MaterialId,
    /// Reader-facing source title.
    pub title: String,
    /// Due item ids selected in stable due order.
    pub item_ids: Vec<LearningItemId>,
    /// Approximate completion time in minutes.
    pub estimated_minutes: u16,
    /// Whether the source is currently paused.
    pub paused: bool,
}

/// Counts kept separate from the bounded item payload.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningChallengeCounts {
    /// Due review items, excluding paused sources.
    pub due: u32,
    /// Active items without schedule history.
    pub ready: u32,
    /// Draft items awaiting activation.
    pub drafts: u32,
}

/// Bounded `Сегодня` projection used by Web and future MCP adapters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LearningToday {
    /// Current account scheduling settings.
    pub settings: LearningSettings,
    /// Selected source groups, together bounded by the daily limit.
    pub groups: Vec<LearningChallengeGroup>,
    /// Active unscheduled items available for immediate reinforcement.
    pub ready_groups: Vec<LearningChallengeGroup>,
    /// Unbounded category counts for clear empty states.
    pub counts: LearningChallengeCounts,
    /// Projection generation timestamp.
    pub generated_at: TimestampMs,
}

/// Snooze a bounded review session without recording fake failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnoozeLearningSessionCommand {
    /// Number of days to move unanswered items, bounded to 1–30.
    pub days: u16,
}

/// Validation errors shared by core, API and clients.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum LearningValidationError {
    /// Scope-specific fields are missing or contradictory.
    #[error("learning source scope is invalid")]
    InvalidScope,
    /// Prompt is empty.
    #[error("learning item prompt is empty")]
    EmptyPrompt,
    /// Answer or sample answer is empty.
    #[error("learning answer is empty")]
    EmptyAnswer,
    /// Options are empty, duplicated or malformed.
    #[error("learning options are invalid")]
    InvalidOptions,
    /// Authoritative answer refers to a missing option.
    #[error("correct answer refers to an unknown option")]
    UnknownCorrectOption,
    /// Source anchor targets another revision.
    #[error("source anchor targets another revision")]
    SourceRevisionMismatch,
    /// Submitted answer does not match the expected input shape.
    #[error("submitted answer is invalid for this item")]
    InvalidAnswer,
    /// Browser audio format is outside the explicit allow-list.
    #[error("unsupported audio media type")]
    UnsupportedAudioMediaType,
    /// Audio is empty or exceeds the bounded upload limit.
    #[error("invalid audio size")]
    InvalidAudioSize,
    /// Checksum is not a lowercase SHA-256 value.
    #[error("invalid audio checksum")]
    InvalidAudioChecksum,
    /// Open answer or flashcard requires an explicit self-check.
    #[error("self-check rating is required")]
    SelfCheckRequired,
    /// Session lifecycle transition is invalid.
    #[error("learning session transition is invalid")]
    InvalidSessionTransition,
    /// Hints must be non-empty and consecutively ordered from one.
    #[error("learning hints are invalid or out of order")]
    InvalidHint,
    /// Daily review limit is outside the supported range.
    #[error("learning daily limit must be between 1 and 100")]
    InvalidDailyLimit,
    /// Scheduler state or rating is invalid.
    #[error("learning scheduler state is invalid")]
    InvalidSchedule,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_upload_should_enforce_type_size_and_checksum() {
        let valid = CreateAudioUploadCommand {
            media_type: "audio/webm".to_owned(),
            byte_length: 42,
            checksum_sha256: "a".repeat(64),
        };
        assert_eq!(valid.validate(), Ok(()));
        assert_eq!(
            CreateAudioUploadCommand {
                media_type: "video/webm".to_owned(),
                ..valid.clone()
            }
            .validate(),
            Err(LearningValidationError::UnsupportedAudioMediaType)
        );
        assert_eq!(
            CreateAudioUploadCommand {
                byte_length: MAX_LEARNING_AUDIO_BYTES + 1,
                ..valid.clone()
            }
            .validate(),
            Err(LearningValidationError::InvalidAudioSize)
        );
        assert_eq!(
            CreateAudioUploadCommand {
                checksum_sha256: "NOT-A-HASH".to_owned(),
                ..valid
            }
            .validate(),
            Err(LearningValidationError::InvalidAudioChecksum)
        );
    }

    fn revision(answer_spec: LearningAnswerSpec) -> LearningItemRevision {
        LearningItemRevision {
            id: Uuid::now_v7(),
            item_id: Uuid::now_v7(),
            revision: 1,
            prompt: "Проверка".to_owned(),
            answer_spec,
            explanation: "Пояснение".to_owned(),
            hints: Vec::new(),
            source_anchor: None,
            created_at: 1,
        }
    }

    #[test]
    fn single_choice_grading_is_deterministic() -> Result<(), LearningValidationError> {
        let item = revision(LearningAnswerSpec::SingleChoice {
            options: vec![
                LearningOption {
                    id: "a".to_owned(),
                    label: "A".to_owned(),
                },
                LearningOption {
                    id: "b".to_owned(),
                    label: "B".to_owned(),
                },
            ],
            correct_option_id: "b".to_owned(),
        });

        let feedback = grade_learning_answer(
            &item,
            &LearningAnswer::SingleChoice {
                option_id: "b".to_owned(),
            },
            None,
        )?;

        assert_eq!(feedback.outcome, LearningAttemptOutcome::Correct);
        Ok(())
    }

    #[test]
    fn multiple_choice_requires_the_exact_set() -> Result<(), LearningValidationError> {
        let item = revision(LearningAnswerSpec::MultipleChoice {
            options: vec![
                LearningOption {
                    id: "a".to_owned(),
                    label: "A".to_owned(),
                },
                LearningOption {
                    id: "b".to_owned(),
                    label: "B".to_owned(),
                },
                LearningOption {
                    id: "c".to_owned(),
                    label: "C".to_owned(),
                },
            ],
            correct_option_ids: vec!["a".to_owned(), "c".to_owned()],
        });

        let feedback = grade_learning_answer(
            &item,
            &LearningAnswer::MultipleChoice {
                option_ids: vec!["a".to_owned()],
            },
            None,
        )?;

        assert_eq!(feedback.outcome, LearningAttemptOutcome::Incorrect);
        Ok(())
    }

    #[test]
    fn open_answer_requires_explicit_self_check() {
        let item = revision(LearningAnswerSpec::OpenSelfCheck {
            sample_answer: "Ожидаемые понятия".to_owned(),
        });

        let result = grade_learning_answer(
            &item,
            &LearningAnswer::Text {
                text: "Мой ответ".to_owned(),
            },
            None,
        );

        assert_eq!(result, Err(LearningValidationError::SelfCheckRequired));
    }

    #[test]
    fn completing_session_does_not_create_attempts_for_unanswered_items(
    ) -> Result<(), LearningValidationError> {
        let mut session = LearningSession {
            id: Uuid::now_v7(),
            user_id: Uuid::now_v7(),
            source: LearningSource {
                id: Uuid::now_v7(),
                space_id: Uuid::now_v7(),
                material_id: Uuid::now_v7(),
                document_revision_id: Uuid::now_v7(),
                scope_kind: LearningScopeKind::Material,
                scope_key: "scope".to_owned(),
                content_unit_id: None,
                anchor: None,
                source_hash: "hash".to_owned(),
                title: "Source".to_owned(),
                created_at: 1,
            },
            kind: LearningSessionKind::ImmediateRecall,
            state: LearningSessionState::Offered,
            items: Vec::new(),
            attempts: Vec::new(),
            created_at: 1,
            updated_at: 1,
        };

        session.start()?;
        session.complete()?;

        assert!(session.attempts.is_empty());
        Ok(())
    }
}
