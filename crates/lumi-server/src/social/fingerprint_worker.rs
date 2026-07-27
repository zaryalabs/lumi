//! Durable material fingerprint and automatic claim re-evaluation worker.

use std::time::Duration;

use sqlx_core::row::Row;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::jobs::{JobFailure, JobRuntime, JobRuntimeError, JobRuntimeStatus, PgJobRepository};

use super::service::SocialStoreError;
use super::store::{storage, PgSocialStore};

const CLAIM_LEASE: Duration = Duration::from_secs(30 * 60);
const IDLE_DELAY: Duration = Duration::from_millis(500);

struct FingerprintRequest {
    request_id: Uuid,
    job_id: Uuid,
    owner_id: Uuid,
    material_id: Uuid,
}

impl PgSocialStore {
    pub(super) async fn run_fingerprint_worker(self, cancellation: CancellationToken) {
        let jobs = JobRuntime::new(PgJobRepository::new(self.pool.clone()));
        if jobs
            .recover_expired(Some("material_fingerprint"))
            .await
            .is_err()
        {
            tracing::warn!("material fingerprint job recovery failed");
        }
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                result = self.process_next_fingerprint(&jobs) => {
                    match result {
                        Ok(true) => {}
                        Ok(false) => tokio::time::sleep(IDLE_DELAY).await,
                        Err(error) => {
                            tracing::warn!(error = %error, "material fingerprint worker iteration failed");
                            tokio::time::sleep(IDLE_DELAY).await;
                        }
                    }
                }
            }
        }
    }

    async fn process_next_fingerprint(
        &self,
        jobs: &JobRuntime<PgJobRepository>,
    ) -> Result<bool, SocialStoreError> {
        let Some(request) = next_request(self).await? else {
            return Ok(false);
        };
        let claim = match jobs
            .claim(Some(request.owner_id), request.job_id, CLAIM_LEASE)
            .await
        {
            Ok(claim) => claim,
            Err(JobRuntimeError::Conflict | JobRuntimeError::NotFound) => return Ok(true),
            Err(_) => return Err(SocialStoreError::Unavailable),
        };
        sqlx_core::query::query(
            "UPDATE material_fingerprint_requests
                SET status = 'running', updated_at = now()
              WHERE request_id = $1 AND status = 'queued'",
        )
        .bind(request.request_id)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        match self
            .process_fingerprint_request(request.owner_id, request.material_id)
            .await
        {
            Ok(()) => {
                let mut transaction = self.pool.begin().await.map_err(storage)?;
                PgJobRepository::complete_in_transaction(
                    &mut transaction,
                    request.owner_id,
                    &claim.claim,
                )
                .await
                .map_err(storage)?;
                sqlx_core::query::query(
                    "UPDATE material_fingerprint_requests
                        SET status = 'succeeded', updated_at = now(), finished_at = now()
                      WHERE request_id = $1",
                )
                .bind(request.request_id)
                .execute(&mut *transaction)
                .await
                .map_err(storage)?;
                transaction.commit().await.map_err(storage)?;
            }
            Err(error) => {
                let failure = JobFailure {
                    code: "material_fingerprint_failed".to_owned(),
                    message: "material fingerprint generation failed".to_owned(),
                    retryable: true,
                };
                let record = jobs
                    .fail(Some(request.owner_id), &claim.claim, &failure)
                    .await
                    .map_err(storage)?;
                let status = if record.status == JobRuntimeStatus::Queued {
                    "queued"
                } else {
                    "failed"
                };
                sqlx_core::query::query(
                    "UPDATE material_fingerprint_requests
                        SET status = $2, error_code = $3, updated_at = $4,
                            finished_at = CASE WHEN $2 = 'failed' THEN $4 ELSE NULL END
                      WHERE request_id = $1",
                )
                .bind(request.request_id)
                .bind(status)
                .bind(error.code())
                .bind(OffsetDateTime::now_utc())
                .execute(&self.pool)
                .await
                .map_err(storage)?;
            }
        }
        Ok(true)
    }
}

async fn next_request(
    store: &PgSocialStore,
) -> Result<Option<FingerprintRequest>, SocialStoreError> {
    let row = sqlx_core::query::query(
        "SELECT request.request_id, request.job_id, request.owner_user_id,
                request.material_id
           FROM material_fingerprint_requests request
           JOIN jobs job ON job.job_id = request.job_id
          WHERE request.status = 'queued' AND job.status = 'queued'
            AND job.kind = 'material_fingerprint'
            AND job.cancellation_requested = false
          ORDER BY request.created_at, request.request_id
          LIMIT 1",
    )
    .fetch_optional(&store.pool)
    .await
    .map_err(storage)?;
    row.map(|row| {
        Ok(FingerprintRequest {
            request_id: row.try_get("request_id").map_err(storage)?,
            job_id: row.try_get("job_id").map_err(storage)?,
            owner_id: row.try_get("owner_user_id").map_err(storage)?,
            material_id: row.try_get("material_id").map_err(storage)?,
        })
    })
    .transpose()
}
