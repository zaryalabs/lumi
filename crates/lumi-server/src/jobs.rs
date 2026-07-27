//! Common durable job lifecycle shared by AI and the existing import adapter.
//!
//! Feature aggregates keep their own payload and user-facing state. This
//! module is the single execution contract for atomic claims, finite leases,
//! monotonic fencing, retries, cancellation and restart recovery.

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx_core::{row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

/// Durable lifecycle shared by all background execution kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobRuntimeStatus {
    /// Waiting for an eligible executor.
    Queued,
    /// Held by one live fenced claim.
    Running,
    /// Completed exactly once.
    Succeeded,
    /// Exhausted or failed permanently.
    Failed,
    /// Cancelled before publication.
    Cancelled,
}

impl JobRuntimeStatus {
    fn from_database(value: &str) -> Result<Self, JobRuntimeError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(JobRuntimeError::Storage),
        }
    }
}

/// Feature-neutral descriptor persisted before background execution starts.
#[derive(Clone, Debug, PartialEq)]
pub struct JobDescriptor {
    /// Stable job identifier.
    pub job_id: Uuid,
    /// Account owning the job.
    pub owner_id: Uuid,
    /// Sync space containing the feature aggregate.
    pub space_id: Uuid,
    /// Extensible typed execution kind.
    pub kind: String,
    /// Opaque feature-owned payload reference, never logged by the runtime.
    pub payload_ref: Value,
    /// Initial versioned stage.
    pub stage: String,
    /// Finite attempt budget.
    pub max_attempts: u32,
}

/// Read projection of one technical job.
#[derive(Clone, Debug, PartialEq)]
pub struct JobRecord {
    /// Stable job identifier.
    pub job_id: Uuid,
    /// Account owning the job.
    pub owner_id: Uuid,
    /// Sync space containing the feature aggregate.
    pub space_id: Uuid,
    /// Extensible typed execution kind.
    pub kind: String,
    /// Opaque feature-owned payload reference.
    pub payload_ref: Value,
    /// Current lifecycle.
    pub status: JobRuntimeStatus,
    /// Current versioned stage.
    pub stage: String,
    /// Normalized progress.
    pub progress: f32,
    /// Number of claims already made.
    pub attempt: u32,
    /// Finite attempt budget.
    pub max_attempts: u32,
    /// Whether cooperative cancellation was requested.
    pub cancellation_requested: bool,
    /// Monotonic fence last issued for the job.
    pub fence: u64,
    /// Optimistic object revision.
    pub object_revision: u64,
}

/// Opaque identity proving ownership of one live job lease.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFence {
    /// Claimed job.
    pub job_id: Uuid,
    /// Opaque random claim identifier.
    pub claim_id: Uuid,
    /// Monotonic fencing token.
    pub fence: u64,
}

/// Result of an atomic job claim.
#[derive(Clone, Debug, PartialEq)]
pub struct JobClaim {
    /// Complete post-claim job projection.
    pub job: JobRecord,
    /// Opaque current claim identity.
    pub claim: JobFence,
    /// Database-authoritative finite lease expiry.
    pub lease_expires_at: OffsetDateTime,
}

/// Result of one expired-lease recovery decision.
#[derive(Clone, Debug, PartialEq)]
pub struct RecoveredJob {
    /// Recovered job.
    pub job_id: Uuid,
    /// Owning account.
    pub owner_id: Uuid,
    /// Feature-owned payload reference used only for aggregate reconciliation.
    pub payload_ref: Value,
    /// Attempt count at recovery time.
    pub attempt: u32,
    /// Resulting lifecycle.
    pub status: JobRuntimeStatus,
}

/// Redacted failure accepted by the common runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFailure {
    /// Stable public diagnostic code.
    pub code: String,
    /// Bounded message that must not contain payload or secret content.
    pub message: String,
    /// Whether remaining retry budget may requeue the job.
    pub retryable: bool,
}

/// Failure returned by a common job operation.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum JobRuntimeError {
    /// Descriptor or mutation violates bounded runtime policy.
    #[error("invalid job runtime input: {0}")]
    Invalid(&'static str),
    /// Job is absent or outside the requested owner scope.
    #[error("job was not found")]
    NotFound,
    /// Current lifecycle does not accept the operation.
    #[error("job state conflicts with the operation")]
    Conflict,
    /// Claim id, fence or lease is stale.
    #[error("job claim is stale")]
    StaleClaim,
    /// PostgreSQL is unavailable or returned an invalid projection.
    #[error("job storage is unavailable")]
    Storage,
}

