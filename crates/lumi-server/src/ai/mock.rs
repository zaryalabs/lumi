//! Deterministic provider and task-service mocks shared by all AI tracks.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use lumi_core::{
    content_hash, validate_ai_output, AiArtifactId, AiClaimId, AiContextPackId,
    AiProviderChatRequest, AiProviderEvent, AiRunId, AiTask, AiTaskClaim, AiTaskId, AiTaskStatus,
    CompleteAiTaskRequest, CompleteAiTaskResult, CreateAiTaskCommand, UserId, AI_CONTRACT_VERSION,
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::mpsc;

use super::providers::{AiProviderClient, AiProviderError, AiProviderEventStream};

/// Script entry emitted by [`MockAiProvider`].
pub type MockProviderEvent = Result<AiProviderEvent, AiProviderError>;

/// Deterministic provider with FIFO stream and structured-output scripts.
#[derive(Default)]
pub struct MockAiProvider {
    streams: Mutex<VecDeque<Vec<MockProviderEvent>>>,
    structured: Mutex<VecDeque<Result<Value, AiProviderError>>>,
    requests: Mutex<Vec<AiProviderChatRequest>>,
}

impl MockAiProvider {
    /// Build an empty mock provider.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue one normalized streaming script.
    ///
    /// # Errors
    ///
    /// Returns an error if the internal mock lock was poisoned.
    pub fn push_stream(&self, events: Vec<MockProviderEvent>) -> Result<(), AiProviderError> {
        lock_provider(&self.streams)?.push_back(events);
        Ok(())
    }

    /// Queue one structured provider response.
    ///
    /// # Errors
    ///
    /// Returns an error if the internal mock lock was poisoned.
    pub fn push_structured(
        &self,
        result: Result<Value, AiProviderError>,
    ) -> Result<(), AiProviderError> {
        lock_provider(&self.structured)?.push_back(result);
        Ok(())
    }

    /// Return all requests observed by this mock.
    ///
    /// # Errors
    ///
    /// Returns an error if the internal mock lock was poisoned.
    pub fn requests(&self) -> Result<Vec<AiProviderChatRequest>, AiProviderError> {
        Ok(lock_provider(&self.requests)?.clone())
    }
}

#[async_trait]
impl AiProviderClient for MockAiProvider {
    async fn stream_chat(
        &self,
        request: AiProviderChatRequest,
    ) -> Result<AiProviderEventStream, AiProviderError> {
        lock_provider(&self.requests)?.push(request);
        let events = lock_provider(&self.streams)?
            .pop_front()
            .ok_or(AiProviderError::Unavailable)?;
        let capacity = events.len().max(1);
        let (sender, receiver) = mpsc::channel(capacity);
        for event in events {
            sender
                .try_send(event)
                .map_err(|_| AiProviderError::Unavailable)?;
        }
        drop(sender);
        Ok(AiProviderEventStream::new(receiver))
    }

    async fn complete_structured(
        &self,
        request: AiProviderChatRequest,
        output_schema_version: &str,
    ) -> Result<Value, AiProviderError> {
        lock_provider(&self.requests)?.push(request);
        let result = lock_provider(&self.structured)?
            .pop_front()
            .ok_or(AiProviderError::Unavailable)??;
        validate_ai_output(output_schema_version, &result)
            .map_err(|_| AiProviderError::InvalidResponse)?;
        Ok(result)
    }
}

fn lock_provider<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, AiProviderError> {
    mutex.lock().map_err(|_| AiProviderError::Unavailable)
}

/// Frozen mock task-service error.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum MockAiTaskServiceError {
    /// Command failed core contract validation.
    #[error("invalid task command: {0}")]
    InvalidCommand(String),
    /// Task is absent or belongs to another account.
    #[error("AI task was not found")]
    NotFound,
    /// Task cannot be claimed or mutated in its current state.
    #[error("AI task state conflict")]
    Conflict,
    /// Completion does not match the active claim/fence/revision.
    #[error("stale AI task claim")]
    StaleClaim,
    /// Internal test-double state was poisoned.
    #[error("mock AI task service is unavailable")]
    Unavailable,
}

/// Minimal application-service interface used by Track B and Track C tests.
#[async_trait]
pub trait AiTaskApplicationService: Send + Sync {
    /// Create or replay one durable task.
    async fn create_task(
        &self,
        owner_id: UserId,
        command: CreateAiTaskCommand,
    ) -> Result<AiTask, MockAiTaskServiceError>;

    /// List account-owned tasks in deterministic insertion order.
    async fn list_tasks(&self, owner_id: UserId) -> Result<Vec<AiTask>, MockAiTaskServiceError>;

    /// Atomically claim one queued task.
    async fn claim_task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
    ) -> Result<AiTaskClaim, MockAiTaskServiceError>;

    /// Validate the current claim and publish one typed result idempotently.
    async fn complete_task(
        &self,
        owner_id: UserId,
        request: CompleteAiTaskRequest,
    ) -> Result<CompleteAiTaskResult, MockAiTaskServiceError>;
}

