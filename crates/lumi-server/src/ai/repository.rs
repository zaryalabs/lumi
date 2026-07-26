//! PostgreSQL repositories for durable AI tasks, runs, context packs and artifacts.

use std::time::Duration;

use lumi_core::{
    content_hash, AbridgementArtifactPayload, AiArtifact, AiArtifactAuthor, AiArtifactStatus,
    AiContextPack, AiExecutorKind, AiRun, AiRunStatus, AiSourceScope, AiTask, AiTaskClaim,
    AiTaskId, AiTaskStatus, AiTokenUsage, CompleteAiTaskRequest, CompleteAiTaskResult,
    CreateAiTaskCommand, SourceCitation, SummaryArtifact, SummaryForm, SummaryScopeKind,
    TaskMutationRequest, UpdateSummaryRequest, UserId, ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION,
    AI_CONTRACT_VERSION, SUMMARY_ARTIFACT_SCHEMA_VERSION,
};
use serde_json::Value;
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::jobs::{
    JobFailure, JobFence, JobRuntime, JobRuntimeError, JobRuntimeStatus, PgJobRepository,
};

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const DEFAULT_CLAIM_LEASE: Duration = Duration::from_secs(15 * 60);

/// Failure returned by production AI persistence.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum AiRepositoryError {
    /// Frozen core contract validation failed.
    #[error("invalid AI repository command: {0}")]
    Invalid(String),
    /// Object is absent or belongs to another account.
    #[error("AI object was not found")]
    NotFound,
    /// State, uniqueness or idempotency conflicts with the command.
    #[error("AI repository state conflict")]
    Conflict,
    /// Claim id, fence, task revision or lease is stale.
    #[error("AI task claim is stale")]
    StaleClaim,
    /// PostgreSQL returned an unavailable or invalid projection.
    #[error("AI repository is unavailable")]
    Storage,
}

impl From<JobRuntimeError> for AiRepositoryError {
    fn from(error: JobRuntimeError) -> Self {
        match error {
            JobRuntimeError::Invalid(message) => Self::Invalid(message.to_owned()),
            JobRuntimeError::NotFound => Self::NotFound,
            JobRuntimeError::Conflict => Self::Conflict,
            JobRuntimeError::StaleClaim => Self::StaleClaim,
            JobRuntimeError::Storage => Self::Storage,
        }
    }
}

/// Production PostgreSQL repository for the frozen AI persistence boundary.
#[derive(Clone)]
pub struct PgAiRepository {
    pool: PgPool,
    jobs: JobRuntime<PgJobRepository>,
}

/// Provider-neutral input required by the internal E2 worker.
pub(crate) struct AiTaskExecutionInput {
    pub task: AiTask,
    pub instruction: String,
}

/// Validated inputs needed by the Lumi-owned derived-material publisher.
pub(crate) struct AbridgementPublicationInput {
    pub artifact_id: Uuid,
    pub task_id: Uuid,
    pub source_material_id: Uuid,
    pub source_revision_id: Uuid,
    pub prompt_version: String,
    pub schema_version: String,
    pub payload: AbridgementArtifactPayload,
    pub source_refs: Vec<SourceCitation>,
}