/// Persistence adapter used by [`JobRuntime`].
#[async_trait]
pub trait JobStore: Clone + Send + Sync + 'static {
    /// Insert a queued job.
    async fn enqueue(&self, descriptor: &JobDescriptor) -> Result<JobRecord, JobRuntimeError>;

    /// Atomically claim one queued job.
    async fn claim(
        &self,
        owner_id: Option<Uuid>,
        job_id: Uuid,
        claim_id: Uuid,
        lease: Duration,
    ) -> Result<JobClaim, JobRuntimeError>;

    /// Extend a current lease and return the cancellation flag.
    async fn heartbeat(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError>;

    /// Persist bounded progress while extending the current lease.
    async fn set_progress(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        stage: &str,
        progress: f32,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError>;

    /// Request cancellation, immediately cancelling work that is not running.
    async fn request_cancel(
        &self,
        owner_id: Uuid,
        job_id: Uuid,
    ) -> Result<JobRecord, JobRuntimeError>;

    /// Complete a live fenced claim.
    async fn complete(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError>;

    /// Fail or requeue a live fenced claim according to retry policy.
    async fn fail(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        failure: &JobFailure,
    ) -> Result<JobRecord, JobRuntimeError>;

    /// Release a live fenced claim back to the queue.
    async fn release(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError>;

    /// Recover expired/cancelled/exhausted work idempotently.
    async fn recover_expired(
        &self,
        kind: Option<&str>,
    ) -> Result<Vec<RecoveredJob>, JobRuntimeError>;
}

/// Policy-validating application boundary over a concrete persistence adapter.
#[derive(Clone)]
pub struct JobRuntime<S> {
    store: S,
}

impl<S: JobStore> JobRuntime<S> {
    /// Construct a runtime over `store`.
    #[must_use]
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Return the underlying adapter for transaction-aware feature code.
    #[must_use]
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Enqueue one validated feature-neutral job.
    pub async fn enqueue(&self, descriptor: &JobDescriptor) -> Result<JobRecord, JobRuntimeError> {
        validate_descriptor(descriptor)?;
        self.store.enqueue(descriptor).await
    }

    /// Atomically claim one job, optionally restricted to an account.
    pub async fn claim(
        &self,
        owner_id: Option<Uuid>,
        job_id: Uuid,
        lease: Duration,
    ) -> Result<JobClaim, JobRuntimeError> {
        validate_lease(lease)?;
        self.store
            .claim(owner_id, job_id, Uuid::now_v7(), lease)
            .await
    }

    /// Extend a current lease and observe cooperative cancellation.
    pub async fn heartbeat(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        validate_fence(claim)?;
        validate_lease(lease)?;
        self.store.heartbeat(owner_id, claim, lease).await
    }

    /// Persist bounded progress and extend a current lease.
    pub async fn set_progress(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        stage: &str,
        progress: f32,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        validate_fence(claim)?;
        validate_stage(stage)?;
        if !progress.is_finite() || !(0.0..=1.0).contains(&progress) {
            return Err(JobRuntimeError::Invalid(
                "progress must be finite and within 0..=1",
            ));
        }
        validate_lease(lease)?;
        self.store
            .set_progress(owner_id, claim, stage, progress, lease)
            .await
    }

    /// Request cooperative cancellation for account-owned work.
    pub async fn request_cancel(
        &self,
        owner_id: Uuid,
        job_id: Uuid,
    ) -> Result<JobRecord, JobRuntimeError> {
        self.store.request_cancel(owner_id, job_id).await
    }

    /// Complete a live fenced claim.
    pub async fn complete(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        validate_fence(claim)?;
        self.store.complete(owner_id, claim).await
    }

    /// Fail or requeue a live fenced claim with redacted diagnostics.
    pub async fn fail(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        failure: &JobFailure,
    ) -> Result<JobRecord, JobRuntimeError> {
        validate_fence(claim)?;
        if failure.code.is_empty()
            || failure.code.len() > 128
            || failure.message.len() > 4096
            || failure.code.chars().any(char::is_whitespace)
        {
            return Err(JobRuntimeError::Invalid(
                "failure code/message violates redaction bounds",
            ));
        }
        self.store.fail(owner_id, claim, failure).await
    }

    /// Release a live claim back to the queue or finish requested cancellation.
    pub async fn release(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        validate_fence(claim)?;
        self.store.release(owner_id, claim).await
    }

    /// Recover expired work for all kinds or one typed kind.
    pub async fn recover_expired(
        &self,
        kind: Option<&str>,
    ) -> Result<Vec<RecoveredJob>, JobRuntimeError> {
        if kind.is_some_and(|value| value.is_empty() || value.len() > 64) {
            return Err(JobRuntimeError::Invalid("job kind is invalid"));
        }
        self.store.recover_expired(kind).await
    }
}

fn validate_descriptor(descriptor: &JobDescriptor) -> Result<(), JobRuntimeError> {
    if descriptor.kind.is_empty()
        || descriptor.kind.len() > 64
        || descriptor.stage.is_empty()
        || descriptor.stage.len() > 128
        || !(1..=10).contains(&descriptor.max_attempts)
    {
        return Err(JobRuntimeError::Invalid("job descriptor is outside policy"));
    }
    Ok(())
}

fn validate_stage(stage: &str) -> Result<(), JobRuntimeError> {
    if stage.is_empty() || stage.len() > 128 {
        Err(JobRuntimeError::Invalid("job stage is outside policy"))
    } else {
        Ok(())
    }
}

fn validate_fence(claim: &JobFence) -> Result<(), JobRuntimeError> {
    if claim.fence == 0 {
        Err(JobRuntimeError::Invalid("job fence must be positive"))
    } else {
        Ok(())
    }
}

fn validate_lease(lease: Duration) -> Result<(), JobRuntimeError> {
    if lease < Duration::from_secs(1) || lease > Duration::from_secs(60 * 60) {
        Err(JobRuntimeError::Invalid(
            "job lease must be between one second and one hour",
        ))
    } else {
        Ok(())
    }
}

fn lease_seconds(lease: Duration) -> Result<i64, JobRuntimeError> {
    i64::try_from(lease.as_secs()).map_err(|_| JobRuntimeError::Invalid("job lease is too large"))
}

/// PostgreSQL persistence adapter for the generic `jobs` table.
#[derive(Clone)]
pub struct PgJobRepository {
    pool: PgPool,
}

impl PgJobRepository {
    /// Build an adapter over an existing PostgreSQL pool.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Access the underlying pool for feature-level atomic transactions.
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub(crate) async fn claim_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        owner_id: Uuid,
        job_id: Uuid,
        claim_id: Uuid,
        lease: Duration,
    ) -> Result<JobClaim, JobRuntimeError> {
        validate_lease(lease)?;
        let row = sqlx::query(
            "UPDATE jobs SET status = 'running', progress = 0, error_code = NULL, error_message = NULL, claim_id = $3, fence = fence + 1, lease_expires_at = now() + ($4 * interval '1 second'), attempt = attempt + 1, started_at = now(), finished_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status = 'queued' AND cancellation_requested = false AND attempt < max_attempts RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision, claim_id, lease_expires_at",
        )
        .bind(job_id)
        .bind(owner_id)
        .bind(claim_id)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::Conflict)?;
        claim_from_row(&row)
    }

    pub(crate) async fn heartbeat_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        owner_id: Uuid,
        claim: &JobFence,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        validate_fence(claim)?;
        validate_lease(lease)?;
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() + ($5 * interval '1 second'), updated_at = now() WHERE job_id = $1 AND user_id = $2 AND claim_id = $3 AND fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING cancellation_requested",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .map(|row| {
            row.try_get("cancellation_requested")
                .map_err(|_| JobRuntimeError::Storage)
        })
        .transpose()?
        .ok_or(JobRuntimeError::StaleClaim)
    }

    pub(crate) async fn complete_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        owner_id: Uuid,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        validate_fence(claim)?;
        let row = sqlx::query(
            "UPDATE jobs SET status = 'succeeded', progress = 1, completion_claim_id = $3, completion_fence = $4, claim_id = NULL, lease_expires_at = NULL, finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND claim_id = $3 AND fence = $4 AND status = 'running' AND cancellation_requested = false AND lease_expires_at > now() RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| JobRuntimeError::Storage)?;
        if let Some(row) = row {
            return record_from_row(&row);
        }
        let replay = sqlx::query(
            "SELECT job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision FROM jobs WHERE job_id = $1 AND user_id = $2 AND status = 'succeeded' AND completion_claim_id = $3 AND completion_fence = $4",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::StaleClaim)?;
        record_from_row(&replay)
    }
}

