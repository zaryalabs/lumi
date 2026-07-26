//! Shared source-backed learning generation application service.

use crate::ai::repository::{AiRepositoryError, PgAiRepository};
use crate::AppState;
use lumi_core::{
    AiExecutionMode, AiSourceScope, AiTask, AiTaskStatus, CreateAiTaskCommand,
    GenerateLearningItemsRequest, LearningScopeKind, LearningSource, LearningSourceId,
    TaskMutationRequest, UserId, LEARNING_ITEMS_PROMPT_VERSION,
    QUESTION_SET_ARTIFACT_SCHEMA_VERSION,
};
use thiserror::Error;

use super::limits::LearningOperationKind;
use super::repository::LearningStoreError;

/// Restriction applied to generated learning drafts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LearningGenerationTarget {
    /// Allow the validated learning item families supported by the common artifact.
    MixedItems,
    /// Request only front/back flashcard drafts.
    Flashcards,
}

/// Stable application-service failures shared by HTTP and MCP adapters.
#[derive(Debug, Error)]
pub(crate) enum LearningGenerationError {
    /// Request bounds or source shape are invalid.
    #[error("invalid learning generation request: {0}")]
    Invalid(String),
    /// Source is absent from the authenticated account.
    #[error("learning source was not found")]
    NotFound,
    /// Current task state conflicts with this command.
    #[error("learning generation task conflicts with current state")]
    Conflict,
    /// AI or persistence prerequisites are unavailable.
    #[error("learning generation is unavailable")]
    Unavailable,
    /// The account or process exhausted its bounded operation budget.
    #[error("learning generation rate limit exceeded")]
    Limited,
}

/// Create one source-backed learning task through the common AI queue.
pub(crate) async fn create_learning_generation_task(
    state: &AppState,
    owner_id: UserId,
    source_id: LearningSourceId,
    request: &GenerateLearningItemsRequest,
    target: LearningGenerationTarget,
) -> Result<AiTask, LearningGenerationError> {
    if !(1..=32).contains(&request.item_count) || request.idempotency_key.trim().is_empty() {
        return Err(LearningGenerationError::Invalid(
            "item_count must be between 1 and 32 and idempotency_key is required".to_owned(),
        ));
    }
    let _permit = state
        .learning_operation_limits()
        .acquire(owner_id, LearningOperationKind::Ai)
        .map_err(|error| match error {
            super::limits::LearningOperationLimitError::RateLimited
            | super::limits::LearningOperationLimitError::Busy => LearningGenerationError::Limited,
            super::limits::LearningOperationLimitError::Unavailable => {
                LearningGenerationError::Unavailable
            }
        })?;
    let source = state
        .learning_runtime()
        .source(owner_id, source_id)
        .await
        .map_err(map_learning_error)?;
    let runtime = state
        .ai_runtime()
        .map_err(|_| LearningGenerationError::Unavailable)?;
    let repository = PgAiRepository::new(runtime.pool().clone());
    let (instruction, allowed_kinds) = match target {
        LearningGenerationTarget::MixedItems => (
            format!(
                "Создай {} разнообразных учебных заданий. Все задания останутся \
                 черновиками до явного решения пользователя.",
                request.item_count
            ),
            None,
        ),
        LearningGenerationTarget::Flashcards => (
            format!(
                "Создай {} карточек flashcard с краткими проверяемыми лицевой и \
                 обратной сторонами. Не создавай задания других видов. Все карточки \
                 останутся черновиками до явного решения пользователя.",
                request.item_count
            ),
            Some(vec!["flashcard"]),
        ),
    };
    let mut task = repository
        .create_task(
            owner_id,
            CreateAiTaskCommand {
                kind: "generate_learning_items".to_owned(),
                source_scope: ai_scope(&source)?,
                instruction,
                parameters: serde_json::json!({
                    "source_id": source_id,
                    "item_count": request.item_count,
                    "allowed_kinds": allowed_kinds,
                }),
                prompt_version: LEARNING_ITEMS_PROMPT_VERSION.to_owned(),
                output_schema_version: QUESTION_SET_ARTIFACT_SCHEMA_VERSION.to_owned(),
                idempotency_key: request.idempotency_key.clone(),
            },
        )
        .await
        .map_err(map_ai_error)?;
    if request.execution_mode == AiExecutionMode::ExecuteNow && task.status == AiTaskStatus::Queued
    {
        task = repository
            .request_execute(
                owner_id,
                task.id,
                &TaskMutationRequest {
                    expected_revision: task.object_revision,
                    idempotency_key: format!("{}:execute", request.idempotency_key),
                },
            )
            .await
            .map_err(map_ai_error)?;
    }
    Ok(task)
}

fn ai_scope(source: &LearningSource) -> Result<AiSourceScope, LearningGenerationError> {
    Ok(match source.scope_kind {
        LearningScopeKind::Material => AiSourceScope::Material {
            material_id: source.material_id,
            revision_id: source.document_revision_id,
        },
        LearningScopeKind::ContentUnit => AiSourceScope::Chapter {
            material_id: source.material_id,
            revision_id: source.document_revision_id,
            scope_ref: source.content_unit_id.clone().unwrap_or_default(),
        },
        LearningScopeKind::Anchor => AiSourceScope::Selection {
            material_id: source.material_id,
            revision_id: source.document_revision_id,
            anchor: Box::new(source.anchor.clone().ok_or_else(|| {
                LearningGenerationError::Invalid(
                    "anchor-scoped learning source has no anchor".to_owned(),
                )
            })?),
        },
    })
}

fn map_learning_error(error: LearningStoreError) -> LearningGenerationError {
    match error {
        LearningStoreError::NotFound => LearningGenerationError::NotFound,
        LearningStoreError::Conflict => LearningGenerationError::Conflict,
        LearningStoreError::Invalid(error) => LearningGenerationError::Invalid(error.to_string()),
        LearningStoreError::InvalidDetail(detail) => LearningGenerationError::Invalid(detail),
        LearningStoreError::Unavailable => LearningGenerationError::Unavailable,
    }
}

fn map_ai_error(error: AiRepositoryError) -> LearningGenerationError {
    match error {
        AiRepositoryError::NotFound => LearningGenerationError::NotFound,
        AiRepositoryError::Conflict | AiRepositoryError::StaleClaim => {
            LearningGenerationError::Conflict
        }
        AiRepositoryError::Invalid(detail) => LearningGenerationError::Invalid(detail),
        AiRepositoryError::Storage => LearningGenerationError::Unavailable,
    }
}
