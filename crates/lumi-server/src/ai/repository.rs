//! PostgreSQL repositories for durable AI tasks, runs, context packs and artifacts.

use std::time::Duration;

use lumi_core::{
    content_hash, AiArtifact, AiArtifactStatus, AiContextPack, AiExecutorKind, AiRun, AiRunStatus,
    AiSourceScope, AiTask, AiTaskClaim, AiTaskId, AiTaskStatus, AiTokenUsage,
    CompleteAiTaskRequest, CompleteAiTaskResult, CreateAiTaskCommand, SummaryForm, UserId,
    AI_CONTRACT_VERSION,
};
use serde_json::Value;
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::jobs::{JobFence, JobRuntime, JobRuntimeError, PgJobRepository};

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
        validate_summary_slot_command(&command)?;
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
        let (scope_kind, scope_ref, summary_form) = summary_slot_from_row(&row, &result_kind)?;
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

fn summary_slot_from_row(
    row: &sqlx_postgres::PgRow,
    result_kind: &str,
) -> Result<(&'static str, String, &'static str), AiRepositoryError> {
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
    Ok((scope_kind, scope_ref, form))
}

fn validate_summary_slot_command(command: &CreateAiTaskCommand) -> Result<(), AiRepositoryError> {
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
        AiSourceScope, SourceCitation, SummaryForm, AI_CONTEXT_PACK_SCHEMA_VERSION,
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
    }
}