#[async_trait]
impl JobStore for PgJobRepository {
    async fn enqueue(&self, descriptor: &JobDescriptor) -> Result<JobRecord, JobRuntimeError> {
        let row = sqlx::query(
            "INSERT INTO jobs (job_id, user_id, space_id, kind, payload_ref, status, stage, max_attempts) VALUES ($1, $2, $3, $4, $5, 'queued', $6, $7) RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision",
        )
        .bind(descriptor.job_id)
        .bind(descriptor.owner_id)
        .bind(descriptor.space_id)
        .bind(&descriptor.kind)
        .bind(&descriptor.payload_ref)
        .bind(&descriptor.stage)
        .bind(i32::try_from(descriptor.max_attempts).map_err(|_| JobRuntimeError::Invalid("max attempts is too large"))?)
        .fetch_one(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?;
        record_from_row(&row)
    }

    async fn claim(
        &self,
        owner_id: Option<Uuid>,
        job_id: Uuid,
        claim_id: Uuid,
        lease: Duration,
    ) -> Result<JobClaim, JobRuntimeError> {
        let row = sqlx::query(
            "UPDATE jobs SET status = 'running', progress = 0, error_code = NULL, error_message = NULL, claim_id = $3, fence = fence + 1, lease_expires_at = now() + ($4 * interval '1 second'), attempt = attempt + 1, started_at = now(), finished_at = NULL, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND status = 'queued' AND cancellation_requested = false AND attempt < max_attempts RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision, claim_id, lease_expires_at",
        )
        .bind(job_id)
        .bind(owner_id)
        .bind(claim_id)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::Conflict)?;
        claim_from_row(&row)
    }