#[derive(Clone)]
struct ActiveMockClaim {
    claim: AiTaskClaim,
    completion: Option<(String, CompleteAiTaskResult)>,
}

#[derive(Default)]
struct MockTaskState {
    tasks: Vec<AiTask>,
    active_claims: HashMap<AiTaskId, ActiveMockClaim>,
    next_fence: u64,
}

/// In-memory deterministic task service implementing frozen dedupe and fencing.
#[derive(Default)]
pub struct MockAiTaskService {
    state: Mutex<MockTaskState>,
}

impl MockAiTaskService {
    /// Build an empty mock task service.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<MutexGuard<'_, MockTaskState>, MockAiTaskServiceError> {
        self.state
            .lock()
            .map_err(|_| MockAiTaskServiceError::Unavailable)
    }
}

#[async_trait]
impl AiTaskApplicationService for MockAiTaskService {
    async fn create_task(
        &self,
        owner_id: UserId,
        command: CreateAiTaskCommand,
    ) -> Result<AiTask, MockAiTaskServiceError> {
        command
            .validate()
            .map_err(|error| MockAiTaskServiceError::InvalidCommand(error.to_string()))?;
        let canonical = serde_json::to_vec(&serde_json::json!({
            "kind": &command.kind,
            "source_scope": &command.source_scope,
            "instruction": &command.instruction,
            "parameters": &command.parameters,
            "prompt_version": &command.prompt_version,
            "output_schema_version": &command.output_schema_version,
        }))
        .map_err(|error| MockAiTaskServiceError::InvalidCommand(error.to_string()))?;
        let dedupe_key = content_hash(&canonical);
        let mut state = self.lock()?;
        if let Some(task) = state.tasks.iter().find(|task| {
            task.owner_id == owner_id
                && task.dedupe_key == dedupe_key
                && matches!(
                    task.status,
                    AiTaskStatus::Queued | AiTaskStatus::Running | AiTaskStatus::NeedsInput
                )
        }) {
            return Ok(task.clone());
        }
        let task = AiTask {
            contract_version: AI_CONTRACT_VERSION.to_owned(),
            id: AiTaskId::now_v7(),
            owner_id,
            kind: command.kind,
            result_kind: output_kind(&command.output_schema_version).to_owned(),
            source_scope: command.source_scope,
            parameters: command.parameters,
            prompt_version: command.prompt_version,
            output_schema_version: command.output_schema_version,
            status: AiTaskStatus::Queued,
            dedupe_key,
            object_revision: 1,
        };
        state.tasks.push(task.clone());
        Ok(task)
    }

    async fn list_tasks(&self, owner_id: UserId) -> Result<Vec<AiTask>, MockAiTaskServiceError> {
        Ok(self
            .lock()?
            .tasks
            .iter()
            .filter(|task| task.owner_id == owner_id)
            .cloned()
            .collect())
    }

    async fn claim_task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
    ) -> Result<AiTaskClaim, MockAiTaskServiceError> {
        let mut state = self.lock()?;
        let task_index = state
            .tasks
            .iter()
            .position(|task| task.id == task_id && task.owner_id == owner_id)
            .ok_or(MockAiTaskServiceError::NotFound)?;
        if state.tasks[task_index].status != AiTaskStatus::Queued {
            return Err(MockAiTaskServiceError::Conflict);
        }
        state.next_fence = state.next_fence.saturating_add(1).max(1);
        let claim = AiTaskClaim {
            task_id,
            run_id: AiRunId::now_v7(),
            claim_id: AiClaimId::now_v7(),
            fence: state.next_fence,
            lease_expires_at: "2099-01-01T00:00:00Z".to_owned(),
            task_revision: state.tasks[task_index].object_revision,
            result_kind: state.tasks[task_index].result_kind.clone(),
            result_schema_version: state.tasks[task_index].output_schema_version.clone(),
            context_pack_id: AiContextPackId::now_v7(),
        };
        state.tasks[task_index].status = AiTaskStatus::Running;
        state.tasks[task_index].object_revision =
            state.tasks[task_index].object_revision.saturating_add(1);
        state.active_claims.insert(
            task_id,
            ActiveMockClaim {
                claim: claim.clone(),
                completion: None,
            },
        );
        Ok(claim)
    }

    async fn complete_task(
        &self,
        owner_id: UserId,
        request: CompleteAiTaskRequest,
    ) -> Result<CompleteAiTaskResult, MockAiTaskServiceError> {
        let mut state = self.lock()?;
        let task_index = state
            .tasks
            .iter()
            .position(|task| task.id == request.task_id && task.owner_id == owner_id)
            .ok_or(MockAiTaskServiceError::NotFound)?;
        let expected_schema = state.tasks[task_index].output_schema_version.clone();
        request
            .validate(&expected_schema)
            .map_err(|error| MockAiTaskServiceError::InvalidCommand(error.to_string()))?;
        let active = state
            .active_claims
            .get(&request.task_id)
            .ok_or(MockAiTaskServiceError::StaleClaim)?;
        if active.claim.run_id != request.run_id
            || active.claim.claim_id != request.claim_id
            || active.claim.fence != request.fence
            || active.claim.task_revision != request.task_revision
        {
            return Err(MockAiTaskServiceError::StaleClaim);
        }
        let payload_hash = content_hash(
            &serde_json::to_vec(&request.result)
                .map_err(|error| MockAiTaskServiceError::InvalidCommand(error.to_string()))?,
        );
        if let Some((prior_hash, prior_result)) = &active.completion {
            return if prior_hash == &payload_hash {
                let mut replay = prior_result.clone();
                replay.replayed = true;
                Ok(replay)
            } else {
                Err(MockAiTaskServiceError::Conflict)
            };
        }
        let result = CompleteAiTaskResult {
            task_id: request.task_id,
            run_id: request.run_id,
            artifact_id: AiArtifactId::now_v7(),
            result_schema_version: expected_schema,
            replayed: false,
        };
        state.tasks[task_index].status = AiTaskStatus::Succeeded;
        state.tasks[task_index].object_revision =
            state.tasks[task_index].object_revision.saturating_add(1);
        if let Some(active) = state.active_claims.get_mut(&request.task_id) {
            active.completion = Some((payload_hash, result.clone()));
        }
        Ok(result)
    }
}