impl PgAiRepository {
    /// Build the repository over an already-migrated PostgreSQL pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            jobs: JobRuntime::new(PgJobRepository::new(pool.clone())),
            pool,
        }
    }

    /// Create or replay one durable task with active deduplication.
    ///
    /// Source ownership and exact material/revision binding are checked before
    /// the task and its generic job are committed in one transaction.
    ///
    /// # Errors
    ///
    /// Returns a contract, ownership, idempotency or storage error.
    pub async fn create_task(
        &self,
        owner_id: UserId,
        command: CreateAiTaskCommand,
    ) -> Result<AiTask, AiRepositoryError> {
        command
            .validate()
            .map_err(|error| AiRepositoryError::Invalid(error.to_string()))?;
        let result_kind = output_kind(&command.output_schema_version)?;
        validate_artifact_slot_command(&command, &result_kind)?;
        let request_hash = hash_json(&serde_json::json!({
            "kind": &command.kind,
            "source_scope": &command.source_scope,
            "instruction": &command.instruction,
            "parameters": &command.parameters,
            "prompt_version": &command.prompt_version,
            "output_schema_version": &command.output_schema_version,
        }))?;
        if let Some((task, prior_hash)) = self
            .task_by_idempotency(owner_id, &command.idempotency_key)
            .await?
        {
            return if prior_hash == request_hash {
                Ok(task)
            } else {
                Err(AiRepositoryError::Conflict)
            };
        }
        let dedupe_key = request_hash.clone();
        if let Some(task) = self.active_task_by_dedupe(owner_id, &dedupe_key).await? {
            self.bind_creation_idempotency(
                owner_id,
                &command.idempotency_key,
                &request_hash,
                task.id,
            )
            .await?;
            return Ok(task);
        }
        let source_material_id = command.source_scope.material_id();
        let source_revision_id = command.source_scope.revision_id();
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let space_id: Uuid = sqlx::query(
            "SELECT m.space_id FROM materials m JOIN document_revisions r ON r.material_id = m.material_id WHERE m.material_id = $1 AND r.revision_id = $2 AND m.owner_user_id = $3 AND m.space_id = r.space_id AND m.deleted_at IS NULL",
        )
        .bind(source_material_id)
        .bind(source_revision_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .map(|row| row.try_get("space_id").map_err(storage_error))
        .transpose()?
        .ok_or(AiRepositoryError::NotFound)?;
        let task_id = Uuid::now_v7();
        let job_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO jobs (job_id, user_id, space_id, kind, payload_ref, status, stage, max_attempts) VALUES ($1, $2, $3, 'ai', $4, 'queued', 'queued', 3)",
        )
        .bind(job_id)
        .bind(owner_id)
        .bind(space_id)
        .bind(serde_json::json!({"task_id": task_id}))
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let inserted = sqlx::query(
            "INSERT INTO ai_tasks (task_id, user_id, space_id, job_id, contract_version, kind, result_kind, source_material_id, source_revision_id, source_scope, instruction, parameters, prompt_version, output_schema_version, status, idempotency_key, request_hash, dedupe_key) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'queued', $15, $16, $17) ON CONFLICT (user_id, dedupe_key) WHERE status IN ('queued', 'running', 'needs_input') DO NOTHING RETURNING task_id",
        )
        .bind(task_id)
        .bind(owner_id)
        .bind(space_id)
        .bind(job_id)
        .bind(AI_CONTRACT_VERSION)
        .bind(&command.kind)
        .bind(&result_kind)
        .bind(source_material_id)
        .bind(source_revision_id)
        .bind(serde_json::to_value(&command.source_scope).map_err(|_| AiRepositoryError::Storage)?)
        .bind(&command.instruction)
        .bind(&command.parameters)
        .bind(&command.prompt_version)
        .bind(&command.output_schema_version)
        .bind(&command.idempotency_key)
        .bind(&request_hash)
        .bind(&dedupe_key)
        .fetch_optional(&mut *transaction)
        .await;
        match inserted {
            Ok(Some(_)) => {
                sqlx::query(
                    "INSERT INTO ai_task_creations (user_id, idempotency_key, request_hash, task_id) VALUES ($1, $2, $3, $4)",
                )
                .bind(owner_id)
                .bind(&command.idempotency_key)
                .bind(&request_hash)
                .bind(task_id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
                transaction.commit().await.map_err(storage_error)?;
                self.task(owner_id, task_id).await
            }
            Ok(None) => {
                transaction.rollback().await.map_err(storage_error)?;
                self.active_task_by_dedupe(owner_id, &dedupe_key)
                    .await?
                    .ok_or(AiRepositoryError::Conflict)
            }
            Err(error) if unique_violation(&error) => {
                transaction.rollback().await.map_err(storage_error)?;
                if let Some((task, prior_hash)) = self
                    .task_by_idempotency(owner_id, &command.idempotency_key)
                    .await?
                {
                    if prior_hash == request_hash {
                        return Ok(task);
                    }
                }
                self.active_task_by_dedupe(owner_id, &dedupe_key)
                    .await?
                    .ok_or(AiRepositoryError::Conflict)
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    /// Read one account-owned task.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for a foreign or absent task.
    pub async fn task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
    ) -> Result<AiTask, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT task_id, user_id, contract_version, kind, result_kind, source_scope, parameters, prompt_version, output_schema_version, status, dedupe_key, object_revision FROM ai_tasks WHERE task_id = $1 AND user_id = $2",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        task_from_row(&row)
    }

    /// List account-owned tasks in deterministic creation order.
    ///
    /// # Errors
    ///
    /// Returns an error when PostgreSQL cannot produce a valid projection.
    pub async fn list_tasks(&self, owner_id: UserId) -> Result<Vec<AiTask>, AiRepositoryError> {
        let rows = sqlx::query(
            "SELECT task_id, user_id, contract_version, kind, result_kind, source_scope, parameters, prompt_version, output_schema_version, status, dedupe_key, object_revision FROM ai_tasks WHERE user_id = $1 ORDER BY created_at, task_id",
        )
        .bind(owner_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(task_from_row).collect()
    }

    /// Mark one queued task for the internal provider worker.
    pub async fn request_execute(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        request: &TaskMutationRequest,
    ) -> Result<AiTask, AiRepositoryError> {
        self.mutate_task(owner_id, task_id, request, "execute")
            .await
    }

    /// Request cooperative cancellation for queued or running work.
    pub async fn request_cancel(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        request: &TaskMutationRequest,
    ) -> Result<AiTask, AiRepositoryError> {
        self.mutate_task(owner_id, task_id, request, "cancel").await
    }

    /// Requeue one failed task without changing its durable identity.
    pub async fn retry_task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        request: &TaskMutationRequest,
    ) -> Result<AiTask, AiRepositoryError> {
        self.mutate_task(owner_id, task_id, request, "retry").await
    }

    async fn mutate_task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        request: &TaskMutationRequest,
        action: &'static str,
    ) -> Result<AiTask, AiRepositoryError> {
        if request.expected_revision == 0
            || request.idempotency_key.trim().is_empty()
            || request.idempotency_key.len() > 256
        {
            return Err(AiRepositoryError::Invalid(
                "task mutation is outside bounds".to_owned(),
            ));
        }
        let request_hash = hash_json(&serde_json::json!({
            "task_id": task_id,
            "action": action,
            "expected_revision": request.expected_revision,
        }))?;
        if let Some(row) = sqlx::query(
            "SELECT task_id, action, request_hash FROM ai_task_mutations WHERE user_id = $1 AND idempotency_key = $2",
        )
        .bind(owner_id)
        .bind(&request.idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        {
            let prior_task: Uuid = row.try_get("task_id").map_err(storage_error)?;
            let prior_action: String = row.try_get("action").map_err(storage_error)?;
            let prior_hash: String = row.try_get("request_hash").map_err(storage_error)?;
            return if prior_task == task_id
                && prior_action == action
                && prior_hash == request_hash
            {
                self.task(owner_id, task_id).await
            } else {
                Err(AiRepositoryError::Conflict)
            };
        }

        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT job_id, status, object_revision FROM ai_tasks WHERE task_id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let current_revision: i64 = row.try_get("object_revision").map_err(storage_error)?;
        if u64::try_from(current_revision).ok() != Some(request.expected_revision) {
            return Err(AiRepositoryError::Conflict);
        }
        let status: String = row.try_get("status").map_err(storage_error)?;
        let job_id: Uuid = row.try_get("job_id").map_err(storage_error)?;
        match (action, status.as_str()) {
            ("execute", "queued") => {
                sqlx::query(
                    "UPDATE ai_tasks SET internal_execution_requested = true, object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2",
                )
                .bind(task_id)
                .bind(owner_id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            }
            ("cancel", "queued" | "running") => {
                let job = sqlx::query(
                    "UPDATE jobs SET cancellation_requested = true, status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END, claim_id = CASE WHEN status = 'queued' THEN NULL ELSE claim_id END, lease_expires_at = CASE WHEN status = 'queued' THEN NULL ELSE lease_expires_at END, finished_at = CASE WHEN status = 'queued' THEN now() ELSE finished_at END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('queued', 'running') RETURNING status",
                )
                .bind(job_id)
                .bind(owner_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(storage_error)?
                .ok_or(AiRepositoryError::Conflict)?;
                let job_status: String = job.try_get("status").map_err(storage_error)?;
                sqlx::query(
                    "UPDATE ai_tasks SET status = CASE WHEN $3 = 'cancelled' THEN 'cancelled' ELSE status END, active_run_id = CASE WHEN $3 = 'cancelled' THEN NULL ELSE active_run_id END, internal_execution_requested = false, object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2",
                )
                .bind(task_id)
                .bind(owner_id)
                .bind(job_status)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            }
            ("retry", "failed" | "needs_input") => {
                sqlx::query(
                    "UPDATE jobs SET status = 'queued', stage = 'queued', progress = 0, cancellation_requested = false, error_code = NULL, error_message = NULL, finished_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status = 'failed'",
                )
                .bind(job_id)
                .bind(owner_id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
                sqlx::query(
                    "UPDATE ai_tasks SET status = 'queued', active_run_id = NULL, result_artifact_id = NULL, internal_execution_requested = false, object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2",
                )
                .bind(task_id)
                .bind(owner_id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            }
            _ => return Err(AiRepositoryError::Conflict),
        }
        let resulting_revision = current_revision.saturating_add(1);
        sqlx::query(
            "INSERT INTO ai_task_mutations (user_id, idempotency_key, task_id, action, request_hash, resulting_revision) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(owner_id)
        .bind(&request.idempotency_key)
        .bind(task_id)
        .bind(action)
        .bind(request_hash)
        .bind(resulting_revision)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if unique_violation(&error) {
                AiRepositoryError::Conflict
            } else {
                storage_error(error)
            }
        })?;
        transaction.commit().await.map_err(storage_error)?;
        self.task(owner_id, task_id).await
    }

    /// Return the oldest queued task explicitly admitted to the internal worker.
    pub(crate) async fn next_internal_task(
        &self,
    ) -> Result<Option<(UserId, AiTaskId)>, AiRepositoryError> {
        sqlx::query(
            "SELECT user_id, task_id FROM ai_tasks WHERE status = 'queued' AND internal_execution_requested ORDER BY priority DESC, created_at, task_id LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .map(|row| {
            Ok((
                row.try_get("user_id").map_err(storage_error)?,
                row.try_get("task_id").map_err(storage_error)?,
            ))
        })
        .transpose()
    }

    /// Load instruction and frozen projection for an internal execution.
    pub(crate) async fn execution_input(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
    ) -> Result<AiTaskExecutionInput, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT task_id, user_id, contract_version, kind, result_kind, source_scope, parameters, prompt_version, output_schema_version, status, dedupe_key, object_revision, instruction FROM ai_tasks WHERE task_id = $1 AND user_id = $2",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        Ok(AiTaskExecutionInput {
            task: task_from_row(&row)?,
            instruction: row.try_get("instruction").map_err(storage_error)?,
        })
    }

    /// Fail queued work that cannot produce a valid immutable context pack.
    pub(crate) async fn fail_unclaimed_task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        code: &str,
    ) -> Result<AiTask, AiRepositoryError> {
        if code.is_empty() || code.len() > 128 {
            return Err(AiRepositoryError::Invalid(
                "failure code is outside bounds".to_owned(),
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT job_id FROM ai_tasks WHERE task_id = $1 AND user_id = $2 AND status = 'queued' FOR UPDATE",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::Conflict)?;
        let job_id: Uuid = row.try_get("job_id").map_err(storage_error)?;
        sqlx::query(
            "UPDATE jobs SET status = 'failed', error_code = $3, error_message = 'AI source context is unavailable', finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status = 'queued'",
        )
        .bind(job_id)
        .bind(owner_id)
        .bind(code)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE ai_tasks SET status = 'failed', internal_execution_requested = false, object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2 AND status = 'queued'",
        )
        .bind(task_id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        self.task(owner_id, task_id).await
    }

    /// Persist one immutable, validated context pack for an account-owned task.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid pack, source mismatch, foreign task or
    /// duplicate immutable identity.
    pub async fn store_context_pack(
        &self,
        owner_id: UserId,
        pack: &AiContextPack,
    ) -> Result<(), AiRepositoryError> {
        pack.validate()
            .map_err(|error| AiRepositoryError::Invalid(error.to_string()))?;
        if pack.owner_id != owner_id {
            return Err(AiRepositoryError::Invalid(
                "context pack owner/hash mismatch".to_owned(),
            ));
        }
        let task_id = pack.task_id.ok_or_else(|| {
            AiRepositoryError::Invalid("task context pack requires task_id".to_owned())
        })?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT task.space_id, task.source_material_id, task.source_revision_id FROM ai_tasks AS task JOIN materials AS material ON material.material_id = task.source_material_id AND material.owner_user_id = task.user_id AND material.space_id = task.space_id JOIN document_revisions AS revision ON revision.revision_id = task.source_revision_id AND revision.material_id = material.material_id AND revision.space_id = material.space_id WHERE task.task_id = $1 AND task.user_id = $2 AND material.deleted_at IS NULL FOR SHARE OF task, material, revision",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let space_id: Uuid = row.try_get("space_id").map_err(storage_error)?;
        let material_id: Uuid = row.try_get("source_material_id").map_err(storage_error)?;
        let revision_id: Uuid = row.try_get("source_revision_id").map_err(storage_error)?;
        if pack.scope.material_id() != material_id || pack.scope.revision_id() != revision_id {
            return Err(AiRepositoryError::Invalid(
                "context pack source does not match task".to_owned(),
            ));
        }
        let source_refs =
            serde_json::to_value(&pack.citations).map_err(|_| AiRepositoryError::Storage)?;
        let payload = serde_json::to_value(pack).map_err(|_| AiRepositoryError::Storage)?;
        sqlx::query(
            "INSERT INTO ai_context_packs (context_pack_id, user_id, space_id, task_id, schema_version, limits_version, source_material_id, source_revision_id, pack_hash, permission_snapshot, source_refs, payload) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(pack.context_pack_id)
        .bind(owner_id)
        .bind(space_id)
        .bind(task_id)
        .bind(&pack.schema_version)
        .bind(&pack.limits_version)
        .bind(material_id)
        .bind(revision_id)
        .bind(&pack.pack_hash)
        .bind(serde_json::to_value(&pack.permission_snapshot).map_err(|_| AiRepositoryError::Storage)?)
        .bind(source_refs)
        .bind(payload)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if unique_violation(&error) {
                AiRepositoryError::Conflict
            } else {
                storage_error(error)
            }
        })?;
        transaction.commit().await.map_err(storage_error)
    }

    /// Read one immutable context pack through owner scope.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for a foreign/absent pack or `Storage` for corrupt
    /// persisted JSON.
    pub async fn context_pack(
        &self,
        owner_id: UserId,
        context_pack_id: Uuid,
    ) -> Result<AiContextPack, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT task_id, source_material_id, source_revision_id, pack_hash, payload FROM ai_context_packs WHERE context_pack_id = $1 AND user_id = $2",
        )
        .bind(context_pack_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let payload: Value = row.try_get("payload").map_err(storage_error)?;
        let pack: AiContextPack =
            serde_json::from_value(payload).map_err(|_| AiRepositoryError::Storage)?;
        pack.validate().map_err(|_| AiRepositoryError::Storage)?;
        if pack.context_pack_id != context_pack_id
            || pack.owner_id != owner_id
            || pack.task_id != row.try_get("task_id").map_err(storage_error)?
            || pack.scope.material_id()
                != row
                    .try_get::<Uuid, _>("source_material_id")
                    .map_err(storage_error)?
            || pack.scope.revision_id()
                != row
                    .try_get::<Uuid, _>("source_revision_id")
                    .map_err(storage_error)?
            || pack.pack_hash
                != row
                    .try_get::<String, _>("pack_hash")
                    .map_err(storage_error)?
        {
            return Err(AiRepositoryError::Storage);
        }
        Ok(pack)
    }

    /// Read one account-owned technical execution attempt.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for a foreign/absent run or `Storage` for a corrupt
    /// persisted projection.
    pub async fn run(&self, owner_id: UserId, run_id: Uuid) -> Result<AiRun, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT run_id, task_id, executor_kind, executor_ref, status, claim_id, fence, lease_expires_at, attempt, prompt_version, output_schema_version, context_pack_id, provider_kind, model, usage, progress, error_code, started_at, finished_at FROM ai_runs WHERE run_id = $1 AND user_id = $2",
        )
        .bind(run_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        run_from_row(&row)
    }

    /// Atomically claim a queued task for a typed executor.
    ///
    /// The task transition, generic job claim and immutable `AiRun` insertion
    /// commit together. A task without a validated context pack cannot be
    /// claimed.
    ///
    /// # Errors
    ///
    /// Returns a state, ownership, context or storage error.
    pub async fn claim_task(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        executor_kind: AiExecutorKind,
        executor_ref: Option<&str>,
    ) -> Result<AiTaskClaim, AiRepositoryError> {
        self.claim_task_with_lease(
            owner_id,
            task_id,
            executor_kind,
            executor_ref,
            DEFAULT_CLAIM_LEASE,
        )
        .await
    }

    /// Atomically claim with an explicit bounded lease, primarily for workers
    /// and deterministic integration tests.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::claim_task`].
    pub async fn claim_task_with_lease(
        &self,
        owner_id: UserId,
        task_id: AiTaskId,
        executor_kind: AiExecutorKind,
        executor_ref: Option<&str>,
        lease: Duration,
    ) -> Result<AiTaskClaim, AiRepositoryError> {
        if executor_ref.is_some_and(|value| value.is_empty() || value.len() > 256) {
            return Err(AiRepositoryError::Invalid(
                "executor_ref is outside bounds".to_owned(),
            ));
        }
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let task = sqlx::query(
            "SELECT task.job_id, task.space_id, task.status, task.object_revision, task.result_kind, task.output_schema_version, task.prompt_version FROM ai_tasks AS task JOIN materials AS material ON material.material_id = task.source_material_id AND material.owner_user_id = task.user_id AND material.space_id = task.space_id JOIN document_revisions AS revision ON revision.revision_id = task.source_revision_id AND revision.material_id = material.material_id AND revision.space_id = material.space_id WHERE task.task_id = $1 AND task.user_id = $2 AND material.deleted_at IS NULL FOR UPDATE OF task",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let status: String = task.try_get("status").map_err(storage_error)?;
        if status != "queued" {
            return Err(AiRepositoryError::Conflict);
        }
        let task_revision: i64 = task.try_get("object_revision").map_err(storage_error)?;
        let context_pack_id: Uuid = sqlx::query(
            "SELECT context.context_pack_id FROM ai_context_packs AS context WHERE context.task_id = $1 AND context.user_id = $2 AND NOT EXISTS (SELECT 1 FROM ai_runs AS run WHERE run.context_pack_id = context.context_pack_id) ORDER BY context.created_at DESC, context.context_pack_id DESC LIMIT 1",
        )
        .bind(task_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .map(|row| row.try_get("context_pack_id").map_err(storage_error))
        .transpose()?
        .ok_or(AiRepositoryError::Conflict)?;
        let job_id: Uuid = task.try_get("job_id").map_err(storage_error)?;
        let claim_id = Uuid::now_v7();
        let job_claim = PgJobRepository::claim_in_transaction(
            &mut transaction,
            owner_id,
            job_id,
            claim_id,
            lease,
        )
        .await?;
        let run_id = Uuid::now_v7();
        let space_id: Uuid = task.try_get("space_id").map_err(storage_error)?;
        let executor_kind_db = match executor_kind {
            AiExecutorKind::InternalProvider => "internal_provider",
            AiExecutorKind::McpAgent => "mcp_agent",
        };
        sqlx::query(
            "INSERT INTO ai_runs (run_id, task_id, job_id, user_id, space_id, context_pack_id, executor_kind, executor_ref, status, claim_id, fence, claimed_task_revision, lease_expires_at, attempt, prompt_version, output_schema_version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'running', $9, $10, $11, $12, $13, $14, $15)",
        )
        .bind(run_id)
        .bind(task_id)
        .bind(job_id)
        .bind(owner_id)
        .bind(space_id)
        .bind(context_pack_id)
        .bind(executor_kind_db)
        .bind(executor_ref)
        .bind(job_claim.claim.claim_id)
        .bind(i64::try_from(job_claim.claim.fence).map_err(|_| AiRepositoryError::Storage)?)
        .bind(task_revision)
        .bind(job_claim.lease_expires_at)
        .bind(i32::try_from(job_claim.job.attempt).map_err(|_| AiRepositoryError::Storage)?)
        .bind(task.try_get::<String, _>("prompt_version").map_err(storage_error)?)
        .bind(task.try_get::<String, _>("output_schema_version").map_err(storage_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let updated = sqlx::query(
            "UPDATE ai_tasks SET status = 'running', active_run_id = $3, object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2 AND status = 'queued' AND object_revision = $4",
        )
        .bind(task_id)
        .bind(owner_id)
        .bind(run_id)
        .bind(task_revision)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if updated.rows_affected() != 1 {
            return Err(AiRepositoryError::Conflict);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(AiTaskClaim {
            task_id,
            run_id,
            claim_id: job_claim.claim.claim_id,
            fence: job_claim.claim.fence,
            lease_expires_at: job_claim.lease_expires_at.to_string(),
            task_revision: u64::try_from(task_revision).map_err(|_| AiRepositoryError::Storage)?,
            result_kind: task.try_get("result_kind").map_err(storage_error)?,
            result_schema_version: task
                .try_get("output_schema_version")
                .map_err(storage_error)?,
            context_pack_id,
        })
    }

    /// Extend a live claim and update monotonic progress transactionally.
    ///
    /// # Errors
    ///
    /// Returns `StaleClaim` after lease expiry or fencing mismatch.
    pub async fn heartbeat(
        &self,
        owner_id: UserId,
        claim: &AiTaskClaim,
        progress: f32,
        stage: Option<&str>,
        lease: Duration,
    ) -> Result<bool, AiRepositoryError> {
        if !progress.is_finite()
            || !(0.0..=1.0).contains(&progress)
            || stage.is_some_and(|value| value.is_empty() || value.len() > 128)
        {
            return Err(AiRepositoryError::Invalid(
                "progress update is outside bounds".to_owned(),
            ));
        }
        let job_id: Uuid =
            sqlx::query("SELECT job_id FROM ai_tasks WHERE task_id = $1 AND user_id = $2")
                .bind(claim.task_id)
                .bind(owner_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_error)?
                .map(|row| row.try_get("job_id").map_err(storage_error))
                .transpose()?
                .ok_or(AiRepositoryError::NotFound)?;
        let fence = JobFence {
            job_id,
            claim_id: claim.claim_id,
            fence: claim.fence,
        };
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let cancelled =
            PgJobRepository::heartbeat_in_transaction(&mut transaction, owner_id, &fence, lease)
                .await?;
        let updated = sqlx::query(
            "UPDATE ai_runs SET progress = GREATEST(progress, $7), progress_stage = COALESCE($8, progress_stage), lease_expires_at = now() + ($9 * interval '1 second') WHERE run_id = $1 AND task_id = $2 AND user_id = $3 AND claim_id = $4 AND fence = $5 AND claimed_task_revision = $6 AND status = 'running' AND lease_expires_at > now()",
        )
        .bind(claim.run_id)
        .bind(claim.task_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| AiRepositoryError::Storage)?)
        .bind(i64::try_from(claim.task_revision).map_err(|_| AiRepositoryError::Storage)?)
        .bind(progress)
        .bind(stage)
        .bind(i64::try_from(lease.as_secs()).map_err(|_| AiRepositoryError::Invalid("lease is too large".to_owned()))?)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if updated.rows_affected() != 1 {
            return Err(AiRepositoryError::StaleClaim);
        }
        transaction.commit().await.map_err(storage_error)?;
        Ok(cancelled)
    }

    /// Validate and transactionally publish one typed artifact.
    ///
    /// Artifact insertion, generic job completion, run completion, task
    /// completion and idempotency record are one PostgreSQL transaction.
    ///
    /// # Errors
    ///
    /// Returns a validation, owner, stale-claim, idempotency or storage error.
    pub async fn complete_task(
        &self,
        owner_id: UserId,
        request: CompleteAiTaskRequest,
    ) -> Result<CompleteAiTaskResult, AiRepositoryError> {
        if let Some(replay) = self.completion_replay(owner_id, &request).await? {
            return Ok(replay);
        }
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 4))")
            .bind(request.task_id.to_string())
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if let Some(replay) =
            completion_replay_in_transaction(&mut transaction, owner_id, &request).await?
        {
            transaction.rollback().await.map_err(storage_error)?;
            return Ok(replay);
        }
        let row = sqlx::query(
            "SELECT task.job_id, task.space_id, task.status AS task_status, task.active_run_id, task.output_schema_version, task.result_kind, task.source_material_id, task.source_revision_id, task.source_scope, task.parameters, run.context_pack_id, run.status AS run_status, run.claim_id, run.fence, run.claimed_task_revision, run.lease_expires_at, context.payload AS context_payload, context.source_refs FROM ai_tasks AS task JOIN ai_runs AS run ON run.run_id = task.active_run_id AND run.task_id = task.task_id AND run.user_id = task.user_id AND run.space_id = task.space_id JOIN ai_context_packs AS context ON context.context_pack_id = run.context_pack_id AND context.task_id = task.task_id AND context.user_id = task.user_id AND context.space_id = task.space_id JOIN materials AS material ON material.material_id = task.source_material_id AND material.owner_user_id = task.user_id AND material.space_id = task.space_id JOIN document_revisions AS revision ON revision.revision_id = task.source_revision_id AND revision.material_id = material.material_id AND revision.space_id = material.space_id WHERE task.task_id = $1 AND task.user_id = $2 AND material.deleted_at IS NULL FOR UPDATE OF task, run",
        )
        .bind(request.task_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let row = if let Some(row) = row {
            row
        } else {
            let exists: bool = sqlx::query(
                "SELECT EXISTS(SELECT 1 FROM ai_tasks WHERE task_id = $1 AND user_id = $2) AS present",
            )
            .bind(request.task_id)
            .bind(owner_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(storage_error)?
            .try_get("present")
            .map_err(storage_error)?;
            return Err(if exists {
                AiRepositoryError::StaleClaim
            } else {
                AiRepositoryError::NotFound
            });
        };
        let expected_schema: String = row
            .try_get("output_schema_version")
            .map_err(storage_error)?;
        request
            .validate(&expected_schema)
            .map_err(|error| AiRepositoryError::Invalid(error.to_string()))?;
        validate_completion_fence(&row, &request)?;
        let context_payload: Value = row.try_get("context_payload").map_err(storage_error)?;
        let context_pack: AiContextPack =
            serde_json::from_value(context_payload).map_err(|_| AiRepositoryError::Storage)?;
        context_pack
            .validate()
            .map_err(|_| AiRepositoryError::Storage)?;
        if context_pack.owner_id != owner_id
            || context_pack.task_id != Some(request.task_id)
            || context_pack.scope.material_id()
                != row
                    .try_get::<Uuid, _>("source_material_id")
                    .map_err(storage_error)?
            || context_pack.scope.revision_id()
                != row
                    .try_get::<Uuid, _>("source_revision_id")
                    .map_err(storage_error)?
        {
            return Err(AiRepositoryError::Storage);
        }
        let source_refs: Value = row.try_get("source_refs").map_err(storage_error)?;
        if source_refs
            != serde_json::to_value(&context_pack.citations)
                .map_err(|_| AiRepositoryError::Storage)?
        {
            return Err(AiRepositoryError::Storage);
        }
        validate_result_citations(&context_pack, &request.result)?;
        let request_hash =
            hash_json(&serde_json::to_value(&request).map_err(|_| AiRepositoryError::Storage)?)?;
        let payload_hash = hash_json(&request.result)?;
        let artifact_id = Uuid::now_v7();
        let space_id: Uuid = row.try_get("space_id").map_err(storage_error)?;
        let source_material_id: Uuid = row.try_get("source_material_id").map_err(storage_error)?;
        let source_revision_id: Uuid = row.try_get("source_revision_id").map_err(storage_error)?;
        let result_kind: String = row.try_get("result_kind").map_err(storage_error)?;
        let (scope_kind, scope_ref, summary_form) = artifact_slot_from_row(&row, &result_kind)?;
        sqlx::query(
            "INSERT INTO ai_artifacts (artifact_id, task_id, run_id, user_id, space_id, kind, schema_version, payload, payload_hash, source_material_id, source_revision_id, scope_kind, scope_ref, summary_form, source_refs, status, authored_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, 'candidate', 'ai')",
        )
        .bind(artifact_id)
        .bind(request.task_id)
        .bind(request.run_id)
        .bind(owner_id)
        .bind(space_id)
        .bind(&result_kind)
        .bind(&expected_schema)
        .bind(&request.result)
        .bind(&payload_hash)
        .bind(source_material_id)
        .bind(source_revision_id)
        .bind(scope_kind)
        .bind(scope_ref)
        .bind(summary_form)
        .bind(source_refs)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if unique_violation(&error) {
                AiRepositoryError::Conflict
            } else {
                storage_error(error)
            }
        })?;
        let job_id: Uuid = row.try_get("job_id").map_err(storage_error)?;
        PgJobRepository::complete_in_transaction(
            &mut transaction,
            owner_id,
            &JobFence {
                job_id,
                claim_id: request.claim_id,
                fence: request.fence,
            },
        )
        .await?;
        let run_updated = sqlx::query(
            "UPDATE ai_runs SET status = 'succeeded', progress = 1, claim_id = NULL, lease_expires_at = NULL, finished_at = now() WHERE run_id = $1 AND task_id = $2 AND user_id = $3 AND status = 'running'",
        )
        .bind(request.run_id)
        .bind(request.task_id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let task_updated = sqlx::query(
            "UPDATE ai_tasks SET status = 'succeeded', result_artifact_id = $3, active_run_id = NULL, object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2 AND status = 'running' AND active_run_id = $4",
        )
        .bind(request.task_id)
        .bind(owner_id)
        .bind(artifact_id)
        .bind(request.run_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if run_updated.rows_affected() != 1 || task_updated.rows_affected() != 1 {
            return Err(AiRepositoryError::StaleClaim);
        }
        sqlx::query(
            "INSERT INTO ai_task_completions (task_id, idempotency_key, user_id, run_id, claim_id, fence, request_hash, artifact_id, result_schema_version) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(request.task_id)
        .bind(&request.idempotency_key)
        .bind(owner_id)
        .bind(request.run_id)
        .bind(request.claim_id)
        .bind(i64::try_from(request.fence).map_err(|_| AiRepositoryError::Storage)?)
        .bind(request_hash)
        .bind(artifact_id)
        .bind(&expected_schema)
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            if unique_violation(&error) {
                AiRepositoryError::Conflict
            } else {
                storage_error(error)
            }
        })?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(CompleteAiTaskResult {
            task_id: request.task_id,
            run_id: request.run_id,
            artifact_id,
            result_schema_version: expected_schema,
            replayed: false,
        })
    }

    /// Read one typed artifact through owner scope.
    ///
    /// # Errors
    ///
    /// Returns `NotFound` for a foreign/absent artifact.
    pub async fn artifact(
        &self,
        owner_id: UserId,
        artifact_id: Uuid,
    ) -> Result<AiArtifact, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT artifact_id, task_id, run_id, kind, schema_version, payload, status, object_revision, created_at, updated_at FROM ai_artifacts WHERE artifact_id = $1 AND user_id = $2",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        artifact_from_row(&row)
    }

    /// Load one completed abridgement candidate through exact owner/source joins.
    pub(crate) async fn abridgement_publication_input(
        &self,
        owner_id: UserId,
        artifact_id: Uuid,
    ) -> Result<AbridgementPublicationInput, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT artifact.artifact_id, artifact.task_id, artifact.source_material_id, artifact.source_revision_id, artifact.schema_version, artifact.payload, artifact.source_refs, task.prompt_version FROM ai_artifacts artifact JOIN ai_tasks task ON task.task_id = artifact.task_id AND task.user_id = artifact.user_id AND task.space_id = artifact.space_id JOIN materials material ON material.material_id = artifact.source_material_id AND material.owner_user_id = artifact.user_id AND material.space_id = artifact.space_id JOIN document_revisions revision ON revision.revision_id = artifact.source_revision_id AND revision.material_id = material.material_id AND revision.space_id = material.space_id WHERE artifact.artifact_id = $1 AND artifact.user_id = $2 AND artifact.kind = 'abridgement_artifact' AND artifact.status IN ('candidate', 'active') AND task.status = 'succeeded' AND material.deleted_at IS NULL",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let payload: Value = row.try_get("payload").map_err(storage_error)?;
        let payload: AbridgementArtifactPayload =
            serde_json::from_value(payload).map_err(|_| AiRepositoryError::Storage)?;
        payload.validate().map_err(|_| AiRepositoryError::Storage)?;
        let source_refs: Value = row.try_get("source_refs").map_err(storage_error)?;
        let source_refs: Vec<SourceCitation> =
            serde_json::from_value(source_refs).map_err(|_| AiRepositoryError::Storage)?;
        let source_material_id: Uuid = row.try_get("source_material_id").map_err(storage_error)?;
        let source_revision_id: Uuid = row.try_get("source_revision_id").map_err(storage_error)?;
        if source_refs.is_empty()
            || source_refs.iter().any(|citation| {
                citation.material_id != source_material_id
                    || citation.revision_id != source_revision_id
            })
        {
            return Err(AiRepositoryError::Storage);
        }
        Ok(AbridgementPublicationInput {
            artifact_id: row.try_get("artifact_id").map_err(storage_error)?,
            task_id: row.try_get("task_id").map_err(storage_error)?,
            source_material_id,
            source_revision_id,
            prompt_version: row.try_get("prompt_version").map_err(storage_error)?,
            schema_version: row.try_get("schema_version").map_err(storage_error)?,
            payload,
            source_refs,
        })
    }

    /// Reconcile a failed or cancelled internal execution through the common
    /// job runtime and its task/run projections.
    pub(crate) async fn fail_task(
        &self,
        owner_id: UserId,
        claim: &AiTaskClaim,
        code: &str,
        retryable: bool,
    ) -> Result<AiTask, AiRepositoryError> {
        let job_id: Uuid =
            sqlx::query("SELECT job_id FROM ai_tasks WHERE task_id = $1 AND user_id = $2")
                .bind(claim.task_id)
                .bind(owner_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage_error)?
                .map(|row| row.try_get("job_id").map_err(storage_error))
                .transpose()?
                .ok_or(AiRepositoryError::NotFound)?;
        let job = self
            .jobs
            .fail(
                Some(owner_id),
                &JobFence {
                    job_id,
                    claim_id: claim.claim_id,
                    fence: claim.fence,
                },
                &JobFailure {
                    code: code.to_owned(),
                    message: "AI provider execution did not complete".to_owned(),
                    retryable,
                },
            )
            .await?;
        let (task_status, run_status) = match job.status {
            JobRuntimeStatus::Queued => ("queued", "released"),
            JobRuntimeStatus::Cancelled => ("cancelled", "cancelled"),
            JobRuntimeStatus::Failed => ("failed", "failed"),
            _ => return Err(AiRepositoryError::Storage),
        };
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "UPDATE ai_runs SET status = $4, claim_id = NULL, lease_expires_at = NULL, error_code = $5, finished_at = now() WHERE run_id = $1 AND task_id = $2 AND user_id = $3 AND status = 'running'",
        )
        .bind(claim.run_id)
        .bind(claim.task_id)
        .bind(owner_id)
        .bind(run_status)
        .bind(code)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "UPDATE ai_tasks SET status = $3, active_run_id = NULL, internal_execution_requested = ($3 = 'queued'), object_revision = object_revision + 1, updated_at = now() WHERE task_id = $1 AND user_id = $2 AND status = 'running'",
        )
        .bind(claim.task_id)
        .bind(owner_id)
        .bind(task_status)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        self.task(owner_id, claim.task_id).await
    }

    /// Promote a generated candidate unless it would overwrite a user edit.
    pub(crate) async fn activate_generated_summary(
        &self,
        owner_id: UserId,
        artifact_id: Uuid,
    ) -> Result<SummaryArtifact, AiRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        let candidate = sqlx::query(
            "SELECT source_revision_id, scope_kind, scope_ref, summary_form FROM ai_artifacts WHERE artifact_id = $1 AND user_id = $2 AND kind = 'summary_artifact' AND status = 'candidate' FOR UPDATE",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let active = sqlx::query(
            "SELECT artifact_id, authored_by FROM ai_artifacts WHERE user_id = $1 AND source_revision_id = $2 AND scope_kind = $3 AND scope_ref = $4 AND summary_form = $5 AND kind = 'summary_artifact' AND status = 'active' FOR UPDATE",
        )
        .bind(owner_id)
        .bind(candidate.try_get::<Uuid, _>("source_revision_id").map_err(storage_error)?)
        .bind(candidate.try_get::<String, _>("scope_kind").map_err(storage_error)?)
        .bind(candidate.try_get::<String, _>("scope_ref").map_err(storage_error)?)
        .bind(candidate.try_get::<String, _>("summary_form").map_err(storage_error)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?;
        if active.as_ref().is_some_and(|row| {
            row.try_get::<String, _>("authored_by").ok().as_deref() == Some("user")
        }) {
            transaction.commit().await.map_err(storage_error)?;
            return self.summary(owner_id, artifact_id).await;
        }
        if let Some(active) = active {
            let active_id: Uuid = active.try_get("artifact_id").map_err(storage_error)?;
            sqlx::query(
                "UPDATE ai_artifacts SET status = 'superseded', object_revision = object_revision + 1, updated_at = now() WHERE artifact_id = $1 AND user_id = $2",
            )
            .bind(active_id)
            .bind(owner_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
            sqlx::query(
                "UPDATE ai_artifacts SET supersedes_artifact_id = $3 WHERE artifact_id = $1 AND user_id = $2",
            )
            .bind(artifact_id)
            .bind(owner_id)
            .bind(active_id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        }
        sqlx::query(
            "UPDATE ai_artifacts SET status = 'active', object_revision = object_revision + 1, updated_at = now() WHERE artifact_id = $1 AND user_id = $2 AND status = 'candidate'",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        self.summary(owner_id, artifact_id).await
    }

    /// List active and pending summary revisions for one owned material.
    pub async fn list_summaries(
        &self,
        owner_id: UserId,
        material_id: Uuid,
    ) -> Result<Vec<SummaryArtifact>, AiRepositoryError> {
        let rows = sqlx::query(
            "SELECT artifact_id, task_id, run_id, kind, schema_version, status, source_revision_id, scope_kind, scope_ref, summary_form, payload, authored_by, artifact_revision FROM ai_artifacts WHERE user_id = $1 AND source_material_id = $2 AND kind = 'summary_artifact' AND status IN ('active', 'candidate') ORDER BY created_at, artifact_id",
        )
        .bind(owner_id)
        .bind(material_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(summary_from_row).collect()
    }

    /// Read one account-owned summary projection.
    pub async fn summary(
        &self,
        owner_id: UserId,
        artifact_id: Uuid,
    ) -> Result<SummaryArtifact, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT artifact_id, task_id, run_id, kind, schema_version, status, source_revision_id, scope_kind, scope_ref, summary_form, payload, authored_by, artifact_revision FROM ai_artifacts WHERE artifact_id = $1 AND user_id = $2 AND kind = 'summary_artifact'",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        summary_from_row(&row)
    }

    /// Accept or reject one generated candidate with optimistic concurrency.
    pub async fn mutate_artifact(
        &self,
        owner_id: UserId,
        artifact_id: Uuid,
        expected_revision: u64,
        idempotency_key: &str,
        accept: bool,
    ) -> Result<AiArtifact, AiRepositoryError> {
        if expected_revision == 0
            || idempotency_key.trim().is_empty()
            || idempotency_key.len() > 256
        {
            return Err(AiRepositoryError::Invalid(
                "artifact mutation is outside bounds".to_owned(),
            ));
        }
        let action = if accept { "accept" } else { "reject" };
        let request_hash = hash_json(&serde_json::json!({
            "artifact_id": artifact_id,
            "expected_revision": expected_revision,
            "action": action,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 6))")
            .bind(format!("{owner_id}:{idempotency_key}"))
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if let Some(replay) = sqlx::query(
            "SELECT artifact_id, action, request_hash, resulting_artifact_id FROM ai_artifact_mutations WHERE user_id = $1 AND idempotency_key = $2",
        )
        .bind(owner_id)
        .bind(idempotency_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        {
            let matches = replay
                .try_get::<Uuid, _>("artifact_id")
                .map_err(storage_error)?
                == artifact_id
                && replay
                    .try_get::<String, _>("action")
                    .map_err(storage_error)?
                    == action
                && replay
                    .try_get::<String, _>("request_hash")
                    .map_err(storage_error)?
                    == request_hash;
            let resulting_artifact_id = replay
                .try_get::<Uuid, _>("resulting_artifact_id")
                .map_err(storage_error)?;
            transaction.rollback().await.map_err(storage_error)?;
            if !matches {
                return Err(AiRepositoryError::Conflict);
            }
            return self.artifact(owner_id, resulting_artifact_id).await;
        }
        let row = sqlx::query(
            "SELECT source_revision_id, scope_kind, scope_ref, summary_form, status, object_revision FROM ai_artifacts WHERE artifact_id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let object_revision: i64 = row.try_get("object_revision").map_err(storage_error)?;
        if row.try_get::<String, _>("status").map_err(storage_error)? != "candidate"
            || u64::try_from(object_revision).ok() != Some(expected_revision)
        {
            return Err(AiRepositoryError::Conflict);
        }
        if accept {
            sqlx::query(
                "UPDATE ai_artifacts SET status = 'superseded', object_revision = object_revision + 1, updated_at = now() WHERE user_id = $1 AND source_revision_id = $2 AND scope_kind = $3 AND scope_ref = $4 AND summary_form = $5 AND kind = 'summary_artifact' AND status = 'active'",
            )
            .bind(owner_id)
            .bind(row.try_get::<Uuid, _>("source_revision_id").map_err(storage_error)?)
            .bind(row.try_get::<String, _>("scope_kind").map_err(storage_error)?)
            .bind(row.try_get::<String, _>("scope_ref").map_err(storage_error)?)
            .bind(row.try_get::<String, _>("summary_form").map_err(storage_error)?)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        }
        sqlx::query(
            "UPDATE ai_artifacts SET status = $3, object_revision = object_revision + 1, updated_at = now() WHERE artifact_id = $1 AND user_id = $2",
        )
        .bind(artifact_id)
        .bind(owner_id)
        .bind(if accept { "active" } else { "rejected" })
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO ai_artifact_mutations (user_id, idempotency_key, artifact_id, action, request_hash, resulting_artifact_id) VALUES ($1, $2, $3, $4, $5, $3)",
        )
        .bind(owner_id)
        .bind(idempotency_key)
        .bind(artifact_id)
        .bind(action)
        .bind(request_hash)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        self.artifact(owner_id, artifact_id).await
    }

    /// Create an immutable user-authored revision and replace the active slot.
    pub async fn update_summary(
        &self,
        owner_id: UserId,
        summary_id: Uuid,
        request: &UpdateSummaryRequest,
    ) -> Result<SummaryArtifact, AiRepositoryError> {
        if request.content.trim().is_empty()
            || request.content.len() > 256 * 1024
            || request.expected_revision == 0
            || request.idempotency_key.trim().is_empty()
            || request.idempotency_key.len() > 256
        {
            return Err(AiRepositoryError::Invalid(
                "summary edit is outside bounds".to_owned(),
            ));
        }
        let content = request.content.trim();
        let request_hash = hash_json(&serde_json::json!({
            "artifact_id": summary_id,
            "expected_revision": request.expected_revision,
            "action": "edit",
            "content": content,
        }))?;
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 6))")
            .bind(format!("{owner_id}:{}", request.idempotency_key))
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if let Some(replay) = sqlx::query(
            "SELECT artifact_id, action, request_hash, resulting_artifact_id FROM ai_artifact_mutations WHERE user_id = $1 AND idempotency_key = $2",
        )
        .bind(owner_id)
        .bind(&request.idempotency_key)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        {
            let matches = replay
                .try_get::<Uuid, _>("artifact_id")
                .map_err(storage_error)?
                == summary_id
                && replay
                    .try_get::<String, _>("action")
                    .map_err(storage_error)?
                    == "edit"
                && replay
                    .try_get::<String, _>("request_hash")
                    .map_err(storage_error)?
                    == request_hash;
            let resulting_artifact_id = replay
                .try_get::<Uuid, _>("resulting_artifact_id")
                .map_err(storage_error)?;
            transaction.rollback().await.map_err(storage_error)?;
            if !matches {
                return Err(AiRepositoryError::Conflict);
            }
            return self.summary(owner_id, resulting_artifact_id).await;
        }
        let row = sqlx::query(
            "SELECT * FROM ai_artifacts WHERE artifact_id = $1 AND user_id = $2 AND kind = 'summary_artifact' AND status = 'active' FOR UPDATE",
        )
        .bind(summary_id)
        .bind(owner_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(storage_error)?
        .ok_or(AiRepositoryError::NotFound)?;
        let artifact_revision: i64 = row.try_get("artifact_revision").map_err(storage_error)?;
        if u64::try_from(artifact_revision).ok() != Some(request.expected_revision) {
            return Err(AiRepositoryError::Conflict);
        }
        let mut payload: Value = row.try_get("payload").map_err(storage_error)?;
        payload["content"] = Value::String(content.to_owned());
        let new_id = Uuid::now_v7();
        sqlx::query(
            "UPDATE ai_artifacts SET status = 'superseded', object_revision = object_revision + 1, updated_at = now() WHERE artifact_id = $1",
        )
        .bind(summary_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO ai_artifacts (artifact_id, task_id, run_id, user_id, space_id, kind, schema_version, payload, payload_hash, source_material_id, source_revision_id, scope_kind, scope_ref, summary_form, source_refs, status, authored_by, artifact_revision, supersedes_artifact_id) VALUES ($1, $2, $3, $4, $5, 'summary_artifact', $6, $7, $8, $9, $10, $11, $12, $13, $14, 'active', 'user', $15, $16)",
        )
        .bind(new_id)
        .bind(row.try_get::<Uuid, _>("task_id").map_err(storage_error)?)
        .bind(row.try_get::<Uuid, _>("run_id").map_err(storage_error)?)
        .bind(owner_id)
        .bind(row.try_get::<Uuid, _>("space_id").map_err(storage_error)?)
        .bind(SUMMARY_ARTIFACT_SCHEMA_VERSION)
        .bind(&payload)
        .bind(hash_json(&payload)?)
        .bind(row.try_get::<Uuid, _>("source_material_id").map_err(storage_error)?)
        .bind(row.try_get::<Uuid, _>("source_revision_id").map_err(storage_error)?)
        .bind(row.try_get::<String, _>("scope_kind").map_err(storage_error)?)
        .bind(row.try_get::<String, _>("scope_ref").map_err(storage_error)?)
        .bind(row.try_get::<String, _>("summary_form").map_err(storage_error)?)
        .bind(row.try_get::<Value, _>("source_refs").map_err(storage_error)?)
        .bind(artifact_revision.saturating_add(1))
        .bind(summary_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO ai_artifact_mutations (user_id, idempotency_key, artifact_id, action, request_hash, resulting_artifact_id) VALUES ($1, $2, $3, 'edit', $4, $5)",
        )
        .bind(owner_id)
        .bind(&request.idempotency_key)
        .bind(summary_id)
        .bind(request_hash)
        .bind(new_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        self.summary(owner_id, new_id).await
    }

    /// Remove an active or pending summary from the product projection.
    pub async fn delete_summary(
        &self,
        owner_id: UserId,
        summary_id: Uuid,
    ) -> Result<(), AiRepositoryError> {
        let updated = sqlx::query(
            "UPDATE ai_artifacts SET status = 'rejected', object_revision = object_revision + 1, updated_at = now() WHERE artifact_id = $1 AND user_id = $2 AND kind = 'summary_artifact' AND status IN ('active', 'candidate')",
        )
        .bind(summary_id)
        .bind(owner_id)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        if updated.rows_affected() == 1 {
            Ok(())
        } else {
            Err(AiRepositoryError::NotFound)
        }
    }

    /// Recover expired generic AI jobs and reconcile their task/run aggregates.
    ///
    /// # Errors
    ///
    /// Returns an error when recovery or aggregate reconciliation fails.
    pub async fn recover_expired(&self) -> Result<u64, AiRepositoryError> {
        self.jobs.recover_expired(Some("ai")).await?;

        // Reconcile by durable state instead of only the rows returned by the
        // recovery update. This closes the restart window where `jobs` was
        // committed but the process stopped before task/run projections were
        // updated; a later startup still observes and repairs the mismatch.
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "UPDATE ai_runs r SET status = CASE j.status WHEN 'queued' THEN 'released' WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled' END, claim_id = NULL, lease_expires_at = NULL, error_code = CASE WHEN j.status = 'failed' THEN 'retry_exhausted' ELSE r.error_code END, finished_at = COALESCE(r.finished_at, now()) FROM jobs j WHERE r.job_id = j.job_id AND j.kind = 'ai' AND r.status = 'running' AND j.status IN ('queued', 'failed', 'cancelled')",
        )
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let result = sqlx::query(
            "UPDATE ai_tasks t SET status = CASE j.status WHEN 'queued' THEN 'queued' WHEN 'failed' THEN 'failed' WHEN 'cancelled' THEN 'cancelled' END, active_run_id = NULL, object_revision = t.object_revision + 1, updated_at = now() FROM jobs j WHERE t.job_id = j.job_id AND j.kind = 'ai' AND t.status = 'running' AND j.status IN ('queued', 'failed', 'cancelled')",
        )
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        transaction.commit().await.map_err(storage_error)?;
        Ok(result.rows_affected())
    }

    async fn task_by_idempotency(
        &self,
        owner_id: UserId,
        idempotency_key: &str,
    ) -> Result<Option<(AiTask, String)>, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT task.task_id, task.user_id, task.contract_version, task.kind, task.result_kind, task.source_scope, task.parameters, task.prompt_version, task.output_schema_version, task.status, task.dedupe_key, task.object_revision, creation.request_hash FROM ai_task_creations AS creation JOIN ai_tasks AS task ON task.task_id = creation.task_id AND task.user_id = creation.user_id WHERE creation.user_id = $1 AND creation.idempotency_key = $2",
        )
        .bind(owner_id)
        .bind(idempotency_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.map(|row| {
            let hash = row.try_get("request_hash").map_err(storage_error)?;
            Ok((task_from_row(&row)?, hash))
        })
        .transpose()
    }

    async fn bind_creation_idempotency(
        &self,
        owner_id: UserId,
        idempotency_key: &str,
        request_hash: &str,
        task_id: AiTaskId,
    ) -> Result<(), AiRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO ai_task_creations (user_id, idempotency_key, request_hash, task_id) VALUES ($1, $2, $3, $4) ON CONFLICT (user_id, idempotency_key) DO NOTHING",
        )
        .bind(owner_id)
        .bind(idempotency_key)
        .bind(request_hash)
        .bind(task_id)
        .execute(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let row = sqlx::query(
            "SELECT request_hash, task_id FROM ai_task_creations WHERE user_id = $1 AND idempotency_key = $2",
        )
        .bind(owner_id)
        .bind(idempotency_key)
        .fetch_one(&mut *transaction)
        .await
        .map_err(storage_error)?;
        let stored_hash: String = row.try_get("request_hash").map_err(storage_error)?;
        let stored_task_id: Uuid = row.try_get("task_id").map_err(storage_error)?;
        if stored_hash != request_hash || stored_task_id != task_id {
            transaction.rollback().await.map_err(storage_error)?;
            return Err(AiRepositoryError::Conflict);
        }
        transaction.commit().await.map_err(storage_error)
    }

    async fn active_task_by_dedupe(
        &self,
        owner_id: UserId,
        dedupe_key: &str,
    ) -> Result<Option<AiTask>, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT task_id, user_id, contract_version, kind, result_kind, source_scope, parameters, prompt_version, output_schema_version, status, dedupe_key, object_revision FROM ai_tasks WHERE user_id = $1 AND dedupe_key = $2 AND status IN ('queued', 'running', 'needs_input')",
        )
        .bind(owner_id)
        .bind(dedupe_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.as_ref().map(task_from_row).transpose()
    }

    async fn completion_replay(
        &self,
        owner_id: UserId,
        request: &CompleteAiTaskRequest,
    ) -> Result<Option<CompleteAiTaskResult>, AiRepositoryError> {
        let row = sqlx::query(
            "SELECT c.run_id, c.claim_id, c.fence, c.request_hash, c.artifact_id, c.result_schema_version FROM ai_task_completions c JOIN ai_tasks t ON t.task_id = c.task_id WHERE c.task_id = $1 AND c.idempotency_key = $2 AND c.user_id = $3 AND t.user_id = $3",
        )
        .bind(request.task_id)
        .bind(&request.idempotency_key)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        replay_from_row(row.as_ref(), request)
    }
}

type ArtifactSlot = (Option<&'static str>, Option<String>, Option<&'static str>);

fn artifact_slot_from_row(
    row: &sqlx_postgres::PgRow,
    result_kind: &str,
) -> Result<ArtifactSlot, AiRepositoryError> {
    if matches!(
        result_kind,
        "abridgement_artifact" | "question_set_artifact" | "open_answer_evaluation"
    ) {
        return Ok((None, None, None));
    }
    if result_kind != "summary_artifact" {
        return Err(AiRepositoryError::Invalid(
            "unsupported artifact slot".to_owned(),
        ));
    }
    let source_scope: Value = row.try_get("source_scope").map_err(storage_error)?;
    let source_scope: AiSourceScope =
        serde_json::from_value(source_scope).map_err(|_| AiRepositoryError::Storage)?;
    let (scope_kind, scope_ref) = match source_scope {
        AiSourceScope::Chapter { scope_ref, .. } => ("chapter", scope_ref),
        AiSourceScope::Material { .. } => ("material", "material".to_owned()),
        AiSourceScope::Selection { .. } => {
            return Err(AiRepositoryError::Invalid(
                "summary artifact requires chapter or material scope".to_owned(),
            ));
        }
    };
    let parameters: Value = row.try_get("parameters").map_err(storage_error)?;
    let form: SummaryForm = serde_json::from_value(
        parameters
            .get("form")
            .cloned()
            .ok_or_else(|| AiRepositoryError::Invalid("summary form is missing".to_owned()))?,
    )
    .map_err(|_| AiRepositoryError::Invalid("summary form is invalid".to_owned()))?;
    let form = match form {
        SummaryForm::Brief => "brief",
        SummaryForm::Outline => "outline",
    };
    Ok((Some(scope_kind), Some(scope_ref), Some(form)))
}

fn validate_artifact_slot_command(
    command: &CreateAiTaskCommand,
    result_kind: &str,
) -> Result<(), AiRepositoryError> {
    if result_kind == "question_set_artifact" {
        if command.kind != "generate_learning_items"
            || command
                .parameters
                .get("source_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .is_none()
            || command
                .parameters
                .get("item_count")
                .and_then(Value::as_u64)
                .is_none_or(|count| !(1..=32).contains(&count))
        {
            return Err(AiRepositoryError::Invalid(
                "learning generation requires source_id and bounded item_count".to_owned(),
            ));
        }
        return Ok(());
    }
    if result_kind == "open_answer_evaluation" {
        if command.kind != "evaluate_open_answer"
            || command
                .parameters
                .get("session_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .is_none()
            || command
                .parameters
                .get("item_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
                .is_none()
        {
            return Err(AiRepositoryError::Invalid(
                "open-answer evaluation requires session and item identity".to_owned(),
            ));
        }
        return Ok(());
    }
    if result_kind == "abridgement_artifact" {
        if command.kind != "abridgement"
            || !matches!(command.source_scope, AiSourceScope::Material { .. })
            || command
                .parameters
                .get("profile")
                .and_then(Value::as_str)
                .is_none_or(|profile| !matches!(profile, "brief" | "balanced"))
        {
            return Err(AiRepositoryError::Invalid(
                "abridgement requires material scope and a supported profile".to_owned(),
            ));
        }
        return Ok(());
    }
    if command.kind != "summary" {
        return Err(AiRepositoryError::Invalid(
            "summary schema requires summary task kind".to_owned(),
        ));
    }
    if matches!(&command.source_scope, AiSourceScope::Selection { .. }) {
        return Err(AiRepositoryError::Invalid(
            "summary artifact requires chapter or material scope".to_owned(),
        ));
    }
    serde_json::from_value::<SummaryForm>(
        command
            .parameters
            .get("form")
            .cloned()
            .ok_or_else(|| AiRepositoryError::Invalid("summary form is missing".to_owned()))?,
    )
    .map(|_| ())
    .map_err(|_| AiRepositoryError::Invalid("summary form is invalid".to_owned()))
}

fn validate_completion_fence(
    row: &sqlx_postgres::PgRow,
    request: &CompleteAiTaskRequest,
) -> Result<(), AiRepositoryError> {
    let task_status: String = row.try_get("task_status").map_err(storage_error)?;
    let run_status: String = row.try_get("run_status").map_err(storage_error)?;
    let active_run_id: Option<Uuid> = row.try_get("active_run_id").map_err(storage_error)?;
    let claim_id: Option<Uuid> = row.try_get("claim_id").map_err(storage_error)?;
    let fence: i64 = row.try_get("fence").map_err(storage_error)?;
    let task_revision: i64 = row
        .try_get("claimed_task_revision")
        .map_err(storage_error)?;
    let lease_expires_at: Option<OffsetDateTime> =
        row.try_get("lease_expires_at").map_err(storage_error)?;
    if task_status != "running"
        || run_status != "running"
        || active_run_id != Some(request.run_id)
        || claim_id != Some(request.claim_id)
        || u64::try_from(fence).ok() != Some(request.fence)
        || u64::try_from(task_revision).ok() != Some(request.task_revision)
        || lease_expires_at.is_none_or(|lease| lease <= OffsetDateTime::now_utc())
    {
        Err(AiRepositoryError::StaleClaim)
    } else {
        Ok(())
    }
}

async fn completion_replay_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    owner_id: UserId,
    request: &CompleteAiTaskRequest,
) -> Result<Option<CompleteAiTaskResult>, AiRepositoryError> {
    let row = sqlx::query(
        "SELECT c.run_id, c.claim_id, c.fence, c.request_hash, c.artifact_id, c.result_schema_version FROM ai_task_completions c JOIN ai_tasks t ON t.task_id = c.task_id WHERE c.task_id = $1 AND c.idempotency_key = $2 AND c.user_id = $3 AND t.user_id = $3",
    )
    .bind(request.task_id)
    .bind(&request.idempotency_key)
    .bind(owner_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(storage_error)?;
    replay_from_row(row.as_ref(), request)
}

fn replay_from_row(
    row: Option<&sqlx_postgres::PgRow>,
    request: &CompleteAiTaskRequest,
) -> Result<Option<CompleteAiTaskResult>, AiRepositoryError> {
    let Some(row) = row else {
        return Ok(None);
    };
    let request_hash =
        hash_json(&serde_json::to_value(request).map_err(|_| AiRepositoryError::Storage)?)?;
    let prior_hash: String = row.try_get("request_hash").map_err(storage_error)?;
    let prior_run: Uuid = row.try_get("run_id").map_err(storage_error)?;
    let prior_claim: Uuid = row.try_get("claim_id").map_err(storage_error)?;
    let prior_fence: i64 = row.try_get("fence").map_err(storage_error)?;
    if prior_hash != request_hash
        || prior_run != request.run_id
        || prior_claim != request.claim_id
        || u64::try_from(prior_fence).ok() != Some(request.fence)
    {
        return Err(AiRepositoryError::Conflict);
    }
    Ok(Some(CompleteAiTaskResult {
        task_id: request.task_id,
        run_id: prior_run,
        artifact_id: row.try_get("artifact_id").map_err(storage_error)?,
        result_schema_version: row
            .try_get("result_schema_version")
            .map_err(storage_error)?,
        replayed: true,
    }))
}

fn validate_result_citations(
    pack: &AiContextPack,
    result: &Value,
) -> Result<(), AiRepositoryError> {
    let citations = result
        .get("citation_ids")
        .and_then(Value::as_array)
        .ok_or_else(|| AiRepositoryError::Invalid("result citations are missing".to_owned()))?;
    let issued = pack
        .fragments
        .iter()
        .map(|fragment| fragment.citation_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut returned = std::collections::HashSet::new();
    if citations.iter().any(|citation| {
        citation.as_str().is_none_or(|citation_id| {
            !issued.contains(citation_id) || !returned.insert(citation_id)
        })
    }) {
        Err(AiRepositoryError::Invalid(
            "result citations must be unique and issued to this run".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn task_from_row(row: &sqlx_postgres::PgRow) -> Result<AiTask, AiRepositoryError> {
    let status: String = row.try_get("status").map_err(storage_error)?;
    let source_scope: Value = row.try_get("source_scope").map_err(storage_error)?;
    let task = AiTask {
        contract_version: row.try_get("contract_version").map_err(storage_error)?,
        id: row.try_get("task_id").map_err(storage_error)?,
        owner_id: row.try_get("user_id").map_err(storage_error)?,
        kind: row.try_get("kind").map_err(storage_error)?,
        result_kind: row.try_get("result_kind").map_err(storage_error)?,
        source_scope: serde_json::from_value(source_scope)
            .map_err(|_| AiRepositoryError::Storage)?,
        parameters: row.try_get("parameters").map_err(storage_error)?,
        prompt_version: row.try_get("prompt_version").map_err(storage_error)?,
        output_schema_version: row
            .try_get("output_schema_version")
            .map_err(storage_error)?,
        status: task_status(&status)?,
        dedupe_key: row.try_get("dedupe_key").map_err(storage_error)?,
        object_revision: positive_u64(row.try_get("object_revision").map_err(storage_error)?)?,
    };
    task.validate().map_err(|_| AiRepositoryError::Storage)?;
    Ok(task)
}

fn run_from_row(row: &sqlx_postgres::PgRow) -> Result<AiRun, AiRepositoryError> {
    let executor_kind: String = row.try_get("executor_kind").map_err(storage_error)?;
    let status: String = row.try_get("status").map_err(storage_error)?;
    let fence: i64 = row.try_get("fence").map_err(storage_error)?;
    let attempt: i32 = row.try_get("attempt").map_err(storage_error)?;
    let usage: Option<Value> = row.try_get("usage").map_err(storage_error)?;
    let lease_expires_at: Option<OffsetDateTime> =
        row.try_get("lease_expires_at").map_err(storage_error)?;
    let finished_at: Option<OffsetDateTime> = row.try_get("finished_at").map_err(storage_error)?;
    Ok(AiRun {
        id: row.try_get("run_id").map_err(storage_error)?,
        task_id: row.try_get("task_id").map_err(storage_error)?,
        executor_kind: match executor_kind.as_str() {
            "internal_provider" => AiExecutorKind::InternalProvider,
            "mcp_agent" => AiExecutorKind::McpAgent,
            _ => return Err(AiRepositoryError::Storage),
        },
        executor_ref: row.try_get("executor_ref").map_err(storage_error)?,
        status: run_status(&status)?,
        claim_id: row.try_get("claim_id").map_err(storage_error)?,
        fence: Some(u64::try_from(fence).map_err(|_| AiRepositoryError::Storage)?),
        lease_expires_at: lease_expires_at.map(|value| value.to_string()),
        attempt: u32::try_from(attempt).map_err(|_| AiRepositoryError::Storage)?,
        prompt_version: row.try_get("prompt_version").map_err(storage_error)?,
        output_schema_version: row
            .try_get("output_schema_version")
            .map_err(storage_error)?,
        context_pack_id: row.try_get("context_pack_id").map_err(storage_error)?,
        provider_kind: row.try_get("provider_kind").map_err(storage_error)?,
        model: row.try_get("model").map_err(storage_error)?,
        usage: usage
            .map(serde_json::from_value::<AiTokenUsage>)
            .transpose()
            .map_err(|_| AiRepositoryError::Storage)?,
        progress: row.try_get("progress").map_err(storage_error)?,
        error_code: row.try_get("error_code").map_err(storage_error)?,
        started_at: timestamp_ms(row.try_get("started_at").map_err(storage_error)?),
        finished_at: finished_at.map(timestamp_ms),
    })
}

fn artifact_from_row(row: &sqlx_postgres::PgRow) -> Result<AiArtifact, AiRepositoryError> {
    let status: String = row.try_get("status").map_err(storage_error)?;
    Ok(AiArtifact {
        id: row.try_get("artifact_id").map_err(storage_error)?,
        task_id: row.try_get("task_id").map_err(storage_error)?,
        run_id: row.try_get("run_id").map_err(storage_error)?,
        kind: row.try_get("kind").map_err(storage_error)?,
        schema_version: row.try_get("schema_version").map_err(storage_error)?,
        payload: row.try_get("payload").map_err(storage_error)?,
        status: artifact_status(&status)?,
        object_revision: positive_u64(row.try_get("object_revision").map_err(storage_error)?)?,
        created_at: timestamp_ms(row.try_get("created_at").map_err(storage_error)?),
        updated_at: timestamp_ms(row.try_get("updated_at").map_err(storage_error)?),
    })
}

fn summary_from_row(row: &sqlx_postgres::PgRow) -> Result<SummaryArtifact, AiRepositoryError> {
    let status: String = row.try_get("status").map_err(storage_error)?;
    let scope_kind: String = row.try_get("scope_kind").map_err(storage_error)?;
    let form: String = row.try_get("summary_form").map_err(storage_error)?;
    let authored_by: String = row.try_get("authored_by").map_err(storage_error)?;
    let payload: Value = row.try_get("payload").map_err(storage_error)?;
    let summary = SummaryArtifact {
        schema_version: row.try_get("schema_version").map_err(storage_error)?,
        artifact_id: row.try_get("artifact_id").map_err(storage_error)?,
        task_id: row.try_get("task_id").map_err(storage_error)?,
        run_id: row.try_get("run_id").map_err(storage_error)?,
        kind: row.try_get("kind").map_err(storage_error)?,
        status: artifact_status(&status)?,
        source_revision_id: row.try_get("source_revision_id").map_err(storage_error)?,
        scope_kind: match scope_kind.as_str() {
            "chapter" => SummaryScopeKind::Chapter,
            "material" => SummaryScopeKind::Material,
            _ => return Err(AiRepositoryError::Storage),
        },
        scope_ref: row.try_get("scope_ref").map_err(storage_error)?,
        form: match form.as_str() {
            "brief" => SummaryForm::Brief,
            "outline" => SummaryForm::Outline,
            _ => return Err(AiRepositoryError::Storage),
        },
        content: payload
            .get("content")
            .and_then(Value::as_str)
            .ok_or(AiRepositoryError::Storage)?
            .to_owned(),
        citation_ids: payload
            .get("citation_ids")
            .and_then(Value::as_array)
            .ok_or(AiRepositoryError::Storage)?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or(AiRepositoryError::Storage)
            })
            .collect::<Result<Vec<_>, _>>()?,
        authored_by: match authored_by.as_str() {
            "ai" => AiArtifactAuthor::Ai,
            "user" => AiArtifactAuthor::User,
            _ => return Err(AiRepositoryError::Storage),
        },
        artifact_revision: positive_u64(row.try_get("artifact_revision").map_err(storage_error)?)?,
    };
    summary.validate().map_err(|_| AiRepositoryError::Storage)?;
    Ok(summary)
}

fn task_status(value: &str) -> Result<AiTaskStatus, AiRepositoryError> {
    match value {
        "queued" => Ok(AiTaskStatus::Queued),
        "running" => Ok(AiTaskStatus::Running),
        "needs_input" => Ok(AiTaskStatus::NeedsInput),
        "succeeded" => Ok(AiTaskStatus::Succeeded),
        "failed" => Ok(AiTaskStatus::Failed),
        "cancelled" => Ok(AiTaskStatus::Cancelled),
        _ => Err(AiRepositoryError::Storage),
    }
}

fn run_status(value: &str) -> Result<AiRunStatus, AiRepositoryError> {
    match value {
        "pending" => Ok(AiRunStatus::Pending),
        "running" => Ok(AiRunStatus::Running),
        "succeeded" => Ok(AiRunStatus::Succeeded),
        "failed" => Ok(AiRunStatus::Failed),
        "cancelled" => Ok(AiRunStatus::Cancelled),
        "released" => Ok(AiRunStatus::Released),
        _ => Err(AiRepositoryError::Storage),
    }
}

fn artifact_status(value: &str) -> Result<AiArtifactStatus, AiRepositoryError> {
    match value {
        "candidate" => Ok(AiArtifactStatus::Candidate),
        "active" => Ok(AiArtifactStatus::Active),
        "rejected" => Ok(AiArtifactStatus::Rejected),
        "superseded" => Ok(AiArtifactStatus::Superseded),
        _ => Err(AiRepositoryError::Storage),
    }
}

fn output_kind(schema_version: &str) -> Result<String, AiRepositoryError> {
    if schema_version == lumi_core::SUMMARY_ARTIFACT_SCHEMA_VERSION {
        Ok("summary_artifact".to_owned())
    } else if schema_version == ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION {
        Ok("abridgement_artifact".to_owned())
    } else {
        Err(AiRepositoryError::Invalid(
            "unsupported result schema".to_owned(),
        ))
    }
}

fn hash_json(value: &Value) -> Result<String, AiRepositoryError> {
    serde_json::to_vec(value)
        .map(|encoded| content_hash(&encoded))
        .map_err(|_| AiRepositoryError::Storage)
}

fn positive_u64(value: i64) -> Result<u64, AiRepositoryError> {
    u64::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .ok_or(AiRepositoryError::Storage)
}

fn timestamp_ms(value: OffsetDateTime) -> u64 {
    u64::try_from(value.unix_timestamp_nanos() / 1_000_000).unwrap_or(0)
}

fn storage_error(_error: impl std::fmt::Display) -> AiRepositoryError {
    AiRepositoryError::Storage
}

fn unique_violation(error: &sqlx_core::error::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database| database.code().as_deref() == Some("23505"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumi_core::{
        AiContextFragment, AiPermissionDecision, AiPermissionSnapshot, AiSourceLocator,
        AiSourceScope, SourceCitation, SummaryForm, ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION,
        ABRIDGEMENT_MATERIAL_PROMPT_VERSION, AI_CONTEXT_PACK_SCHEMA_VERSION,
        EXPLICIT_CONTEXT_LIMITS_VERSION, SOURCE_CITATION_SCHEMA_VERSION,
        SUMMARY_ARTIFACT_SCHEMA_VERSION, SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION,
    };

    #[tokio::test]
    async fn postgres_ai_repository_enforces_owner_dedupe_claims_and_atomic_publication(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = AiPgFixture::create().await? else {
            return Ok(());
        };
        let repository = PgAiRepository::new(fixture.pool.clone());
        let command = fixture.command("create-summary");
        let task = repository
            .create_task(fixture.owner_id, command.clone())
            .await?;
        assert_eq!(
            repository
                .create_task(fixture.owner_id, command.clone())
                .await?
                .id,
            task.id
        );
        let mut same_work = command;
        same_work.idempotency_key = "create-summary-second-key".to_owned();
        let replayed_second_key = same_work.clone();
        assert_eq!(
            repository
                .create_task(fixture.owner_id, same_work)
                .await?
                .id,
            task.id
        );
        assert_eq!(
            repository.task(fixture.foreign_owner_id, task.id).await,
            Err(AiRepositoryError::NotFound)
        );

        let pack = fixture.context_pack(task.id);
        repository
            .store_context_pack(fixture.owner_id, &pack)
            .await?;
        assert_eq!(
            repository
                .context_pack(fixture.foreign_owner_id, pack.context_pack_id)
                .await,
            Err(AiRepositoryError::NotFound)
        );
        let cross_owner_pack =
            sqlx::query("UPDATE ai_context_packs SET user_id = $2 WHERE context_pack_id = $1")
                .bind(pack.context_pack_id)
                .bind(fixture.foreign_owner_id)
                .execute(&fixture.pool)
                .await;
        assert!(cross_owner_pack.is_err());

        let first = repository.clone();
        let second = repository.clone();
        let (left, right) = tokio::join!(
            first.claim_task(
                fixture.owner_id,
                task.id,
                AiExecutorKind::McpAgent,
                Some("agent:first")
            ),
            second.claim_task(
                fixture.owner_id,
                task.id,
                AiExecutorKind::InternalProvider,
                Some("worker:second")
            ),
        );
        let claim = match (left, right) {
            (Ok(claim), Err(AiRepositoryError::Conflict))
            | (Err(AiRepositoryError::Conflict), Ok(claim)) => claim,
            outcomes => return Err(format!("unexpected AI competing claims: {outcomes:?}").into()),
        };
        assert_eq!(
            repository.run(fixture.owner_id, claim.run_id).await?.status,
            AiRunStatus::Running
        );
        assert_eq!(
            repository.run(fixture.foreign_owner_id, claim.run_id).await,
            Err(AiRepositoryError::NotFound)
        );
        let result = serde_json::json!({
            "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
            "content": "Проверенное резюме",
            "citation_ids": ["ctx:test:1"],
        });
        let stale = CompleteAiTaskRequest {
            task_id: task.id,
            run_id: claim.run_id,
            claim_id: claim.claim_id,
            fence: claim.fence.saturating_add(1),
            task_revision: claim.task_revision,
            idempotency_key: "complete-summary".to_owned(),
            result: result.clone(),
        };
        assert_eq!(
            repository.complete_task(fixture.owner_id, stale).await,
            Err(AiRepositoryError::StaleClaim)
        );
        let artifact_count: i64 =
            sqlx::query("SELECT count(*) AS total FROM ai_artifacts WHERE task_id = $1")
                .bind(task.id)
                .fetch_one(&fixture.pool)
                .await?
                .try_get("total")?;
        assert_eq!(artifact_count, 0);
        let duplicate_citations = CompleteAiTaskRequest {
            task_id: task.id,
            run_id: claim.run_id,
            claim_id: claim.claim_id,
            fence: claim.fence,
            task_revision: claim.task_revision,
            idempotency_key: "duplicate-citations".to_owned(),
            result: serde_json::json!({
                "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                "content": "Результат с повторной ссылкой",
                "citation_ids": ["ctx:test:1", "ctx:test:1"],
            }),
        };
        assert!(matches!(
            repository
                .complete_task(fixture.owner_id, duplicate_citations)
                .await,
            Err(AiRepositoryError::Invalid(_))
        ));

        let completion = CompleteAiTaskRequest {
            task_id: task.id,
            run_id: claim.run_id,
            claim_id: claim.claim_id,
            fence: claim.fence,
            task_revision: claim.task_revision,
            idempotency_key: "complete-summary".to_owned(),
            result,
        };
        let published = repository
            .complete_task(fixture.owner_id, completion.clone())
            .await?;
        assert!(!published.replayed);
        let replay = repository
            .complete_task(fixture.owner_id, completion.clone())
            .await?;
        assert!(replay.replayed);
        assert_eq!(replay.artifact_id, published.artifact_id);
        assert_eq!(
            repository
                .create_task(fixture.owner_id, replayed_second_key)
                .await?
                .id,
            task.id
        );
        let mut conflicting = completion;
        conflicting.result["content"] = Value::String("Другой результат".to_owned());
        assert_eq!(
            repository
                .complete_task(fixture.owner_id, conflicting)
                .await,
            Err(AiRepositoryError::Conflict)
        );
        let artifact = repository
            .artifact(fixture.owner_id, published.artifact_id)
            .await?;
        assert_eq!(artifact.status, AiArtifactStatus::Candidate);
        let summary_slot: (String, String, String) = sqlx::query(
            "SELECT scope_kind, scope_ref, summary_form FROM ai_artifacts WHERE artifact_id = $1",
        )
        .bind(published.artifact_id)
        .fetch_one(&fixture.pool)
        .await
        .and_then(|row| {
            Ok((
                row.try_get("scope_kind")?,
                row.try_get("scope_ref")?,
                row.try_get("summary_form")?,
            ))
        })?;
        assert_eq!(
            summary_slot,
            (
                "chapter".to_owned(),
                "chapter-1".to_owned(),
                "brief".to_owned()
            )
        );
        let mut competing_command = fixture.command("candidate-conflict");
        competing_command.instruction = "Сделай другой вариант резюме.".to_owned();
        let competing_task = repository
            .create_task(fixture.owner_id, competing_command)
            .await?;
        repository
            .store_context_pack(fixture.owner_id, &fixture.context_pack(competing_task.id))
            .await?;
        let competing_claim = repository
            .claim_task(
                fixture.owner_id,
                competing_task.id,
                AiExecutorKind::McpAgent,
                Some("agent:candidate-conflict"),
            )
            .await?;
        let competing_completion = CompleteAiTaskRequest {
            task_id: competing_task.id,
            run_id: competing_claim.run_id,
            claim_id: competing_claim.claim_id,
            fence: competing_claim.fence,
            task_revision: competing_claim.task_revision,
            idempotency_key: "candidate-conflict-complete".to_owned(),
            result: serde_json::json!({
                "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                "content": "Конфликтующий кандидат",
                "citation_ids": ["ctx:test:1"],
            }),
        };
        assert_eq!(
            repository
                .complete_task(fixture.owner_id, competing_completion)
                .await,
            Err(AiRepositoryError::Conflict)
        );
        let competing_states: (String, String, String, i64) = sqlx::query(
            "SELECT task.status AS task_status, run.status AS run_status, job.status AS job_status, (SELECT count(*) FROM ai_artifacts WHERE task_id = task.task_id) AS artifact_count FROM ai_tasks AS task JOIN ai_runs AS run ON run.run_id = task.active_run_id JOIN jobs AS job ON job.job_id = task.job_id WHERE task.task_id = $1",
        )
        .bind(competing_task.id)
        .fetch_one(&fixture.pool)
        .await
        .and_then(|row| {
            Ok((
                row.try_get("task_status")?,
                row.try_get("run_status")?,
                row.try_get("job_status")?,
                row.try_get("artifact_count")?,
            ))
        })?;
        assert_eq!(
            competing_states,
            (
                "running".to_owned(),
                "running".to_owned(),
                "running".to_owned(),
                0,
            )
        );
        let duplicate_candidate = sqlx::query(
            "INSERT INTO ai_artifacts (artifact_id, task_id, run_id, user_id, space_id, kind, schema_version, payload, payload_hash, source_material_id, source_revision_id, scope_kind, scope_ref, summary_form, source_refs, status, authored_by) SELECT $1, task_id, run_id, user_id, space_id, kind, schema_version, '{\"content\":\"duplicate\"}'::jsonb, repeat('a', 64), source_material_id, source_revision_id, scope_kind, scope_ref, summary_form, source_refs, 'candidate', 'ai' FROM ai_artifacts WHERE artifact_id = $2",
        )
        .bind(Uuid::now_v7())
        .bind(published.artifact_id)
        .execute(&fixture.pool)
        .await;
        assert!(duplicate_candidate.is_err());
        assert_eq!(
            repository
                .artifact(fixture.foreign_owner_id, published.artifact_id)
                .await,
            Err(AiRepositoryError::NotFound)
        );
        let states: (String, String, String) = sqlx::query(
            "SELECT t.status AS task_status, r.status AS run_status, j.status AS job_status FROM ai_tasks t JOIN ai_runs r ON r.run_id = $2 JOIN jobs j ON j.job_id = t.job_id WHERE t.task_id = $1",
        )
        .bind(task.id)
        .bind(claim.run_id)
        .fetch_one(&fixture.pool)
        .await
        .and_then(|row| {
            Ok((
                row.try_get("task_status")?,
                row.try_get("run_status")?,
                row.try_get("job_status")?,
            ))
        })?;
        assert_eq!(
            states,
            (
                "succeeded".to_owned(),
                "succeeded".to_owned(),
                "succeeded".to_owned()
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn postgres_ai_repository_recovers_expired_lease_and_rejects_old_claim(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = AiPgFixture::create().await? else {
            return Ok(());
        };
        let repository = PgAiRepository::new(fixture.pool.clone());
        let task = repository
            .create_task(fixture.owner_id, fixture.command("recovery-summary"))
            .await?;
        repository
            .store_context_pack(fixture.owner_id, &fixture.context_pack(task.id))
            .await?;
        let claim = repository
            .claim_task_with_lease(
                fixture.owner_id,
                task.id,
                AiExecutorKind::McpAgent,
                Some("agent:recovery"),
                Duration::from_secs(30),
            )
            .await?;
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 second' WHERE payload_ref->>'task_id' = $1",
        )
        .bind(task.id.to_string())
        .execute(&fixture.pool)
        .await?;
        sqlx::query(
            "UPDATE ai_runs SET lease_expires_at = now() - interval '1 second' WHERE run_id = $1",
        )
        .bind(claim.run_id)
        .execute(&fixture.pool)
        .await?;
        repository.jobs.recover_expired(Some("ai")).await?;
        let job_status: String = sqlx::query(
            "SELECT j.status FROM jobs j JOIN ai_tasks t ON t.job_id = j.job_id WHERE t.task_id = $1",
        )
        .bind(task.id)
        .fetch_one(&fixture.pool)
        .await?
        .try_get("status")?;
        assert_eq!(job_status, "queued");
        assert_eq!(
            repository.task(fixture.owner_id, task.id).await?.status,
            AiTaskStatus::Running
        );
        assert_eq!(repository.recover_expired().await?, 1);
        assert_eq!(
            repository.task(fixture.owner_id, task.id).await?.status,
            AiTaskStatus::Queued
        );
        let stale_completion = CompleteAiTaskRequest {
            task_id: task.id,
            run_id: claim.run_id,
            claim_id: claim.claim_id,
            fence: claim.fence,
            task_revision: claim.task_revision,
            idempotency_key: "stale-after-recovery".to_owned(),
            result: serde_json::json!({
                "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                "content": "Просроченный результат",
                "citation_ids": ["ctx:test:1"],
            }),
        };
        assert_eq!(
            repository
                .complete_task(fixture.owner_id, stale_completion)
                .await,
            Err(AiRepositoryError::StaleClaim)
        );
        assert_eq!(
            repository
                .claim_task(
                    fixture.owner_id,
                    task.id,
                    AiExecutorKind::InternalProvider,
                    Some("worker:retry-with-reused-pack"),
                )
                .await,
            Err(AiRepositoryError::Conflict)
        );
        repository
            .store_context_pack(fixture.owner_id, &fixture.context_pack(task.id))
            .await?;
        let next = repository
            .claim_task(
                fixture.owner_id,
                task.id,
                AiExecutorKind::InternalProvider,
                Some("worker:retry"),
            )
            .await?;
        assert!(next.fence > claim.fence);
        assert_ne!(next.run_id, claim.run_id);
        Ok(())
    }

    #[tokio::test]
    async fn postgres_e2_summary_queue_preserves_manual_edit_until_candidate_accept(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = AiPgFixture::create().await? else {
            return Ok(());
        };
        let repository = PgAiRepository::new(fixture.pool.clone());
        let task = repository
            .create_task(fixture.owner_id, fixture.command("e2-summary-create"))
            .await?;
        let execute = TaskMutationRequest {
            expected_revision: task.object_revision,
            idempotency_key: "e2-summary-execute".to_owned(),
        };
        let scheduled = repository
            .request_execute(fixture.owner_id, task.id, &execute)
            .await?;
        assert_eq!(scheduled.object_revision, task.object_revision + 1);
        assert_eq!(
            repository
                .request_execute(fixture.owner_id, task.id, &execute)
                .await?
                .object_revision,
            scheduled.object_revision
        );
        assert_eq!(
            repository.next_internal_task().await?,
            Some((fixture.owner_id, task.id))
        );

        repository
            .store_context_pack(fixture.owner_id, &fixture.context_pack(task.id))
            .await?;
        let claim = repository
            .claim_task(
                fixture.owner_id,
                task.id,
                AiExecutorKind::InternalProvider,
                Some("worker:e2"),
            )
            .await?;
        let completion = repository
            .complete_task(
                fixture.owner_id,
                CompleteAiTaskRequest {
                    task_id: task.id,
                    run_id: claim.run_id,
                    claim_id: claim.claim_id,
                    fence: claim.fence,
                    task_revision: claim.task_revision,
                    idempotency_key: "e2-summary-complete".to_owned(),
                    result: serde_json::json!({
                        "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                        "content": "Сгенерированное саммари",
                        "citation_ids": ["ctx:test:1"],
                    }),
                },
            )
            .await?;
        let active = repository
            .activate_generated_summary(fixture.owner_id, completion.artifact_id)
            .await?;
        assert_eq!(active.status, AiArtifactStatus::Active);

        let edit_request = UpdateSummaryRequest {
            content: "Моя ручная версия".to_owned(),
            expected_revision: active.artifact_revision,
            idempotency_key: "e2-summary-edit".to_owned(),
        };
        let edited = repository
            .update_summary(fixture.owner_id, active.artifact_id, &edit_request)
            .await?;
        assert_eq!(edited.authored_by, AiArtifactAuthor::User);
        assert_eq!(
            repository
                .update_summary(fixture.owner_id, active.artifact_id, &edit_request)
                .await?
                .artifact_id,
            edited.artifact_id
        );

        let mut regenerate = fixture.command("e2-summary-regenerate");
        regenerate.instruction = "Создай новую версию саммари.".to_owned();
        let regenerated_task = repository.create_task(fixture.owner_id, regenerate).await?;
        repository
            .store_context_pack(fixture.owner_id, &fixture.context_pack(regenerated_task.id))
            .await?;
        let regenerated_claim = repository
            .claim_task(
                fixture.owner_id,
                regenerated_task.id,
                AiExecutorKind::InternalProvider,
                Some("worker:e2-regenerate"),
            )
            .await?;
        let regenerated = repository
            .complete_task(
                fixture.owner_id,
                CompleteAiTaskRequest {
                    task_id: regenerated_task.id,
                    run_id: regenerated_claim.run_id,
                    claim_id: regenerated_claim.claim_id,
                    fence: regenerated_claim.fence,
                    task_revision: regenerated_claim.task_revision,
                    idempotency_key: "e2-summary-regenerated-complete".to_owned(),
                    result: serde_json::json!({
                        "schema_version": SUMMARY_ARTIFACT_SCHEMA_VERSION,
                        "content": "Новая AI-версия",
                        "citation_ids": ["ctx:test:1"],
                    }),
                },
            )
            .await?;
        let candidate = repository
            .activate_generated_summary(fixture.owner_id, regenerated.artifact_id)
            .await?;
        assert_eq!(candidate.status, AiArtifactStatus::Candidate);
        let summaries = repository
            .list_summaries(fixture.owner_id, fixture.material_id)
            .await?;
        assert!(summaries.iter().any(|summary| {
            summary.artifact_id == edited.artifact_id
                && summary.status == AiArtifactStatus::Active
                && summary.content == "Моя ручная версия"
        }));
        assert!(summaries.iter().any(|summary| {
            summary.artifact_id == candidate.artifact_id
                && summary.status == AiArtifactStatus::Candidate
        }));
        let candidate_artifact = repository
            .artifact(fixture.owner_id, candidate.artifact_id)
            .await?;
        let accepted = repository
            .mutate_artifact(
                fixture.owner_id,
                candidate.artifact_id,
                candidate_artifact.object_revision,
                "e2-summary-accept",
                true,
            )
            .await?;
        assert_eq!(accepted.status, AiArtifactStatus::Active);
        assert_eq!(
            repository
                .mutate_artifact(
                    fixture.owner_id,
                    candidate.artifact_id,
                    candidate_artifact.object_revision,
                    "e2-summary-accept",
                    true,
                )
                .await?
                .id,
            accepted.id
        );
        Ok(())
    }

    #[tokio::test]
    async fn postgres_e4_publishes_generated_lum_once_with_exact_provenance(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some(fixture) = AiPgFixture::create().await? else {
            return Ok(());
        };
        let repository = PgAiRepository::new(fixture.pool.clone());
        let task = repository
            .create_task(
                fixture.owner_id,
                fixture.abridgement_command("e4-abridgement"),
            )
            .await?;
        repository
            .store_context_pack(fixture.owner_id, &fixture.material_context_pack(task.id))
            .await?;
        let claim = repository
            .claim_task(
                fixture.owner_id,
                task.id,
                AiExecutorKind::McpAgent,
                Some("agent:e4"),
            )
            .await?;
        let completion = repository
            .complete_task(
                fixture.owner_id,
                CompleteAiTaskRequest {
                    task_id: task.id,
                    run_id: claim.run_id,
                    claim_id: claim.claim_id,
                    fence: claim.fence,
                    task_revision: claim.task_revision,
                    idempotency_key: "e4-complete".to_owned(),
                    result: serde_json::json!({
                        "schema_version": ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION,
                        "title": "Сокращённый материал",
                        "profile": "balanced",
                        "chapters": [{
                            "title": "Глава 1",
                            "content": "Краткий проверяемый тезис.",
                            "citation_ids": ["ctx:test:1"]
                        }],
                        "citation_ids": ["ctx:test:1"]
                    }),
                },
            )
            .await?;
        let blob_root = std::env::temp_dir().join(format!("lumi-e4-derived-{}", Uuid::now_v7()));
        let imports = crate::imports::ImportService::local(fixture.pool.clone(), blob_root);
        let relation = imports
            .publish_abridgement_artifact(fixture.owner_id, completion.artifact_id)
            .await?;
        let replay = imports
            .publish_abridgement_artifact(fixture.owner_id, completion.artifact_id)
            .await?;

        assert_eq!(replay, relation);
        let row = sqlx::query(
            "SELECT derivation.derived_material_id, derivation.derived_revision_id, artifact.status, source.active_revision_id FROM material_derivations derivation JOIN ai_artifacts artifact ON artifact.artifact_id = derivation.artifact_id JOIN materials source ON source.material_id = derivation.source_material_id WHERE derivation.artifact_id = $1",
        )
        .bind(completion.artifact_id)
        .fetch_one(&fixture.pool)
        .await?;
        let derived_revision_id: Uuid = row.try_get("derived_revision_id")?;
        assert_eq!(row.try_get::<String, _>("status")?, "active");
        assert_eq!(
            row.try_get::<Option<Uuid>, _>("active_revision_id")?,
            Some(fixture.revision_id)
        );
        let document = imports
            .reading_document(fixture.owner_id, derived_revision_id)
            .await?;
        assert_eq!(document.title, "Сокращённый материал");
        assert!(document
            .nodes
            .iter()
            .flat_map(|node| node.children.iter())
            .filter_map(|node| node.text.as_deref())
            .any(|text| text.contains("Краткий проверяемый тезис.")));
        Ok(())
    }

    struct AiPgFixture {
        pool: PgPool,
        owner_id: Uuid,
        foreign_owner_id: Uuid,
        material_id: Uuid,
        revision_id: Uuid,
    }

    impl AiPgFixture {
        async fn create() -> Result<Option<Self>, Box<dyn std::error::Error>> {
            let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
                return Ok(None);
            };
            crate::run_migrations(&database_url).await?;
            let pool = sqlx_postgres::PgPoolOptions::new()
                .max_connections(8)
                .connect(&database_url)
                .await?;
            let owner_id = Uuid::now_v7();
            let foreign_owner_id = Uuid::now_v7();
            let space_id = Uuid::now_v7();
            for owner in [owner_id, foreign_owner_id] {
                sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
                    .bind(owner)
                    .execute(&pool)
                    .await?;
            }
            sqlx::query(
                "INSERT INTO sync_devices (device_id, user_id, name, kind) VALUES ($1, $2, 'AI fixture', 'test')",
            )
            .bind(Uuid::now_v7())
            .bind(owner_id)
            .execute(&pool)
            .await?;
            sqlx::query(
                "INSERT INTO sync_spaces (space_id, owner_user_id, kind) VALUES ($1, $2, 'personal')",
            )
            .bind(space_id)
            .bind(owner_id)
            .execute(&pool)
            .await?;
            sqlx::query(
                "INSERT INTO sync_space_members (space_id, user_id, role) VALUES ($1, $2, 'owner')",
            )
            .bind(space_id)
            .bind(owner_id)
            .execute(&pool)
            .await?;
            let material_id = Uuid::now_v7();
            let revision_id = Uuid::now_v7();
            sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'markdown', 'AI repository', 'active', $4, 'ready')")
                .bind(material_id)
                .bind(space_id)
                .bind(owner_id)
                .bind(serde_json::json!({"format":"markdown","source_name":"ai.md","source_hash":"fixture"}))
                .execute(&pool)
                .await?;
            sqlx::query("INSERT INTO document_revisions (revision_id, material_id, space_id, source_format, source_hash, importer_id, importer_version) VALUES ($1, $2, $3, 'markdown', 'fixture', 'test', '1')")
                .bind(revision_id)
                .bind(material_id)
                .bind(space_id)
                .execute(&pool)
                .await?;
            sqlx::query("UPDATE materials SET active_revision_id = $2 WHERE material_id = $1")
                .bind(material_id)
                .bind(revision_id)
                .execute(&pool)
                .await?;
            Ok(Some(Self {
                pool,
                owner_id,
                foreign_owner_id,
                material_id,
                revision_id,
            }))
        }

        fn command(&self, idempotency_key: &str) -> CreateAiTaskCommand {
            CreateAiTaskCommand {
                kind: "summary".to_owned(),
                source_scope: AiSourceScope::Chapter {
                    material_id: self.material_id,
                    revision_id: self.revision_id,
                    scope_ref: "chapter-1".to_owned(),
                },
                instruction: "Сделай краткое резюме.".to_owned(),
                parameters: serde_json::json!({"form": SummaryForm::Brief}),
                prompt_version: SUMMARY_CHAPTER_BRIEF_PROMPT_VERSION.to_owned(),
                output_schema_version: SUMMARY_ARTIFACT_SCHEMA_VERSION.to_owned(),
                idempotency_key: idempotency_key.to_owned(),
            }
        }

        fn abridgement_command(&self, idempotency_key: &str) -> CreateAiTaskCommand {
            CreateAiTaskCommand {
                kind: "abridgement".to_owned(),
                source_scope: AiSourceScope::Material {
                    material_id: self.material_id,
                    revision_id: self.revision_id,
                },
                instruction: "Сократи материал по главам.".to_owned(),
                parameters: serde_json::json!({"profile": "balanced"}),
                prompt_version: ABRIDGEMENT_MATERIAL_PROMPT_VERSION.to_owned(),
                output_schema_version: ABRIDGEMENT_ARTIFACT_SCHEMA_VERSION.to_owned(),
                idempotency_key: idempotency_key.to_owned(),
            }
        }

        fn context_pack(&self, task_id: AiTaskId) -> AiContextPack {
            let context_pack_id = Uuid::now_v7();
            let locator = AiSourceLocator::Markdown {
                file_path: "chapter-1.md".to_owned(),
                byte_start: 0,
                byte_end: 42,
            };
            AiContextPack {
                schema_version: AI_CONTEXT_PACK_SCHEMA_VERSION.to_owned(),
                context_pack_id,
                owner_id: self.owner_id,
                task_id: Some(task_id),
                message_id: None,
                scope: AiSourceScope::Chapter {
                    material_id: self.material_id,
                    revision_id: self.revision_id,
                    scope_ref: "chapter-1".to_owned(),
                },
                permission_snapshot: AiPermissionSnapshot {
                    actor_id: self.owner_id,
                    decision: AiPermissionDecision::Allowed,
                    policy_version: "personal-owner.v1".to_owned(),
                },
                limits_version: EXPLICIT_CONTEXT_LIMITS_VERSION.to_owned(),
                fragments: vec![AiContextFragment {
                    citation_id: "ctx:test:1".to_owned(),
                    unit_id: "chapter-1".to_owned(),
                    block_id: "paragraph-1".to_owned(),
                    label: Some("Глава 1".to_owned()),
                    text: "Проверяемый фрагмент с кириллицей 📚.".to_owned(),
                    text_hash: "fixture-text-hash".to_owned(),
                    source_locator: locator.clone(),
                }],
                citations: vec![SourceCitation {
                    schema_version: SOURCE_CITATION_SCHEMA_VERSION.to_owned(),
                    citation_id: "ctx:test:1".to_owned(),
                    material_id: self.material_id,
                    revision_id: self.revision_id,
                    unit_id: "chapter-1".to_owned(),
                    block_id: "paragraph-1".to_owned(),
                    source_locator: locator,
                    anchor: None,
                    quote_hash: "fixture-text-hash".to_owned(),
                    fragment_byte_start: 0,
                    fragment_byte_end: 42,
                }],
                pack_hash: format!("fixture-pack-{task_id}-{context_pack_id}"),
            }
        }

        fn material_context_pack(&self, task_id: AiTaskId) -> AiContextPack {
            let mut pack = self.context_pack(task_id);
            pack.scope = AiSourceScope::Material {
                material_id: self.material_id,
                revision_id: self.revision_id,
            };
            pack
        }
    }
}