    async fn heartbeat(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() + ($5 * interval '1 second'), updated_at = now() WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND claim_id = $3 AND fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING cancellation_requested",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .map(|row| {
            row.try_get("cancellation_requested")
                .map_err(|_| JobRuntimeError::Storage)
        })
        .transpose()?
        .ok_or(JobRuntimeError::StaleClaim)
    }

    async fn set_progress(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        stage: &str,
        progress: f32,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        sqlx::query(
            "UPDATE jobs SET stage = $5, progress = GREATEST(progress, $6), lease_expires_at = now() + ($7 * interval '1 second'), updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND claim_id = $3 AND fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING cancellation_requested",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .bind(stage)
        .bind(progress)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .map(|row| {
            row.try_get("cancellation_requested")
                .map_err(|_| JobRuntimeError::Storage)
        })
        .transpose()?
        .ok_or(JobRuntimeError::StaleClaim)
    }

    async fn request_cancel(
        &self,
        owner_id: Uuid,
        job_id: Uuid,
    ) -> Result<JobRecord, JobRuntimeError> {
        let row = sqlx::query(
            "UPDATE jobs SET cancellation_requested = true, status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END, claim_id = CASE WHEN status = 'queued' THEN NULL ELSE claim_id END, lease_expires_at = CASE WHEN status = 'queued' THEN NULL ELSE lease_expires_at END, finished_at = CASE WHEN status = 'queued' THEN now() ELSE finished_at END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('queued', 'running') RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision",
        )
        .bind(job_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::Conflict)?;
        record_from_row(&row)
    }

    async fn complete(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        let fence = i64::try_from(claim.fence)
            .map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?;
        let row = sqlx::query(
            "UPDATE jobs SET status = 'succeeded', progress = 1, completion_claim_id = $3, completion_fence = $4, claim_id = NULL, lease_expires_at = NULL, finished_at = now(), updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND claim_id = $3 AND fence = $4 AND status = 'running' AND cancellation_requested = false AND lease_expires_at > now() RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(fence)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?;
        if let Some(row) = row {
            return record_from_row(&row);
        }
        let replay = sqlx::query(
            "SELECT job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision FROM jobs WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND status = 'succeeded' AND completion_claim_id = $3 AND completion_fence = $4",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(fence)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::StaleClaim)?;
        record_from_row(&replay)
    }

    async fn fail(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        failure: &JobFailure,
    ) -> Result<JobRecord, JobRuntimeError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| JobRuntimeError::Storage)?;
        let row = sqlx::query(
            "UPDATE jobs SET status = CASE WHEN $5 AND attempt < max_attempts AND cancellation_requested = false THEN 'queued' WHEN cancellation_requested THEN 'cancelled' ELSE 'failed' END, error_code = $6, error_message = $7, claim_id = NULL, lease_expires_at = NULL, finished_at = CASE WHEN $5 AND attempt < max_attempts AND cancellation_requested = false THEN NULL ELSE now() END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND claim_id = $3 AND fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .bind(failure.retryable)
        .bind(&failure.code)
        .bind(&failure.message)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::StaleClaim)?;
        let record = record_from_row(&row)?;
        sqlx::query(
            "INSERT INTO job_diagnostics (job_id, attempt, code, message) VALUES ($1, $2, $3, $4)",
        )
        .bind(record.job_id)
        .bind(i32::try_from(record.attempt).map_err(|_| JobRuntimeError::Storage)?)
        .bind(&failure.code)
        .bind(&failure.message)
        .execute(&mut *transaction)
        .await
        .map_err(|_| JobRuntimeError::Storage)?;
        transaction
            .commit()
            .await
            .map_err(|_| JobRuntimeError::Storage)?;
        Ok(record)
    }

    async fn release(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        terminal_update(
            &self.pool,
            "UPDATE jobs SET status = CASE WHEN cancellation_requested THEN 'cancelled' ELSE 'queued' END, claim_id = NULL, lease_expires_at = NULL, finished_at = CASE WHEN cancellation_requested THEN now() ELSE NULL END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND claim_id = $3 AND fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING job_id, user_id, space_id, kind, payload_ref, status, stage, progress, attempt, max_attempts, cancellation_requested, fence, object_revision",
            owner_id,
            claim,
        )
        .await
    }

    async fn recover_expired(
        &self,
        kind: Option<&str>,
    ) -> Result<Vec<RecoveredJob>, JobRuntimeError> {
        let rows = sqlx::query(
            "WITH candidates AS (SELECT job_id FROM jobs WHERE ($1::text IS NULL OR kind = $1) AND ((status = 'running' AND (cancellation_requested OR lease_expires_at IS NULL OR lease_expires_at <= now())) OR (status = 'queued' AND (cancellation_requested OR attempt >= max_attempts))) FOR UPDATE SKIP LOCKED), recovered AS (UPDATE jobs j SET status = CASE WHEN j.cancellation_requested THEN 'cancelled' WHEN j.attempt >= j.max_attempts THEN 'failed' ELSE 'queued' END, error_code = CASE WHEN NOT j.cancellation_requested AND j.attempt >= j.max_attempts THEN 'retry_exhausted' ELSE j.error_code END, claim_id = NULL, lease_expires_at = NULL, finished_at = CASE WHEN j.cancellation_requested OR j.attempt >= j.max_attempts THEN now() ELSE NULL END, updated_at = now(), object_revision = j.object_revision + 1 FROM candidates c WHERE j.job_id = c.job_id RETURNING j.job_id, j.user_id, j.payload_ref, j.attempt, j.status) SELECT * FROM recovered ORDER BY job_id",
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?;
        rows.iter().map(recovery_from_row).collect()
    }
}

async fn terminal_update(
    pool: &PgPool,
    statement: &str,
    owner_id: Option<Uuid>,
    claim: &JobFence,
) -> Result<JobRecord, JobRuntimeError> {
    let row = sqlx::query(statement)
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(
            i64::try_from(claim.fence)
                .map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?,
        )
        .fetch_optional(pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::StaleClaim)?;
    record_from_row(&row)
}

/// Adapter exposing existing `import_jobs` through the common runtime contract.
#[derive(Clone)]
pub(crate) struct ImportJobRepository {
    pool: PgPool,
}

impl ImportJobRepository {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobStore for ImportJobRepository {
    async fn enqueue(&self, _descriptor: &JobDescriptor) -> Result<JobRecord, JobRuntimeError> {
        Err(JobRuntimeError::Invalid(
            "import enqueue remains owned by ImportService",
        ))
    }

    async fn claim(
        &self,
        owner_id: Option<Uuid>,
        job_id: Uuid,
        claim_id: Uuid,
        lease: Duration,
    ) -> Result<JobClaim, JobRuntimeError> {
        let row = sqlx::query(
            "UPDATE import_jobs AS job SET status = 'running', stage = 'source_accepted', attempt = job.attempt + 1, worker_claim_id = $3, worker_fence = job.worker_fence + 1, lease_expires_at = now() + ($4 * interval '1 second'), started_at = now(), finished_at = NULL, updated_at = now(), object_revision = job.object_revision + 1 FROM materials AS material WHERE job.job_id = $1 AND ($2::uuid IS NULL OR job.user_id = $2) AND job.status = 'queued' AND job.cancellation_requested = false AND job.attempt < job.max_attempts AND material.material_id = job.result_material_id AND material.owner_user_id = job.user_id AND material.space_id = job.space_id AND material.deleted_at IS NULL RETURNING job.job_id, job.user_id, job.space_id, job.source_kind, job.source_ref, job.result_material_id, job.status, job.stage, job.attempt, job.max_attempts, job.cancellation_requested, job.worker_fence, job.object_revision, job.worker_claim_id, job.lease_expires_at",
        )
        .bind(job_id)
        .bind(owner_id)
        .bind(claim_id)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::Conflict)?;
        import_claim_from_row(&row)
    }

