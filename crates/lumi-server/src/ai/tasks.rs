//! Durable AI task queue, internal worker and saved-summary application service.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use lumi_core::{
    AiArtifact, AiExecutionMode, AiExecutorKind, AiMessageRole, AiPage, AiProviderChatRequest,
    AiProviderMessage, AiSourceScope, AiTask, AiTaskStatus, ArtifactMutationRequest,
    BulkExecuteTasksRequest, BulkTaskItemResult, BulkTaskResult, CompleteAiTaskRequest,
    CreateAiTaskCommand, CreateSummaryTaskRequest, SummaryArtifact, SummaryForm, SummaryScopeKind,
    TaskMutationRequest, UpdateSummaryRequest, SUMMARY_ARTIFACT_SCHEMA_VERSION,
    SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION, SUMMARY_CHAPTER_OUTLINE_PROMPT_VERSION,
    SUMMARY_MATERIAL_BRIEF_PROMPT_VERSION, SUMMARY_MATERIAL_OUTLINE_PROMPT_VERSION,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::context::SourceContextError;
use super::providers::{self, AiProviderClient, AiProviderError};
use super::repository::{AiRepositoryError, PgAiRepository};
use super::AiRuntime;
use crate::account::AuthenticatedSession;
use crate::{AppError, AppState};

const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(250);
const WORKER_LEASE: Duration = Duration::from_secs(15 * 60);
const MAX_BULK_TASKS: usize = 50;
const HIERARCHICAL_FRAGMENT_THRESHOLD: usize = 8;
const HIERARCHICAL_TEXT_THRESHOLD: usize = 64 * 1024;
const SECTION_TEXT_TARGET: usize = 32 * 1024;

#[derive(Deserialize)]
struct TaskListQuery {
    status: Option<String>,
    material_id: Option<Uuid>,
}

/// Return the E2 queue and summary route contribution.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/ai/tasks", get(list_tasks).post(create_task))
        .route("/ai/tasks/bulk-execute", post(bulk_execute))
        .route("/ai/tasks/{task_id}", get(get_task))
        .route("/ai/tasks/{task_id}/execute", post(execute_task))
        .route("/ai/tasks/{task_id}/cancel", post(cancel_task))
        .route("/ai/tasks/{task_id}/retry", post(retry_task))
        .route("/ai/artifacts/{artifact_id}", get(get_artifact))
        .route("/ai/artifacts/{artifact_id}/accept", post(accept_artifact))
        .route("/ai/artifacts/{artifact_id}/reject", post(reject_artifact))
        .route("/materials/{material_id}/summaries", get(list_summaries))
        .route(
            "/materials/{material_id}/summary-tasks",
            post(create_summary_task),
        )
        .route(
            "/ai/summaries/{summary_id}",
            axum::routing::patch(update_summary).delete(delete_summary),
        )
}