fn output_kind(schema_version: &str) -> &'static str {
    if schema_version == lumi_core::SUMMARY_ARTIFACT_SCHEMA_VERSION {
        "summary_artifact"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumi_core::{
        AiFinishReason, AiProviderMessage, AiSourceScope, SummaryForm,
        SUMMARY_ARTIFACT_SCHEMA_VERSION, SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION,
    };
    use serde_json::json;

    fn task_command() -> CreateAiTaskCommand {
        CreateAiTaskCommand {
            kind: "summary".to_owned(),
            source_scope: AiSourceScope::Chapter {
                material_id: lumi_core::MaterialId::now_v7(),
                revision_id: lumi_core::DocumentRevisionId::now_v7(),
                scope_ref: "chapter-1".to_owned(),
            },
            instruction: "Сделай краткое резюме.".to_owned(),
            parameters: json!({"form": SummaryForm::Brief}),
            prompt_version: SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION.to_owned(),
            output_schema_version: SUMMARY_ARTIFACT_SCHEMA_VERSION.to_owned(),
            idempotency_key: "create-summary-1".to_owned(),
        }
    }

    fn provider_request() -> AiProviderChatRequest {
        AiProviderChatRequest {
            generation_id: lumi_core::AiGenerationId::now_v7(),
            model: "mock/model".to_owned(),
            messages: vec![AiProviderMessage {
                role: lumi_core::AiMessageRole::User,
                content: "Привет".to_owned(),
            }],
            context_pack: None,
            max_output_tokens: 64,
            provider_options: json!({}),
        }
    }

    #[tokio::test]
    async fn mock_provider_preserves_event_order() -> Result<(), Box<dyn std::error::Error>> {
        let provider = MockAiProvider::new();
        provider.push_stream(vec![
            Ok(AiProviderEvent::TextDelta {
                sequence: 1,
                text: "При".to_owned(),
            }),
            Ok(AiProviderEvent::ResponseCompleted {
                finish_reason: AiFinishReason::Stop,
            }),
        ])?;

        let mut stream = provider.stream_chat(provider_request()).await?;
        let first = stream.recv().await.transpose()?;
        let second = stream.recv().await.transpose()?;

        assert!(matches!(
            first,
            Some(AiProviderEvent::TextDelta { sequence: 1, .. })
        ));
        assert!(matches!(
            second,
            Some(AiProviderEvent::ResponseCompleted { .. })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn mock_task_service_rejects_stale_completion() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = MockAiTaskService::new();
        let owner_id = UserId::now_v7();
        let task = service.create_task(owner_id, task_command()).await?;
        let claim = service.claim_task(owner_id, task.id).await?;
        let request = CompleteAiTaskRequest {
            task_id: task.id,
            run_id: claim.run_id,
            claim_id: claim.claim_id,
            fence: claim.fence.saturating_add(1),
            task_revision: claim.task_revision,
            idempotency_key: "complete-summary-1".to_owned(),
            result: json!({
                "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                "content": "Результат",
                "citation_ids": ["ctx:1"]
            }),
        };

        assert_eq!(
            service.complete_task(owner_id, request).await,
            Err(MockAiTaskServiceError::StaleClaim)
        );
        Ok(())
    }

    #[tokio::test]
    async fn mock_task_service_deduplicates_active_create() -> Result<(), Box<dyn std::error::Error>>
    {
        let service = MockAiTaskService::new();
        let owner_id = UserId::now_v7();
        let command = task_command();
        let mut retried_command = command.clone();
        retried_command.idempotency_key = "create-summary-2".to_owned();

        let first = service.create_task(owner_id, command).await?;
        let second = service.create_task(owner_id, retried_command).await?;

        assert_eq!(first.id, second.id);
        Ok(())
    }
}