    async fn heartbeat(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        sqlx::query(
            "UPDATE import_jobs SET lease_expires_at = now() + ($5 * interval '1 second'), updated_at = now() WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND worker_claim_id = $3 AND worker_fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING cancellation_requested",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .map(|row| {
            row.try_get("cancellation_requested")
                .map_err(|_| JobRuntimeError::Storage)
        })
        .transpose()?
        .ok_or(JobRuntimeError::StaleClaim)
    }

    async fn set_progress(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
        stage: &str,
        _progress: f32,
        lease: Duration,
    ) -> Result<bool, JobRuntimeError> {
        sqlx::query(
            "UPDATE import_jobs SET stage = $5, lease_expires_at = now() + ($6 * interval '1 second'), updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND worker_claim_id = $3 AND worker_fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING cancellation_requested",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .bind(stage)
        .bind(lease_seconds(lease)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .map(|row| {
            row.try_get("cancellation_requested")
                .map_err(|_| JobRuntimeError::Storage)
        })
        .transpose()?
        .ok_or(JobRuntimeError::StaleClaim)
    }

    async fn request_cancel(
        &self,
        owner_id: Uuid,
        job_id: Uuid,
    ) -> Result<JobRecord, JobRuntimeError> {
        let row = sqlx::query(
            "UPDATE import_jobs SET cancellation_requested = true, status = CASE WHEN status = 'queued' THEN 'cancelled' ELSE status END, worker_claim_id = CASE WHEN status = 'queued' THEN NULL ELSE worker_claim_id END, lease_expires_at = CASE WHEN status = 'queued' THEN NULL ELSE lease_expires_at END, finished_at = CASE WHEN status = 'queued' THEN now() ELSE finished_at END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND user_id = $2 AND status IN ('queued', 'running') RETURNING job_id, user_id, space_id, source_kind, source_ref, result_material_id, status, stage, attempt, max_attempts, cancellation_requested, worker_fence, object_revision",
        )
        .bind(job_id)
        .bind(owner_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::Conflict)?;
        import_record_from_row(&row)
    }

    async fn complete(
        &self,
        _owner_id: Option<Uuid>,
        _claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        Err(JobRuntimeError::Invalid(
            "import publication owns its feature transaction",
        ))
    }

    async fn fail(
        &self,
        _owner_id: Option<Uuid>,
        _claim: &JobFence,
        _failure: &JobFailure,
    ) -> Result<JobRecord, JobRuntimeError> {
        Err(JobRuntimeError::Invalid(
            "import failure owns its feature transaction",
        ))
    }

    async fn release(
        &self,
        owner_id: Option<Uuid>,
        claim: &JobFence,
    ) -> Result<JobRecord, JobRuntimeError> {
        let row = sqlx::query(
            "UPDATE import_jobs SET status = CASE WHEN cancellation_requested THEN 'cancelled' ELSE 'queued' END, worker_claim_id = NULL, lease_expires_at = NULL, finished_at = CASE WHEN cancellation_requested THEN now() ELSE NULL END, updated_at = now(), object_revision = object_revision + 1 WHERE job_id = $1 AND ($2::uuid IS NULL OR user_id = $2) AND worker_claim_id = $3 AND worker_fence = $4 AND status = 'running' AND lease_expires_at > now() RETURNING job_id, user_id, space_id, source_kind, source_ref, result_material_id, status, stage, attempt, max_attempts, cancellation_requested, worker_fence, object_revision",
        )
        .bind(claim.job_id)
        .bind(owner_id)
        .bind(claim.claim_id)
        .bind(i64::try_from(claim.fence).map_err(|_| JobRuntimeError::Invalid("job fence is too large"))?)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?
        .ok_or(JobRuntimeError::StaleClaim)?;
        import_record_from_row(&row)
    }

    async fn recover_expired(
        &self,
        kind: Option<&str>,
    ) -> Result<Vec<RecoveredJob>, JobRuntimeError> {
        let rows = sqlx::query(
            "WITH candidates AS (SELECT job_id FROM import_jobs WHERE ($1::text IS NULL OR source_kind = $1) AND ((status = 'running' AND (cancellation_requested OR lease_expires_at IS NULL OR lease_expires_at <= now())) OR (status = 'queued' AND (cancellation_requested OR attempt >= max_attempts))) FOR UPDATE SKIP LOCKED), recovered AS (UPDATE import_jobs j SET status = CASE WHEN j.cancellation_requested THEN 'cancelled' WHEN j.attempt >= j.max_attempts THEN 'failed' ELSE 'queued' END, stage = CASE WHEN NOT j.cancellation_requested AND j.attempt < j.max_attempts THEN 'source_accepted' ELSE j.stage END, error_code = CASE WHEN NOT j.cancellation_requested AND j.attempt >= j.max_attempts THEN 'import_retry_exhausted' ELSE j.error_code END, worker_claim_id = NULL, lease_expires_at = NULL, started_at = CASE WHEN NOT j.cancellation_requested AND j.attempt < j.max_attempts THEN NULL ELSE j.started_at END, finished_at = CASE WHEN j.cancellation_requested OR j.attempt >= j.max_attempts THEN now() ELSE NULL END, updated_at = now(), object_revision = j.object_revision + 1 FROM candidates c WHERE j.job_id = c.job_id RETURNING j.job_id, j.user_id, j.source_ref, j.result_material_id, j.attempt, j.status) SELECT * FROM recovered ORDER BY job_id",
        )
        .bind(kind)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| JobRuntimeError::Storage)?;
        rows.iter()
            .map(|row| {
                let source_ref: Option<Value> = row
                    .try_get("source_ref")
                    .map_err(|_| JobRuntimeError::Storage)?;
                let material_id: Option<Uuid> = row
                    .try_get("result_material_id")
                    .map_err(|_| JobRuntimeError::Storage)?;
                recovered_from_values(
                    row,
                    serde_json::json!({
                        "source_ref": source_ref,
                        "result_material_id": material_id,
                    }),
                )
            })
            .collect()
    }
}

fn record_from_row(row: &sqlx_postgres::PgRow) -> Result<JobRecord, JobRuntimeError> {
    record_from_values(
        row,
        row.try_get("kind").map_err(|_| JobRuntimeError::Storage)?,
        row.try_get("payload_ref")
            .map_err(|_| JobRuntimeError::Storage)?,
        row.try_get("progress")
            .map_err(|_| JobRuntimeError::Storage)?,
        row.try_get("fence").map_err(|_| JobRuntimeError::Storage)?,
    )
}

fn import_record_from_row(row: &sqlx_postgres::PgRow) -> Result<JobRecord, JobRuntimeError> {
    let source_kind: String = row
        .try_get("source_kind")
        .map_err(|_| JobRuntimeError::Storage)?;
    let source_ref: Option<Value> = row
        .try_get("source_ref")
        .map_err(|_| JobRuntimeError::Storage)?;
    let material_id: Option<Uuid> = row
        .try_get("result_material_id")
        .map_err(|_| JobRuntimeError::Storage)?;
    let status: String = row
        .try_get("status")
        .map_err(|_| JobRuntimeError::Storage)?;
    record_from_values(
        row,
        format!("import.{source_kind}"),
        serde_json::json!({
            "source_ref": source_ref,
            "result_material_id": material_id,
        }),
        if status == "succeeded" { 1.0 } else { 0.0 },
        row.try_get("worker_fence")
            .map_err(|_| JobRuntimeError::Storage)?,
    )
}

fn record_from_values(
    row: &sqlx_postgres::PgRow,
    kind: String,
    payload_ref: Value,
    progress: f32,
    fence: i64,
) -> Result<JobRecord, JobRuntimeError> {
    let status: String = row
        .try_get("status")
        .map_err(|_| JobRuntimeError::Storage)?;
    Ok(JobRecord {
        job_id: row
            .try_get("job_id")
            .map_err(|_| JobRuntimeError::Storage)?,
        owner_id: row
            .try_get("user_id")
            .map_err(|_| JobRuntimeError::Storage)?,
        space_id: row
            .try_get("space_id")
            .map_err(|_| JobRuntimeError::Storage)?,
        kind,
        payload_ref,
        status: JobRuntimeStatus::from_database(&status)?,
        stage: row.try_get("stage").map_err(|_| JobRuntimeError::Storage)?,
        progress,
        attempt: positive_u32(
            row.try_get("attempt")
                .map_err(|_| JobRuntimeError::Storage)?,
        )?,
        max_attempts: positive_u32(
            row.try_get("max_attempts")
                .map_err(|_| JobRuntimeError::Storage)?,
        )?,
        cancellation_requested: row
            .try_get("cancellation_requested")
            .map_err(|_| JobRuntimeError::Storage)?,
        fence: nonnegative_u64(fence)?,
        object_revision: positive_u64(
            row.try_get("object_revision")
                .map_err(|_| JobRuntimeError::Storage)?,
        )?,
    })
}

fn claim_from_row(row: &sqlx_postgres::PgRow) -> Result<JobClaim, JobRuntimeError> {
    let job = record_from_row(row)?;
    let claim_id = row
        .try_get("claim_id")
        .map_err(|_| JobRuntimeError::Storage)?;
    Ok(JobClaim {
        claim: JobFence {
            job_id: job.job_id,
            claim_id,
            fence: job.fence,
        },
        lease_expires_at: row
            .try_get("lease_expires_at")
            .map_err(|_| JobRuntimeError::Storage)?,
        job,
    })
}

fn import_claim_from_row(row: &sqlx_postgres::PgRow) -> Result<JobClaim, JobRuntimeError> {
    let job = import_record_from_row(row)?;
    let claim_id = row
        .try_get("worker_claim_id")
        .map_err(|_| JobRuntimeError::Storage)?;
    Ok(JobClaim {
        claim: JobFence {
            job_id: job.job_id,
            claim_id,
            fence: job.fence,
        },
        lease_expires_at: row
            .try_get("lease_expires_at")
            .map_err(|_| JobRuntimeError::Storage)?,
        job,
    })
}

fn recovery_from_row(row: &sqlx_postgres::PgRow) -> Result<RecoveredJob, JobRuntimeError> {
    recovered_from_values(
        row,
        row.try_get("payload_ref")
            .map_err(|_| JobRuntimeError::Storage)?,
    )
}

fn recovered_from_values(
    row: &sqlx_postgres::PgRow,
    payload_ref: Value,
) -> Result<RecoveredJob, JobRuntimeError> {
    let status: String = row
        .try_get("status")
        .map_err(|_| JobRuntimeError::Storage)?;
    Ok(RecoveredJob {
        job_id: row
            .try_get("job_id")
            .map_err(|_| JobRuntimeError::Storage)?,
        owner_id: row
            .try_get("user_id")
            .map_err(|_| JobRuntimeError::Storage)?,
        payload_ref,
        attempt: positive_u32(
            row.try_get("attempt")
                .map_err(|_| JobRuntimeError::Storage)?,
        )?,
        status: JobRuntimeStatus::from_database(&status)?,
    })
}

fn positive_u32(value: i32) -> Result<u32, JobRuntimeError> {
    u32::try_from(value).map_err(|_| JobRuntimeError::Storage)
}

fn nonnegative_u64(value: i64) -> Result<u64, JobRuntimeError> {
    u64::try_from(value).map_err(|_| JobRuntimeError::Storage)
}

fn positive_u64(value: i64) -> Result<u64, JobRuntimeError> {
    let value = nonnegative_u64(value)?;
    if value == 0 {
        Err(JobRuntimeError::Storage)
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_rejects_unbounded_lease_and_progress() {
        assert_eq!(
            validate_lease(Duration::from_secs(0)),
            Err(JobRuntimeError::Invalid(
                "job lease must be between one second and one hour"
            ))
        );
        assert!(validate_lease(Duration::from_secs(60)).is_ok());
        assert!(validate_descriptor(&JobDescriptor {
            job_id: Uuid::now_v7(),
            owner_id: Uuid::now_v7(),
            space_id: Uuid::now_v7(),
            kind: "test.generic".to_owned(),
            payload_ref: serde_json::json!({"task_id": Uuid::now_v7()}),
            stage: "queued".to_owned(),
            max_attempts: 3,
        })
        .is_ok());
    }

    #[tokio::test]
    async fn postgres_job_runtime_claim_fence_cancel_and_recovery_are_atomic(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some((pool, owner_id, space_id)) = postgres_scope().await? else {
            return Ok(());
        };
        let runtime = JobRuntime::new(PgJobRepository::new(pool.clone()));
        let descriptor = JobDescriptor {
            job_id: Uuid::now_v7(),
            owner_id,
            space_id,
            kind: "test.generic".to_owned(),
            payload_ref: serde_json::json!({"task_id": Uuid::now_v7()}),
            stage: "queued".to_owned(),
            max_attempts: 2,
        };
        runtime.enqueue(&descriptor).await?;
        let first = runtime.clone();
        let second = runtime.clone();
        let (left, right) = tokio::join!(
            first.claim(Some(owner_id), descriptor.job_id, Duration::from_secs(30)),
            second.claim(Some(owner_id), descriptor.job_id, Duration::from_secs(30)),
        );
        let claim = match (left, right) {
            (Ok(claim), Err(JobRuntimeError::Conflict))
            | (Err(JobRuntimeError::Conflict), Ok(claim)) => claim,
            outcomes => {
                return Err(format!("unexpected competing claim outcomes: {outcomes:?}").into())
            }
        };
        let stale = JobFence {
            fence: claim.claim.fence.saturating_add(1),
            ..claim.claim.clone()
        };
        assert_eq!(
            runtime
                .heartbeat(Some(owner_id), &stale, Duration::from_secs(30))
                .await,
            Err(JobRuntimeError::StaleClaim)
        );
        let cancelled = runtime.request_cancel(owner_id, descriptor.job_id).await?;
        assert_eq!(cancelled.status, JobRuntimeStatus::Running);
        assert!(
            runtime
                .heartbeat(Some(owner_id), &claim.claim, Duration::from_secs(30))
                .await?
        );
        let released = runtime.release(Some(owner_id), &claim.claim).await?;
        assert_eq!(released.status, JobRuntimeStatus::Cancelled);

        let recovery_job = JobDescriptor {
            job_id: Uuid::now_v7(),
            payload_ref: serde_json::json!({"task_id": Uuid::now_v7()}),
            ..descriptor
        };
        runtime.enqueue(&recovery_job).await?;
        let expired = runtime
            .claim(Some(owner_id), recovery_job.job_id, Duration::from_secs(30))
            .await?;
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 second' WHERE job_id = $1",
        )
        .bind(recovery_job.job_id)
        .execute(&pool)
        .await?;
        let recovered = runtime.recover_expired(Some("test.generic")).await?;
        assert!(recovered.iter().any(|job| {
            job.job_id == recovery_job.job_id && job.status == JobRuntimeStatus::Queued
        }));
        assert_eq!(
            runtime
                .heartbeat(Some(owner_id), &expired.claim, Duration::from_secs(30))
                .await,
            Err(JobRuntimeError::StaleClaim)
        );

        let exhausted = runtime
            .claim(Some(owner_id), recovery_job.job_id, Duration::from_secs(30))
            .await?;
        sqlx::query(
            "UPDATE jobs SET lease_expires_at = now() - interval '1 second' WHERE job_id = $1",
        )
        .bind(recovery_job.job_id)
        .execute(&pool)
        .await?;
        let recovered = runtime.recover_expired(Some("test.generic")).await?;
        assert!(recovered.iter().any(|job| {
            job.job_id == recovery_job.job_id && job.status == JobRuntimeStatus::Failed
        }));
        assert_eq!(
            runtime.complete(Some(owner_id), &exhausted.claim).await,
            Err(JobRuntimeError::StaleClaim)
        );

        let completion_job = JobDescriptor {
            job_id: Uuid::now_v7(),
            payload_ref: serde_json::json!({"task_id": Uuid::now_v7()}),
            ..recovery_job.clone()
        };
        runtime.enqueue(&completion_job).await?;
        let first_completion_claim = runtime
            .claim(
                Some(owner_id),
                completion_job.job_id,
                Duration::from_secs(30),
            )
            .await?;
        runtime
            .set_progress(
                Some(owner_id),
                &first_completion_claim.claim,
                "halfway",
                0.75,
                Duration::from_secs(30),
            )
            .await?;
        runtime
            .release(Some(owner_id), &first_completion_claim.claim)
            .await?;
        let completion_claim = runtime
            .claim(
                Some(owner_id),
                completion_job.job_id,
                Duration::from_secs(30),
            )
            .await?;
        assert_eq!(completion_claim.job.progress, 0.0);
        let completed = runtime
            .complete(Some(owner_id), &completion_claim.claim)
            .await?;
        assert_eq!(completed.status, JobRuntimeStatus::Succeeded);
        assert_eq!(
            runtime
                .complete(Some(owner_id), &completion_claim.claim)
                .await?,
            completed
        );

        let failure_job = JobDescriptor {
            job_id: Uuid::now_v7(),
            payload_ref: serde_json::json!({"task_id": Uuid::now_v7()}),
            ..recovery_job
        };
        runtime.enqueue(&failure_job).await?;
        let first_failure_claim = runtime
            .claim(Some(owner_id), failure_job.job_id, Duration::from_secs(30))
            .await?;
        let retrying = runtime
            .fail(
                Some(owner_id),
                &first_failure_claim.claim,
                &JobFailure {
                    code: "provider_unavailable".to_owned(),
                    message: "Bounded redacted provider failure.".to_owned(),
                    retryable: true,
                },
            )
            .await?;
        assert_eq!(retrying.status, JobRuntimeStatus::Queued);
        let second_failure_claim = runtime
            .claim(Some(owner_id), failure_job.job_id, Duration::from_secs(30))
            .await?;
        let failed = runtime
            .fail(
                Some(owner_id),
                &second_failure_claim.claim,
                &JobFailure {
                    code: "invalid_provider_response".to_owned(),
                    message: "Bounded redacted response failure.".to_owned(),
                    retryable: false,
                },
            )
            .await?;
        assert_eq!(failed.status, JobRuntimeStatus::Failed);
        let diagnostic_count: i64 =
            sqlx::query("SELECT count(*) AS total FROM job_diagnostics WHERE job_id = $1")
                .bind(failure_job.job_id)
                .fetch_one(&pool)
                .await?
                .try_get("total")?;
        assert_eq!(diagnostic_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn postgres_import_adapter_uses_common_claim_and_fencing_contract(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let _recovery_guard = crate::imports::POSTGRES_RECOVERY_TEST_LOCK.lock().await;
        let Some((pool, owner_id, space_id)) = postgres_scope().await? else {
            return Ok(());
        };
        let material_id = Uuid::now_v7();
        sqlx::query("INSERT INTO materials (material_id, space_id, owner_user_id, kind, canonical_title, library_state, source_identity, import_status) VALUES ($1, $2, $3, 'epub', 'Job adapter', 'active', $4, 'queued')")
            .bind(material_id)
            .bind(space_id)
            .bind(owner_id)
            .bind(serde_json::json!({"format":"epub","source_name":"adapter.epub","source_hash":"adapter"}))
            .execute(&pool)
            .await?;
        let job_id = Uuid::now_v7();
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind) VALUES ($1, $2, $3, 'queued', 'source_accepted', $4, $5, $6, 'epub')")
            .bind(job_id)
            .bind(owner_id)
            .bind(space_id)
            .bind(serde_json::json!({"kind":"epub","blob_hash":"adapter","file_name":"adapter.epub","media_type":"application/epub+zip","device_id":Uuid::now_v7()}))
            .bind(material_id)
            .bind(format!("job-adapter-{job_id}"))
            .execute(&pool)
            .await?;
        let runtime = JobRuntime::new(ImportJobRepository::new(pool.clone()));
        let claim = runtime
            .claim(Some(owner_id), job_id, Duration::from_secs(30))
            .await?;
        assert_eq!(claim.job.kind, "import.epub");
        assert_eq!(claim.job.attempt, 1);
        let stale = JobFence {
            fence: claim.claim.fence.saturating_add(1),
            ..claim.claim.clone()
        };
        assert_eq!(
            runtime
                .heartbeat(Some(owner_id), &stale, Duration::from_secs(30))
                .await,
            Err(JobRuntimeError::StaleClaim)
        );
        assert_eq!(
            runtime.release(Some(owner_id), &claim.claim).await?.status,
            JobRuntimeStatus::Queued
        );
        let cancelled_claim = runtime
            .claim(Some(owner_id), job_id, Duration::from_secs(30))
            .await?;
        assert!(
            runtime
                .request_cancel(owner_id, job_id)
                .await?
                .cancellation_requested
        );
        assert!(
            runtime
                .heartbeat(
                    Some(owner_id),
                    &cancelled_claim.claim,
                    Duration::from_secs(30)
                )
                .await?
        );
        assert_eq!(
            runtime
                .release(Some(owner_id), &cancelled_claim.claim)
                .await?
                .status,
            JobRuntimeStatus::Cancelled
        );

        let recovery_job_id = Uuid::now_v7();
        sqlx::query("INSERT INTO import_jobs (job_id, user_id, space_id, status, stage, source_ref, result_material_id, idempotency_key, source_kind, max_attempts) VALUES ($1, $2, $3, 'queued', 'source_accepted', $4, $5, $6, 'epub', 1)")
            .bind(recovery_job_id)
            .bind(owner_id)
            .bind(space_id)
            .bind(serde_json::json!({"kind":"epub","blob_hash":"adapter-recovery","file_name":"adapter-recovery.epub","media_type":"application/epub+zip","device_id":Uuid::now_v7()}))
            .bind(material_id)
            .bind(format!("job-adapter-recovery-{recovery_job_id}"))
            .execute(&pool)
            .await?;
        let recovery_claim = runtime
            .claim(Some(owner_id), recovery_job_id, Duration::from_secs(30))
            .await?;
        sqlx::query(
            "UPDATE import_jobs SET lease_expires_at = now() - interval '1 second' WHERE job_id = $1",
        )
        .bind(recovery_job_id)
        .execute(&pool)
        .await?;
        runtime.recover_expired(Some("epub")).await?;
        let recovery_status: String =
            sqlx::query("SELECT status FROM import_jobs WHERE job_id = $1")
                .bind(recovery_job_id)
                .fetch_one(&pool)
                .await?
                .try_get("status")?;
        assert_eq!(recovery_status, "failed");
        assert_eq!(
            runtime
                .heartbeat(
                    Some(owner_id),
                    &recovery_claim.claim,
                    Duration::from_secs(30)
                )
                .await,
            Err(JobRuntimeError::StaleClaim)
        );
        Ok(())
    }

    async fn postgres_scope() -> Result<Option<(PgPool, Uuid, Uuid)>, Box<dyn std::error::Error>> {
        let Ok(database_url) = std::env::var("LUMI_TEST_DATABASE_URL") else {
            return Ok(None);
        };
        crate::run_migrations(&database_url).await?;
        let pool = sqlx_postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&database_url)
            .await?;
        let owner_id = Uuid::now_v7();
        let space_id = Uuid::now_v7();
        sqlx::query("INSERT INTO accounts (user_id, status) VALUES ($1, 'active')")
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
        Ok(Some((pool, owner_id, space_id)))
    }
}