async fn list_tasks(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<AiPage<AiTask>>, AppError> {
    let repository = repository(&state)?;
    let mut items = repository
        .list_tasks(session.user_id)
        .await
        .map_err(map_repository_error)?;
    if let Some(status) = query.status.as_deref() {
        items.retain(|task| task_status_name(task.status) == status);
    }
    if let Some(material_id) = query.material_id {
        items.retain(|task| task.source_scope.material_id() == material_id);
    }
    Ok(Json(AiPage {
        items,
        next_cursor: None,
    }))
}

async fn create_task(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(command): Json<CreateAiTaskCommand>,
) -> Result<Json<AiTask>, AppError> {
    repository(&state)?
        .create_task(session.user_id, command)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn get_task(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(task_id): Path<Uuid>,
) -> Result<Json<AiTask>, AppError> {
    repository(&state)?
        .task(session.user_id, task_id)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn execute_task(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(task_id): Path<Uuid>,
    Json(request): Json<TaskMutationRequest>,
) -> Result<Json<AiTask>, AppError> {
    repository(&state)?
        .request_execute(session.user_id, task_id, &request)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn cancel_task(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(task_id): Path<Uuid>,
    Json(request): Json<TaskMutationRequest>,
) -> Result<Json<AiTask>, AppError> {
    repository(&state)?
        .request_cancel(session.user_id, task_id, &request)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn retry_task(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(task_id): Path<Uuid>,
    Json(request): Json<TaskMutationRequest>,
) -> Result<Json<AiTask>, AppError> {
    repository(&state)?
        .retry_task(session.user_id, task_id, &request)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn bulk_execute(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<BulkExecuteTasksRequest>,
) -> Result<Json<BulkTaskResult>, AppError> {
    if request.task_ids.is_empty()
        || request.task_ids.len() > MAX_BULK_TASKS
        || request.idempotency_key.trim().is_empty()
    {
        return Err(AppError::BadRequest(
            "bulk execute accepts from 1 to 50 tasks".to_owned(),
        ));
    }
    let repository = repository(&state)?;
    let mut results = Vec::with_capacity(request.task_ids.len());
    for (index, task_id) in request.task_ids.iter().copied().enumerate() {
        let item = match repository.task(session.user_id, task_id).await {
            Ok(task) => {
                let mutation = TaskMutationRequest {
                    expected_revision: task.object_revision,
                    idempotency_key: format!("{}:{index}", request.idempotency_key),
                };
                match repository
                    .request_execute(session.user_id, task_id, &mutation)
                    .await
                {
                    Ok(updated) => BulkTaskItemResult {
                        task_id,
                        outcome: "scheduled".to_owned(),
                        object_revision: Some(updated.object_revision),
                    },
                    Err(AiRepositoryError::Conflict) => BulkTaskItemResult {
                        task_id,
                        outcome: "not_queued".to_owned(),
                        object_revision: Some(task.object_revision),
                    },
                    Err(_) => BulkTaskItemResult {
                        task_id,
                        outcome: "unavailable".to_owned(),
                        object_revision: None,
                    },
                }
            }
            Err(AiRepositoryError::NotFound) => BulkTaskItemResult {
                task_id,
                outcome: "not_found".to_owned(),
                object_revision: None,
            },
            Err(_) => BulkTaskItemResult {
                task_id,
                outcome: "unavailable".to_owned(),
                object_revision: None,
            },
        };
        results.push(item);
    }
    Ok(Json(BulkTaskResult { results }))
}

async fn get_artifact(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(artifact_id): Path<Uuid>,
) -> Result<Json<AiArtifact>, AppError> {
    repository(&state)?
        .artifact(session.user_id, artifact_id)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn accept_artifact(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(artifact_id): Path<Uuid>,
    Json(request): Json<ArtifactMutationRequest>,
) -> Result<Json<AiArtifact>, AppError> {
    repository(&state)?
        .mutate_artifact(
            session.user_id,
            artifact_id,
            request.expected_revision,
            &request.idempotency_key,
            true,
        )
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn reject_artifact(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(artifact_id): Path<Uuid>,
    Json(request): Json<ArtifactMutationRequest>,
) -> Result<Json<AiArtifact>, AppError> {
    repository(&state)?
        .mutate_artifact(
            session.user_id,
            artifact_id,
            request.expected_revision,
            &request.idempotency_key,
            false,
        )
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn list_summaries(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<Uuid>,
) -> Result<Json<AiPage<SummaryArtifact>>, AppError> {
    let items = repository(&state)?
        .list_summaries(session.user_id, material_id)
        .await
        .map_err(map_repository_error)?;
    Ok(Json(AiPage {
        items,
        next_cursor: None,
    }))
}

async fn create_summary_task(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<Uuid>,
    Json(request): Json<CreateSummaryTaskRequest>,
) -> Result<Json<AiTask>, AppError> {
    if request.scope_ref.trim().is_empty() || request.idempotency_key.trim().is_empty() {
        return Err(AppError::BadRequest(
            "summary scope and idempotency key are required".to_owned(),
        ));
    }
    let (source_scope, prompt_version, instruction) = match request.scope_kind {
        SummaryScopeKind::Chapter => (
            AiSourceScope::Chapter {
                material_id,
                revision_id: request.source_revision_id,
                scope_ref: request.scope_ref.clone(),
            },
            match request.form {
                SummaryForm::Brief => SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION,
                SummaryForm::Outline => SUMMARY_CHAPTER_OUTLINE_PROMPT_VERSION,
            },
            "Создай сохранённое саммари этой главы. Верни только проверяемый JSON.",
        ),
        SummaryScopeKind::Material => (
            AiSourceScope::Material {
                material_id,
                revision_id: request.source_revision_id,
            },
            match request.form {
                SummaryForm::Brief => SUMMARY_MATERIAL_BRIEF_PROMPT_VERSION,
                SummaryForm::Outline => SUMMARY_MATERIAL_OUTLINE_PROMPT_VERSION,
            },
            "Создай сохранённое саммари всего материала. Верни только проверяемый JSON.",
        ),
    };
    let repository = repository(&state)?;
    let mut task = repository
        .create_task(
            session.user_id,
            CreateAiTaskCommand {
                kind: "summary".to_owned(),
                source_scope,
                instruction: instruction.to_owned(),
                parameters: serde_json::json!({"form": request.form}),
                prompt_version: prompt_version.to_owned(),
                output_schema_version: SUMMARY_ARTIFACT_SCHEMA_VERSION.to_owned(),
                idempotency_key: request.idempotency_key.clone(),
            },
        )
        .await
        .map_err(map_repository_error)?;
    if request.execution_mode == AiExecutionMode::ExecuteNow && task.status == AiTaskStatus::Queued
    {
        task = repository
            .request_execute(
                session.user_id,
                task.id,
                &TaskMutationRequest {
                    expected_revision: task.object_revision,
                    idempotency_key: format!("{}:execute", request.idempotency_key),
                },
            )
            .await
            .map_err(map_repository_error)?;
    }
    Ok(Json(task))
}

async fn update_summary(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(summary_id): Path<Uuid>,
    Json(request): Json<UpdateSummaryRequest>,
) -> Result<Json<SummaryArtifact>, AppError> {
    repository(&state)?
        .update_summary(session.user_id, summary_id, &request)
        .await
        .map(Json)
        .map_err(map_repository_error)
}

async fn delete_summary(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(summary_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    repository(&state)?
        .delete_summary(session.user_id, summary_id)
        .await
        .map_err(map_repository_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn repository(state: &AppState) -> Result<PgAiRepository, AppError> {
    Ok(PgAiRepository::new(state.ai_runtime()?.pool().clone()))
}

/// Run the internal provider worker until server shutdown.
pub(crate) async fn run_worker(runtime: Arc<AiRuntime>, cancellation: CancellationToken) {
    let repository = PgAiRepository::new(runtime.pool().clone());
    if let Err(error) = repository.recover_expired().await {
        tracing::warn!(error = %error, "AI task recovery failed");
    }
    loop {
        let next = repository.next_internal_task().await;
        match next {
            Ok(Some((owner_id, task_id))) => {
                if let Err(error) =
                    execute_one(Arc::clone(&runtime), &repository, owner_id, task_id).await
                {
                    tracing::warn!(%task_id, error = %error, "AI task execution failed");
                }
            }
            Ok(None) => {
                tokio::select! {
                    () = cancellation.cancelled() => break,
                    () = tokio::time::sleep(WORKER_POLL_INTERVAL) => {}
                }
            }
            Err(error) => {
                tracing::warn!(error = %error, "AI task queue polling failed");
                tokio::select! {
                    () = cancellation.cancelled() => break,
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                }
            }
        }
        if cancellation.is_cancelled() {
            break;
        }
    }
}

async fn execute_one(
    runtime: Arc<AiRuntime>,
    repository: &PgAiRepository,
    owner_id: Uuid,
    task_id: Uuid,
) -> Result<(), AiRepositoryError> {
    let input = repository.execution_input(owner_id, task_id).await?;
    let pack = match runtime
        .context()
        .resolve_for_task(owner_id, task_id, input.task.source_scope.clone())
        .await
    {
        Ok(pack) => pack,
        Err(error) => {
            let code = context_error_code(&error);
            repository
                .fail_unclaimed_task(owner_id, task_id, code)
                .await?;
            return Ok(());
        }
    };
    repository.store_context_pack(owner_id, &pack).await?;
    let claim = repository
        .claim_task(
            owner_id,
            task_id,
            AiExecutorKind::InternalProvider,
            Some("lumi:internal-summary-worker"),
        )
        .await?;
    if repository
        .heartbeat(owner_id, &claim, 0.12, Some("context"), WORKER_LEASE)
        .await?
    {
        repository
            .fail_task(owner_id, &claim, "cancelled", false)
            .await?;
        return Ok(());
    }
    let (provider, preferences) =
        match providers::load_provider_for_runtime(&runtime, owner_id).await {
            Ok(value) => value,
            Err(error) => {
                repository
                    .fail_task(
                        owner_id,
                        &claim,
                        provider_error_code(&error),
                        error.retryable(),
                    )
                    .await?;
                return Ok(());
            }
        };
    repository
        .heartbeat(
            owner_id,
            &claim,
            0.36,
            Some("section_summaries"),
            WORKER_LEASE,
        )
        .await?;
    let section_summaries =
        if requires_hierarchical_summary(&pack) {
            let sections = summary_sections(&pack);
            let mut summaries = Vec::with_capacity(sections.len());
            for (index, section) in sections.iter().enumerate() {
                let request = AiProviderChatRequest {
                    generation_id: Uuid::now_v7(),
                    model: preferences.default_model.clone(),
                    messages: vec![
                        AiProviderMessage {
                            role: AiMessageRole::System,
                            content: summary_system_prompt(&input.task),
                        },
                        AiProviderMessage {
                            role: AiMessageRole::User,
                            content: format!(
                                "Сделай промежуточное source-backed саммари части {}/{}. \
                             Сохрани citation_ids исходных фрагментов.\n\n{section}",
                                index + 1,
                                sections.len()
                            ),
                        },
                    ],
                    context_pack: None,
                    max_output_tokens: 2048,
                    provider_options: serde_json::json!({"temperature": 0.1}),
                };
                let result = match provider
                    .complete_structured(request, &input.task.output_schema_version)
                    .await
                {
                    Ok(result) => result,
                    Err(error) => {
                        repository
                            .fail_task(
                                owner_id,
                                &claim,
                                provider_error_code(&error),
                                error.retryable(),
                            )
                            .await?;
                        return Ok(());
                    }
                };
                summaries.push(result);
                let fraction = 0.36 + (0.28 * ((index + 1) as f32 / sections.len() as f32));
                if repository
                    .heartbeat(
                        owner_id,
                        &claim,
                        fraction,
                        Some("section_summaries"),
                        WORKER_LEASE,
                    )
                    .await?
                {
                    repository
                        .fail_task(owner_id, &claim, "cancelled", false)
                        .await?;
                    return Ok(());
                }
            }
            Some(serde_json::to_string(&summaries).map_err(|_| {
                AiRepositoryError::Invalid("summary serialization failed".to_owned())
            })?)
        } else {
            None
        };
    let synthesis_instruction = match section_summaries {
        Some(summaries) => format!(
            "{}\n\nНиже промежуточные саммари частей. Синтезируй единый результат, \
             проверяя факты и citation_ids по исходным source fragments:\n{summaries}",
            input.instruction
        ),
        None => input.instruction,
    };
    let request = AiProviderChatRequest {
        generation_id: claim.run_id,
        model: preferences.default_model,
        messages: vec![
            AiProviderMessage {
                role: AiMessageRole::System,
                content: summary_system_prompt(&input.task),
            },
            AiProviderMessage {
                role: AiMessageRole::User,
                content: synthesis_instruction,
            },
        ],
        context_pack: Some(pack),
        max_output_tokens: 4096,
        provider_options: serde_json::json!({"temperature": 0.2}),
    };
    let result = match provider
        .complete_structured(request, &input.task.output_schema_version)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            repository
                .fail_task(
                    owner_id,
                    &claim,
                    provider_error_code(&error),
                    error.retryable(),
                )
                .await?;
            return Ok(());
        }
    };
    if repository
        .heartbeat(owner_id, &claim, 0.72, Some("synthesis"), WORKER_LEASE)
        .await?
    {
        repository
            .fail_task(owner_id, &claim, "cancelled", false)
            .await?;
        return Ok(());
    }
    repository
        .heartbeat(owner_id, &claim, 0.86, Some("coverage_check"), WORKER_LEASE)
        .await?;
    repository
        .heartbeat(
            owner_id,
            &claim,
            0.94,
            Some("artifact_validation"),
            WORKER_LEASE,
        )
        .await?;
    let completion = repository
        .complete_task(
            owner_id,
            CompleteAiTaskRequest {
                task_id,
                run_id: claim.run_id,
                claim_id: claim.claim_id,
                fence: claim.fence,
                task_revision: claim.task_revision,
                idempotency_key: format!("internal-complete:{}", claim.run_id),
                result,
            },
        )
        .await?;
    repository
        .activate_generated_summary(owner_id, completion.artifact_id)
        .await?;
    Ok(())
}

fn requires_hierarchical_summary(pack: &lumi_core::AiContextPack) -> bool {
    pack.fragments.len() > HIERARCHICAL_FRAGMENT_THRESHOLD
        || pack
            .fragments
            .iter()
            .map(|fragment| fragment.text.len())
            .sum::<usize>()
            > HIERARCHICAL_TEXT_THRESHOLD
}

fn summary_sections(pack: &lumi_core::AiContextPack) -> Vec<String> {
    let mut sections = Vec::new();
    let mut current = String::new();
    for fragment in &pack.fragments {
        let source = format!(
            "<source citation_id=\"{}\">\n{}\n</source>",
            fragment.citation_id, fragment.text
        );
        if !current.is_empty() && current.len().saturating_add(source.len()) > SECTION_TEXT_TARGET {
            sections.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(&source);
    }
    if !current.is_empty() {
        sections.push(current);
    }
    sections
}

fn summary_system_prompt(task: &AiTask) -> String {
    format!(
        "Ты создаёшь source-backed summary. Используй только переданные источники. \
         Верни JSON schema_version={SUMMARY_ARTIFACT_SCHEMA_VERSION}, content и \
         непустой citation_ids из выданных citation_id. Prompt version: {}.",
        task.prompt_version
    )
}

fn provider_error_code(error: &AiProviderError) -> &'static str {
    match error {
        AiProviderError::MissingCredential => "missing_credential",
        AiProviderError::Authentication => "provider_authentication",
        AiProviderError::ModelUnavailable => "model_unavailable",
        AiProviderError::RateLimited => "rate_limited",
        AiProviderError::QuotaExhausted => "quota_exhausted",
        AiProviderError::LimitExceeded => "limit_exceeded",
        AiProviderError::PolicyRejected => "policy_rejected",
        AiProviderError::Timeout => "provider_timeout",
        AiProviderError::Cancelled => "cancelled",
        AiProviderError::InvalidResponse => "invalid_structured_result",
        AiProviderError::Unavailable => "provider_unavailable",
    }
}

fn context_error_code(error: &SourceContextError) -> &'static str {
    match error {
        SourceContextError::NotFound => "source_revision_unavailable",
        SourceContextError::StaleSelection => "stale_source_selection",
        SourceContextError::ScopeNotFound => "source_scope_unavailable",
        SourceContextError::MissingText => "source_text_unavailable",
        SourceContextError::LimitExceeded => "source_context_limit",
        SourceContextError::Storage => "source_context_storage",
    }
}

fn map_repository_error(error: AiRepositoryError) -> AppError {
    match error {
        AiRepositoryError::Invalid(detail) => AppError::BadRequest(detail),
        AiRepositoryError::NotFound => AppError::NotFound("AI task or artifact"),
        AiRepositoryError::Conflict | AiRepositoryError::StaleClaim => {
            AppError::Conflict("AI task state changed; reload and retry".to_owned())
        }
        AiRepositoryError::Storage => AppError::Unavailable("AI task repository"),
    }
}

fn task_status_name(status: AiTaskStatus) -> &'static str {
    match status {
        AiTaskStatus::Queued => "queued",
        AiTaskStatus::Running => "running",
        AiTaskStatus::NeedsInput => "needs_input",
        AiTaskStatus::Succeeded => "succeeded",
        AiTaskStatus::Failed => "failed",
        AiTaskStatus::Cancelled => "cancelled",
    }
}

#[cfg(test)]
mod tests {
    use lumi_core::AiContextPack;

    use super::{requires_hierarchical_summary, summary_sections, HIERARCHICAL_FRAGMENT_THRESHOLD};

    #[test]
    fn hierarchical_summary_chunks_large_context_without_losing_citations(
    ) -> Result<(), serde_json::Error> {
        let mut pack: AiContextPack = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/ai/contracts/v1/context-pack.json"
        ))?;
        let original = pack.fragments[0].clone();
        pack.fragments = (0..=HIERARCHICAL_FRAGMENT_THRESHOLD)
            .map(|index| {
                let mut fragment = original.clone();
                fragment.citation_id = format!("ctx:hierarchy:{index}");
                fragment.text = "текст ".repeat(6_000);
                fragment
            })
            .collect();

        assert!(requires_hierarchical_summary(&pack));
        let sections = summary_sections(&pack);
        assert_eq!(sections.len(), pack.fragments.len());
        for fragment in &pack.fragments {
            assert!(
                sections
                    .iter()
                    .any(|section| section.contains(&fragment.citation_id)),
                "citation must remain available to the section provider call"
            );
        }
        Ok(())
    }
}
