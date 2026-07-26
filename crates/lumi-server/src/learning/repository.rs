//! Durable and in-memory repositories behind one learning application service.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use lumi_core::{
    grade_learning_answer, now_timestamp_ms, ChangeLearningItemStatusCommand,
    CompleteReadingResponse, CompleteReadingScopeCommand, CreateLearningItemCommand,
    CreateLearningSessionCommand, DocumentRevisionId, LearningAiEvaluation, LearningAttempt,
    LearningChallengeCounts, LearningChallengeGroup, LearningFeedback, LearningHintReveal,
    LearningItem, LearningItemId, LearningItemKind, LearningItemOrigin, LearningItemPage,
    LearningItemRevision, LearningItemStatus, LearningOffer, LearningOfferAction,
    LearningReviewRating, LearningSchedule, LearningScheduleState, LearningScopeKind,
    LearningSession, LearningSessionId, LearningSessionItem, LearningSessionKind,
    LearningSessionState, LearningSettings, LearningSource, LearningSourceId,
    LearningSourceScheduleSettings, LearningToday, LearningValidationError, MaterialId,
    OpenAnswerEvaluationPayload, QuestionSetArtifactPayload, ReadingCompletion,
    RecordLearningSourceOpenedCommand, RevealLearningHintCommand, SchedulerReview,
    SnoozeLearningSessionCommand, SubmitLearningAttemptCommand, UpdateLearningItemCommand,
    UpdateLearningOfferCommand, UpdateLearningSettingsCommand, UserId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row};
use sqlx_postgres::{PgPool, Postgres};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::scheduler::FsrsScheduler;

/// Material metadata used to validate a reader-emitted source in memory mode.
#[derive(Clone, Debug)]
pub(crate) struct LearningMaterialContext {
    pub(crate) space_id: Uuid,
    pub(crate) owner_user_id: UserId,
    pub(crate) material_id: MaterialId,
    pub(crate) active_revision_id: DocumentRevisionId,
    pub(crate) source_hash: String,
    pub(crate) title: String,
    pub(crate) content_unit_ids: HashSet<String>,
}

/// Learning repository failures mapped to the shared problem response.
#[derive(Debug, Error)]
pub(crate) enum LearningStoreError {
    #[error("learning object was not found")]
    NotFound,
    #[error("learning command conflicts with current state")]
    Conflict,
    #[error("learning request is invalid: {0}")]
    Invalid(#[from] LearningValidationError),
    #[error("learning request is invalid: {0}")]
    InvalidDetail(String),
    #[error("learning repository is unavailable")]
    Unavailable,
}

#[derive(Clone)]
enum LearningBackend {
    Memory(Arc<Mutex<MemoryLearningData>>),
    Postgres(PgPool),
}

/// Shared learning application service used by Web and future MCP adapters.
#[derive(Clone)]
pub(crate) struct LearningRuntime {
    backend: LearningBackend,
    scheduler: FsrsScheduler,
}

impl LearningRuntime {
    pub(crate) fn memory() -> Self {
        Self {
            backend: LearningBackend::Memory(Arc::new(Mutex::new(MemoryLearningData::default()))),
            scheduler: FsrsScheduler,
        }
    }

    pub(crate) fn postgres(pool: PgPool) -> Self {
        Self {
            backend: LearningBackend::Postgres(pool),
            scheduler: FsrsScheduler,
        }
    }

    pub(crate) async fn complete_reading(
        &self,
        user_id: UserId,
        device_id: Uuid,
        context: Option<LearningMaterialContext>,
        command: &CompleteReadingScopeCommand,
        command_key: &str,
    ) -> Result<CompleteReadingResponse, LearningStoreError> {
        command.validate()?;
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_complete(
                    &mut data,
                    user_id,
                    context.ok_or(LearningStoreError::NotFound)?,
                    command,
                    command_key,
                )
            }
            LearningBackend::Postgres(pool) => {
                pg_complete(pool, user_id, device_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn offer(
        &self,
        user_id: UserId,
        material_id: MaterialId,
        source_id: LearningSourceId,
    ) -> Result<LearningOffer, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let data = lock_memory(data)?;
                memory_offer(&data, user_id, material_id, source_id)
            }
            LearningBackend::Postgres(pool) => {
                pg_offer(pool, user_id, material_id, source_id).await
            }
        }
    }

    pub(crate) async fn source(
        &self,
        user_id: UserId,
        source_id: LearningSourceId,
    ) -> Result<LearningSource, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => lock_memory(data)?
                .sources
                .get(&source_id)
                .filter(|source| source.space_id == user_id)
                .cloned()
                .ok_or(LearningStoreError::NotFound),
            LearningBackend::Postgres(pool) => {
                let mut tx = pool.begin().await.map_err(log_pg)?;
                let source = pg_source(&mut tx, user_id, source_id).await?;
                tx.commit().await.map_err(log_pg)?;
                Ok(source)
            }
        }
    }

    pub(crate) async fn publish_generated_drafts(
        &self,
        user_id: UserId,
        source_id: LearningSourceId,
        task_id: Uuid,
        artifact_id: Uuid,
        payload: &QuestionSetArtifactPayload,
    ) -> Result<Vec<LearningItem>, LearningStoreError> {
        payload
            .validate()
            .map_err(|error| LearningStoreError::InvalidDetail(error.to_string()))?;
        match &self.backend {
            LearningBackend::Memory(_) => Err(LearningStoreError::Unavailable),
            LearningBackend::Postgres(pool) => {
                pg_publish_generated_drafts(pool, user_id, source_id, task_id, artifact_id, payload)
                    .await
            }
        }
    }

    pub(crate) async fn publish_open_answer_evaluation(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
        item_id: LearningItemId,
        task_id: Uuid,
        artifact_id: Uuid,
        payload: &OpenAnswerEvaluationPayload,
    ) -> Result<(), LearningStoreError> {
        payload
            .validate()
            .map_err(|error| LearningStoreError::InvalidDetail(error.to_string()))?;
        let LearningBackend::Postgres(pool) = &self.backend else {
            return Err(LearningStoreError::Unavailable);
        };
        let inserted = query(
            "INSERT INTO learning_ai_evaluations
             (evaluation_id, owner_user_id, session_id, item_id, artifact_id, task_id, payload)
             SELECT $1, $2, $3, $4, $5, $6, $7
             WHERE EXISTS (
                 SELECT 1 FROM learning_sessions session
                 JOIN learning_session_items snapshot
                   ON snapshot.session_id = session.session_id
                  AND snapshot.item_id = $4
                 JOIN ai_artifacts artifact
                   ON artifact.artifact_id = $5
                  AND artifact.task_id = $6
                  AND artifact.user_id = $2
                 WHERE session.session_id = $3 AND session.user_id = $2
             )
             ON CONFLICT (owner_user_id, artifact_id) DO NOTHING",
        )
        .bind(Uuid::now_v7())
        .bind(user_id)
        .bind(session_id)
        .bind(item_id)
        .bind(artifact_id)
        .bind(task_id)
        .bind(serde_json::to_value(payload).map_err(|_| LearningStoreError::Unavailable)?)
        .execute(pool)
        .await
        .map_err(log_pg)?;
        if inserted.rows_affected() == 0 {
            return Err(LearningStoreError::NotFound);
        }
        Ok(())
    }

    pub(crate) async fn list_ai_evaluations(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
    ) -> Result<Vec<LearningAiEvaluation>, LearningStoreError> {
        let LearningBackend::Postgres(pool) = &self.backend else {
            return Ok(Vec::new());
        };
        let rows = query(
            "SELECT evaluation_id, session_id, item_id, task_id, artifact_id, payload, created_at
             FROM learning_ai_evaluations
             WHERE owner_user_id = $1 AND session_id = $2
             ORDER BY created_at, evaluation_id",
        )
        .bind(user_id)
        .bind(session_id)
        .fetch_all(pool)
        .await
        .map_err(log_pg)?;
        rows.into_iter()
            .map(|row| {
                Ok(LearningAiEvaluation {
                    id: row.try_get("evaluation_id").map_err(log_pg)?,
                    session_id: row.try_get("session_id").map_err(log_pg)?,
                    item_id: row.try_get("item_id").map_err(log_pg)?,
                    task_id: row.try_get("task_id").map_err(log_pg)?,
                    artifact_id: row.try_get("artifact_id").map_err(log_pg)?,
                    feedback: serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
                        .map_err(|_| LearningStoreError::Unavailable)?,
                    created_at: time_to_ms(
                        row.try_get::<OffsetDateTime, _>("created_at")
                            .map_err(log_pg)?,
                    ),
                })
            })
            .collect()
    }

    pub(crate) async fn update_offer(
        &self,
        user_id: UserId,
        material_id: MaterialId,
        command: &UpdateLearningOfferCommand,
        command_key: &str,
    ) -> Result<LearningOffer, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_update_offer(&mut data, user_id, material_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_update_offer(pool, user_id, material_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn list_items(
        &self,
        user_id: UserId,
        material_id: Option<MaterialId>,
        source_id: Option<LearningSourceId>,
        status: Option<LearningItemStatus>,
        cursor: Option<LearningItemId>,
        limit: usize,
    ) -> Result<LearningItemPage, LearningStoreError> {
        let bounded_limit = limit.clamp(1, 100);
        match &self.backend {
            LearningBackend::Memory(data) => {
                let data = lock_memory(data)?;
                memory_list_items(
                    &data,
                    user_id,
                    material_id,
                    source_id,
                    status,
                    cursor,
                    bounded_limit,
                )
            }
            LearningBackend::Postgres(pool) => {
                pg_list_items(
                    pool,
                    user_id,
                    material_id,
                    source_id,
                    status,
                    cursor,
                    bounded_limit,
                )
                .await
            }
        }
    }

    pub(crate) async fn create_item(
        &self,
        user_id: UserId,
        device_id: Uuid,
        command: &CreateLearningItemCommand,
        command_key: &str,
    ) -> Result<LearningItem, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_create_item(&mut data, user_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_create_item(pool, user_id, device_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn get_item(
        &self,
        user_id: UserId,
        item_id: LearningItemId,
    ) -> Result<LearningItem, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => lock_memory(data)?
                .items
                .get(&item_id)
                .filter(|record| record.owner_user_id == user_id)
                .map(|record| record.item.clone())
                .ok_or(LearningStoreError::NotFound),
            LearningBackend::Postgres(pool) => pg_get_item(pool, user_id, item_id).await,
        }
    }

    pub(crate) async fn session_item_revision(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
        item_id: LearningItemId,
    ) -> Result<LearningItemRevision, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => lock_memory(data)?
                .sessions
                .get(&session_id)
                .filter(|record| record.session.user_id == user_id)
                .and_then(|record| record.snapshots.get(&item_id))
                .map(|snapshot| snapshot.revision.clone())
                .ok_or(LearningStoreError::NotFound),
            LearningBackend::Postgres(pool) => {
                let snapshot: serde_json::Value = query_scalar(
                    "SELECT item.snapshot
                       FROM learning_sessions session
                       JOIN learning_session_items item
                         ON item.session_id = session.session_id
                      WHERE session.user_id = $1
                        AND session.session_id = $2
                        AND item.item_id = $3",
                )
                .bind(user_id)
                .bind(session_id)
                .bind(item_id)
                .fetch_optional(pool)
                .await
                .map_err(log_pg)?
                .ok_or(LearningStoreError::NotFound)?;
                let snapshot: SessionItemSnapshot = serde_json::from_value(snapshot)
                    .map_err(|_| LearningStoreError::Unavailable)?;
                Ok(snapshot.revision)
            }
        }
    }

    pub(crate) async fn update_item(
        &self,
        user_id: UserId,
        device_id: Uuid,
        item_id: LearningItemId,
        command: &UpdateLearningItemCommand,
        command_key: &str,
    ) -> Result<LearningItem, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_update_item(&mut data, user_id, item_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_update_item(pool, user_id, device_id, item_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn change_item_status(
        &self,
        user_id: UserId,
        device_id: Uuid,
        item_id: LearningItemId,
        status: LearningItemStatus,
        command: ChangeLearningItemStatusCommand,
        command_key: &str,
    ) -> Result<LearningItem, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_change_item_status(&mut data, user_id, item_id, status, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_change_item_status(
                    pool,
                    user_id,
                    device_id,
                    item_id,
                    status,
                    command,
                    command_key,
                )
                .await
            }
        }
    }

    pub(crate) async fn create_session(
        &self,
        user_id: UserId,
        device_id: Uuid,
        command: CreateLearningSessionCommand,
        command_key: &str,
    ) -> Result<LearningSession, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_create_session(&mut data, user_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_create_session(pool, user_id, device_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn get_session(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
    ) -> Result<LearningSession, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let data = lock_memory(data)?;
                memory_get_session(&data, user_id, session_id)
            }
            LearningBackend::Postgres(pool) => pg_get_session(pool, user_id, session_id).await,
        }
    }

    pub(crate) async fn transition_session(
        &self,
        user_id: UserId,
        device_id: Uuid,
        session_id: LearningSessionId,
        transition: SessionTransition,
        command_key: &str,
    ) -> Result<LearningSession, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_transition_session(&mut data, user_id, session_id, transition, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_transition_session(
                    pool,
                    user_id,
                    device_id,
                    session_id,
                    transition,
                    command_key,
                )
                .await
            }
        }
    }

    pub(crate) async fn source_opened(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
        command: RecordLearningSourceOpenedCommand,
        command_key: &str,
    ) -> Result<LearningSession, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_source_opened(&mut data, user_id, session_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_source_opened(pool, user_id, session_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn submit_attempt(
        &self,
        user_id: UserId,
        device_id: Uuid,
        session_id: LearningSessionId,
        item_id: LearningItemId,
        command: &SubmitLearningAttemptCommand,
        command_key: &str,
    ) -> Result<LearningAttempt, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_submit_attempt(
                    &mut data,
                    self.scheduler,
                    user_id,
                    session_id,
                    item_id,
                    command,
                    command_key,
                )
            }
            LearningBackend::Postgres(pool) => {
                pg_submit_attempt(
                    pool,
                    self.scheduler,
                    user_id,
                    device_id,
                    session_id,
                    item_id,
                    command,
                    command_key,
                )
                .await
            }
        }
    }

    pub(crate) async fn reveal_hint(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
        command: RevealLearningHintCommand,
        command_key: &str,
    ) -> Result<LearningHintReveal, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_reveal_hint(&mut data, user_id, session_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_reveal_hint(pool, user_id, session_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn settings(
        &self,
        user_id: UserId,
    ) -> Result<LearningSettings, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => Ok(lock_memory(data)?
                .settings
                .get(&user_id)
                .cloned()
                .unwrap_or_default()),
            LearningBackend::Postgres(pool) => pg_settings(pool, user_id).await,
        }
    }

    pub(crate) async fn update_settings(
        &self,
        user_id: UserId,
        device_id: Uuid,
        command: &UpdateLearningSettingsCommand,
        command_key: &str,
    ) -> Result<LearningSettings, LearningStoreError> {
        command.validate()?;
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_update_settings(&mut data, user_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_update_settings(pool, user_id, device_id, command, command_key).await
            }
        }
    }

    pub(crate) async fn source_schedule_settings(
        &self,
        user_id: UserId,
        source_id: LearningSourceId,
    ) -> Result<LearningSourceScheduleSettings, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let data = lock_memory(data)?;
                if !data.sources.contains_key(&source_id) {
                    return Err(LearningStoreError::NotFound);
                }
                Ok(data
                    .source_schedule_settings
                    .get(&(user_id, source_id))
                    .cloned()
                    .unwrap_or_else(|| default_source_settings(source_id)))
            }
            LearningBackend::Postgres(pool) => {
                pg_source_schedule_settings(pool, user_id, source_id).await
            }
        }
    }

    pub(crate) async fn set_source_paused(
        &self,
        user_id: UserId,
        device_id: Uuid,
        source_id: LearningSourceId,
        paused: bool,
        command_key: &str,
    ) -> Result<LearningSourceScheduleSettings, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_set_source_paused(&mut data, user_id, source_id, paused, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_set_source_paused(pool, user_id, device_id, source_id, paused, command_key).await
            }
        }
    }

    pub(crate) async fn today(
        &self,
        user_id: UserId,
        material_id: Option<MaterialId>,
    ) -> Result<LearningToday, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let data = lock_memory(data)?;
                memory_today(&data, user_id, material_id)
            }
            LearningBackend::Postgres(pool) => pg_today(pool, user_id, material_id).await,
        }
    }

    pub(crate) async fn list_schedules(
        &self,
        user_id: UserId,
        material_id: Option<MaterialId>,
    ) -> Result<Vec<LearningSchedule>, LearningStoreError> {
        match &self.backend {
            LearningBackend::Memory(data) => {
                let data = lock_memory(data)?;
                let mut schedules = data
                    .schedules
                    .iter()
                    .filter(|((owner, _), _)| *owner == user_id)
                    .filter(|((_, item_id), _)| {
                        material_id.is_none_or(|material_id| {
                            data.items
                                .get(item_id)
                                .and_then(|item| data.sources.get(&item.item.source_id))
                                .is_some_and(|source| source.material_id == material_id)
                        })
                    })
                    .map(|(_, schedule)| schedule.clone())
                    .collect::<Vec<_>>();
                schedules.sort_by_key(|schedule| (schedule.due_at, schedule.item_id));
                Ok(schedules)
            }
            LearningBackend::Postgres(pool) => pg_list_schedules(pool, user_id, material_id).await,
        }
    }

    pub(crate) async fn snooze_session(
        &self,
        user_id: UserId,
        session_id: LearningSessionId,
        command: SnoozeLearningSessionCommand,
        command_key: &str,
    ) -> Result<LearningSession, LearningStoreError> {
        if !(1..=30).contains(&command.days) {
            return Err(LearningStoreError::InvalidDetail(
                "snooze days must be between 1 and 30".to_owned(),
            ));
        }
        match &self.backend {
            LearningBackend::Memory(data) => {
                let mut data = lock_memory(data)?;
                memory_snooze_session(&mut data, user_id, session_id, command, command_key)
            }
            LearningBackend::Postgres(pool) => {
                pg_snooze_session(pool, user_id, session_id, command, command_key).await
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum SessionTransition {
    Start,
    Complete,
    Abandon,
}

#[derive(Clone)]
struct ItemRecord {
    owner_user_id: UserId,
    item: LearningItem,
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionItemSnapshot {
    kind: LearningItemKind,
    revision: LearningItemRevision,
}

#[derive(Clone)]
struct SessionRecord {
    session: LearningSession,
    snapshots: HashMap<LearningItemId, SessionItemSnapshot>,
}

#[derive(Clone)]
struct IdempotentValue {
    request_hash: [u8; 32],
    payload: serde_json::Value,
}

#[derive(Default)]
struct MemoryLearningData {
    sources: HashMap<LearningSourceId, LearningSource>,
    completions: HashMap<(UserId, LearningSourceId), ReadingCompletion>,
    disabled_offers: HashSet<(UserId, MaterialId)>,
    items: HashMap<LearningItemId, ItemRecord>,
    sessions: HashMap<LearningSessionId, SessionRecord>,
    source_opened: HashSet<(LearningSessionId, LearningItemId)>,
    hints_revealed: HashMap<(LearningSessionId, LearningItemId), Vec<u16>>,
    settings: HashMap<UserId, LearningSettings>,
    source_schedule_settings: HashMap<(UserId, LearningSourceId), LearningSourceScheduleSettings>,
    schedules: HashMap<(UserId, LearningItemId), LearningSchedule>,
    snoozed_until: HashMap<LearningSessionId, u64>,
    idempotency: HashMap<(UserId, String), IdempotentValue>,
}

fn lock_memory(
    data: &Arc<Mutex<MemoryLearningData>>,
) -> Result<MutexGuard<'_, MemoryLearningData>, LearningStoreError> {
    data.lock().map_err(|_| LearningStoreError::Unavailable)
}

fn request_hash<T: Serialize>(value: &T) -> Result<[u8; 32], LearningStoreError> {
    let bytes = serde_json::to_vec(value).map_err(|_| LearningStoreError::Unavailable)?;
    Ok(Sha256::digest(bytes).into())
}

fn memory_idempotent<T: for<'de> Deserialize<'de>>(
    data: &MemoryLearningData,
    user_id: UserId,
    command_key: &str,
    hash: [u8; 32],
) -> Result<Option<T>, LearningStoreError> {
    let Some(value) = data.idempotency.get(&(user_id, command_key.to_owned())) else {
        return Ok(None);
    };
    if value.request_hash != hash {
        return Err(LearningStoreError::Conflict);
    }
    serde_json::from_value(value.payload.clone())
        .map(Some)
        .map_err(|_| LearningStoreError::Unavailable)
}

fn remember_memory<T: Serialize>(
    data: &mut MemoryLearningData,
    user_id: UserId,
    command_key: &str,
    hash: [u8; 32],
    value: &T,
) -> Result<(), LearningStoreError> {
    let payload = serde_json::to_value(value).map_err(|_| LearningStoreError::Unavailable)?;
    data.idempotency.insert(
        (user_id, command_key.to_owned()),
        IdempotentValue {
            request_hash: hash,
            payload,
        },
    );
    Ok(())
}

fn validate_context(
    context: &LearningMaterialContext,
    user_id: UserId,
    command: &CompleteReadingScopeCommand,
) -> Result<(), LearningStoreError> {
    if context.owner_user_id != user_id
        || context.material_id != command.material_id
        || context.active_revision_id != command.revision_id
    {
        return Err(LearningStoreError::NotFound);
    }
    if command.scope_kind == LearningScopeKind::ContentUnit
        && command
            .content_unit_id
            .as_ref()
            .is_none_or(|id| !context.content_unit_ids.contains(id))
    {
        return Err(LearningStoreError::InvalidDetail(
            "content unit does not belong to the active revision".to_owned(),
        ));
    }
    Ok(())
}

fn memory_complete(
    data: &mut MemoryLearningData,
    user_id: UserId,
    context: LearningMaterialContext,
    command: &CompleteReadingScopeCommand,
    command_key: &str,
) -> Result<CompleteReadingResponse, LearningStoreError> {
    validate_context(&context, user_id, command)?;
    let hash = request_hash(command)?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let scope_key = LearningSource::scope_key(
        command.revision_id,
        command.scope_kind,
        command.content_unit_id.as_deref(),
        command.anchor.as_ref(),
    );
    let source = data
        .sources
        .values()
        .find(|source| source.space_id == context.space_id && source.scope_key == scope_key)
        .cloned()
        .unwrap_or_else(|| {
            let source = LearningSource {
                id: Uuid::now_v7(),
                space_id: context.space_id,
                material_id: context.material_id,
                document_revision_id: context.active_revision_id,
                scope_kind: command.scope_kind,
                scope_key,
                content_unit_id: command.content_unit_id.clone(),
                anchor: command.anchor.clone(),
                source_hash: context.source_hash,
                title: context.title,
                created_at: now_timestamp_ms(),
            };
            data.sources.insert(source.id, source.clone());
            source
        });
    let key = (user_id, source.id);
    let (completion, created) = match data.completions.get(&key) {
        Some(completion) => (completion.clone(), false),
        None => {
            let completion = ReadingCompletion {
                id: Uuid::now_v7(),
                space_id: source.space_id,
                user_id,
                source_id: source.id,
                completion_generation: 1,
                trigger: command.trigger,
                completed_at: now_timestamp_ms(),
                offer_dismissed_at: None,
                offer_presented_at: None,
            };
            data.completions.insert(key, completion.clone());
            (completion, true)
        }
    };
    let result = CompleteReadingResponse {
        offer: offer_from_memory(data, user_id, &source, &completion),
        created,
    };
    if result.offer.should_offer {
        if let Some(stored) = data.completions.get_mut(&key) {
            stored.offer_presented_at = Some(now_timestamp_ms());
        }
    }
    remember_memory(data, user_id, command_key, hash, &result)?;
    Ok(result)
}

fn offer_from_memory(
    data: &MemoryLearningData,
    user_id: UserId,
    source: &LearningSource,
    completion: &ReadingCompletion,
) -> LearningOffer {
    let offers_enabled = !data
        .disabled_offers
        .contains(&(user_id, source.material_id));
    let active_item_count = data
        .items
        .values()
        .filter(|record| {
            record.owner_user_id == user_id
                && record.item.source_id == source.id
                && record.item.status == LearningItemStatus::Active
        })
        .count();
    LearningOffer {
        source: source.clone(),
        completion: completion.clone(),
        active_item_count,
        should_offer: offers_enabled
            && completion.offer_dismissed_at.is_none()
            && completion.offer_presented_at.is_none(),
        offers_enabled,
    }
}

fn memory_offer(
    data: &MemoryLearningData,
    user_id: UserId,
    material_id: MaterialId,
    source_id: LearningSourceId,
) -> Result<LearningOffer, LearningStoreError> {
    let source = data
        .sources
        .get(&source_id)
        .filter(|source| source.material_id == material_id)
        .ok_or(LearningStoreError::NotFound)?;
    let completion = data
        .completions
        .get(&(user_id, source_id))
        .ok_or(LearningStoreError::NotFound)?;
    Ok(offer_from_memory(data, user_id, source, completion))
}

fn memory_update_offer(
    data: &mut MemoryLearningData,
    user_id: UserId,
    material_id: MaterialId,
    command: &UpdateLearningOfferCommand,
    command_key: &str,
) -> Result<LearningOffer, LearningStoreError> {
    let hash = request_hash(command)?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let source_id = data
        .completions
        .values()
        .find(|completion| completion.user_id == user_id && completion.id == command.completion_id)
        .map(|completion| completion.source_id)
        .ok_or(LearningStoreError::NotFound)?;
    let source = data
        .sources
        .get(&source_id)
        .filter(|source| source.material_id == material_id)
        .cloned()
        .ok_or(LearningStoreError::NotFound)?;
    if command.action == LearningOfferAction::DisableMaterialOffers {
        data.disabled_offers.insert((user_id, material_id));
    }
    let completion = data
        .completions
        .get_mut(&(user_id, source_id))
        .ok_or(LearningStoreError::NotFound)?;
    completion
        .offer_dismissed_at
        .get_or_insert_with(now_timestamp_ms);
    let completion = completion.clone();
    let offer = offer_from_memory(data, user_id, &source, &completion);
    remember_memory(data, user_id, command_key, hash, &offer)?;
    Ok(offer)
}

fn item_kind_matches(kind: LearningItemKind, revision: &LearningItemRevision) -> bool {
    use lumi_core::LearningAnswerSpec;
    matches!(
        (kind, &revision.answer_spec),
        (
            LearningItemKind::QuizSingleChoice,
            LearningAnswerSpec::SingleChoice { .. }
        ) | (
            LearningItemKind::QuizMultipleChoice,
            LearningAnswerSpec::MultipleChoice { .. }
        ) | (
            LearningItemKind::QuizTrueFalse,
            LearningAnswerSpec::TrueFalse { .. }
        ) | (
            LearningItemKind::OpenQuestion,
            LearningAnswerSpec::OpenSelfCheck { .. }
        ) | (
            LearningItemKind::Flashcard,
            LearningAnswerSpec::Flashcard { .. }
        ) | (LearningItemKind::Cloze, LearningAnswerSpec::Cloze { .. })
            | (
                LearningItemKind::HintedQuestion,
                LearningAnswerSpec::HintedSelfCheck { .. }
            )
            | (
                LearningItemKind::ExplainBackPrompt,
                LearningAnswerSpec::ExplainBack
            )
            | (
                LearningItemKind::ReflectionPrompt,
                LearningAnswerSpec::Reflection
            )
    )
}

fn memory_create_item(
    data: &mut MemoryLearningData,
    user_id: UserId,
    command: &CreateLearningItemCommand,
    command_key: &str,
) -> Result<LearningItem, LearningStoreError> {
    let hash = request_hash(command)?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let source = data
        .sources
        .get(&command.source_id)
        .cloned()
        .ok_or(LearningStoreError::NotFound)?;
    let now = now_timestamp_ms();
    let item_id = Uuid::now_v7();
    let revision = LearningItemRevision {
        id: Uuid::now_v7(),
        item_id,
        revision: 1,
        prompt: command.prompt.clone(),
        answer_spec: command.answer_spec.clone(),
        explanation: command.explanation.clone(),
        hints: command.hints.clone(),
        source_anchor: command.source_anchor.clone(),
        created_at: now,
    };
    revision.validate(source.document_revision_id)?;
    if !item_kind_matches(command.kind, &revision) {
        return Err(LearningStoreError::InvalidDetail(
            "item kind does not match answer specification".to_owned(),
        ));
    }
    let item = LearningItem {
        id: item_id,
        source_id: source.id,
        kind: command.kind,
        status: command.status,
        origin: LearningItemOrigin::User,
        current_revision: revision,
        object_revision: 1,
        created_at: now,
        updated_at: now,
    };
    data.items.insert(
        item.id,
        ItemRecord {
            owner_user_id: user_id,
            item: item.clone(),
        },
    );
    remember_memory(data, user_id, command_key, hash, &item)?;
    Ok(item)
}

fn memory_list_items(
    data: &MemoryLearningData,
    user_id: UserId,
    material_id: Option<MaterialId>,
    source_id: Option<LearningSourceId>,
    status: Option<LearningItemStatus>,
    cursor: Option<LearningItemId>,
    limit: usize,
) -> Result<LearningItemPage, LearningStoreError> {
    let mut items = data
        .items
        .values()
        .filter(|record| record.owner_user_id == user_id)
        .filter(|record| source_id.is_none_or(|id| record.item.source_id == id))
        .filter(|record| status.is_none_or(|value| record.item.status == value))
        .filter(|record| cursor.is_none_or(|value| record.item.id > value))
        .filter(|record| {
            material_id.is_none_or(|id| {
                data.sources
                    .get(&record.item.source_id)
                    .is_some_and(|source| source.material_id == id)
            })
        })
        .map(|record| record.item.clone())
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.id);
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = has_more
        .then(|| items.last().map(|item| item.id.to_string()))
        .flatten();
    Ok(LearningItemPage { items, next_cursor })
}

fn memory_update_item(
    data: &mut MemoryLearningData,
    user_id: UserId,
    item_id: LearningItemId,
    command: &UpdateLearningItemCommand,
    command_key: &str,
) -> Result<LearningItem, LearningStoreError> {
    let hash = request_hash(command)?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let source_revision_id = {
        let record = data
            .items
            .get(&item_id)
            .filter(|record| record.owner_user_id == user_id)
            .ok_or(LearningStoreError::NotFound)?;
        data.sources
            .get(&record.item.source_id)
            .map(|source| source.document_revision_id)
            .ok_or(LearningStoreError::NotFound)?
    };
    let record = data
        .items
        .get_mut(&item_id)
        .ok_or(LearningStoreError::NotFound)?;
    if record.item.object_revision != command.expected_revision {
        return Err(LearningStoreError::Conflict);
    }
    let now = now_timestamp_ms();
    let revision = LearningItemRevision {
        id: Uuid::now_v7(),
        item_id,
        revision: record.item.current_revision.revision.saturating_add(1),
        prompt: command.prompt.clone(),
        answer_spec: command.answer_spec.clone(),
        explanation: command.explanation.clone(),
        hints: command.hints.clone(),
        source_anchor: command.source_anchor.clone(),
        created_at: now,
    };
    revision.validate(source_revision_id)?;
    if !item_kind_matches(record.item.kind, &revision) {
        return Err(LearningStoreError::InvalidDetail(
            "item kind does not match answer specification".to_owned(),
        ));
    }
    record.item.current_revision = revision;
    record.item.object_revision = record.item.object_revision.saturating_add(1);
    record.item.updated_at = now;
    let item = record.item.clone();
    remember_memory(data, user_id, command_key, hash, &item)?;
    Ok(item)
}

fn memory_change_item_status(
    data: &mut MemoryLearningData,
    user_id: UserId,
    item_id: LearningItemId,
    status: LearningItemStatus,
    command: ChangeLearningItemStatusCommand,
    command_key: &str,
) -> Result<LearningItem, LearningStoreError> {
    let hash = request_hash(&(command, status))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let record = data
        .items
        .get_mut(&item_id)
        .filter(|record| record.owner_user_id == user_id)
        .ok_or(LearningStoreError::NotFound)?;
    if record.item.object_revision != command.expected_revision {
        return Err(LearningStoreError::Conflict);
    }
    record.item.status = status;
    record.item.object_revision = record.item.object_revision.saturating_add(1);
    record.item.updated_at = now_timestamp_ms();
    let item = record.item.clone();
    remember_memory(data, user_id, command_key, hash, &item)?;
    Ok(item)
}

fn public_snapshot(snapshot: &SessionItemSnapshot, position: usize) -> LearningSessionItem {
    LearningSessionItem {
        item_id: snapshot.revision.item_id,
        item_revision_id: snapshot.revision.id,
        position: position as u16,
        kind: snapshot.kind,
        prompt: snapshot.revision.prompt.clone(),
        answer: snapshot.revision.answer_spec.presentation(),
        hints: snapshot.revision.hints.clone(),
        revealed_hint_count: 0,
        source_anchor: snapshot.revision.source_anchor.clone(),
    }
}

fn memory_create_session(
    data: &mut MemoryLearningData,
    user_id: UserId,
    command: CreateLearningSessionCommand,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&command)?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let source = data
        .sources
        .get(&command.source_id)
        .cloned()
        .ok_or(LearningStoreError::NotFound)?;
    let mut selected = data
        .items
        .values()
        .filter(|record| {
            record.owner_user_id == user_id
                && record.item.source_id == command.source_id
                && record.item.status == LearningItemStatus::Active
                && (command.kind != LearningSessionKind::ExplainBack
                    || record.item.kind == LearningItemKind::ExplainBackPrompt)
                && (command.kind != LearningSessionKind::ScheduledReview
                    || data
                        .schedules
                        .get(&(user_id, record.item.id))
                        .is_some_and(|schedule| {
                            schedule.state != LearningScheduleState::Paused
                                && schedule.due_at <= now_timestamp_ms()
                        }))
        })
        .map(|record| record.item.clone())
        .collect::<Vec<_>>();
    selected.sort_by_key(|item| item.id);
    selected.truncate(7);
    if selected.is_empty() {
        return Err(LearningStoreError::InvalidDetail(
            "source has no active learning items".to_owned(),
        ));
    }
    let snapshots = selected
        .iter()
        .map(|item| {
            (
                item.id,
                SessionItemSnapshot {
                    kind: item.kind,
                    revision: item.current_revision.clone(),
                },
            )
        })
        .collect::<HashMap<_, _>>();
    let items = selected
        .iter()
        .enumerate()
        .filter_map(|(position, item)| {
            snapshots
                .get(&item.id)
                .map(|snapshot| public_snapshot(snapshot, position))
        })
        .collect();
    let now = now_timestamp_ms();
    let session = LearningSession {
        id: Uuid::now_v7(),
        user_id,
        source,
        kind: command.kind,
        state: LearningSessionState::Offered,
        items,
        attempts: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    data.sessions.insert(
        session.id,
        SessionRecord {
            session: session.clone(),
            snapshots,
        },
    );
    remember_memory(data, user_id, command_key, hash, &session)?;
    Ok(session)
}

fn memory_get_session(
    data: &MemoryLearningData,
    user_id: UserId,
    session_id: LearningSessionId,
) -> Result<LearningSession, LearningStoreError> {
    let mut session = data
        .sessions
        .get(&session_id)
        .filter(|record| record.session.user_id == user_id)
        .map(|record| record.session.clone())
        .ok_or(LearningStoreError::NotFound)?;
    for item in &mut session.items {
        item.revealed_hint_count = u16::try_from(
            data.hints_revealed
                .get(&(session_id, item.item_id))
                .map_or(0, Vec::len),
        )
        .unwrap_or(u16::MAX);
    }
    Ok(session)
}

fn memory_transition_session(
    data: &mut MemoryLearningData,
    user_id: UserId,
    session_id: LearningSessionId,
    transition: SessionTransition,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&(session_id, transition_key(transition)))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let record = data
        .sessions
        .get_mut(&session_id)
        .filter(|record| record.session.user_id == user_id)
        .ok_or(LearningStoreError::NotFound)?;
    match transition {
        SessionTransition::Start => record.session.start()?,
        SessionTransition::Complete => record.session.complete()?,
        SessionTransition::Abandon => record.session.abandon()?,
    }
    let session = record.session.clone();
    remember_memory(data, user_id, command_key, hash, &session)?;
    Ok(session)
}

fn transition_key(transition: SessionTransition) -> &'static str {
    match transition {
        SessionTransition::Start => "start",
        SessionTransition::Complete => "complete",
        SessionTransition::Abandon => "abandon",
    }
}

fn memory_source_opened(
    data: &mut MemoryLearningData,
    user_id: UserId,
    session_id: LearningSessionId,
    command: RecordLearningSourceOpenedCommand,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&(session_id, command))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let session = data
        .sessions
        .get(&session_id)
        .filter(|record| {
            record.session.user_id == user_id && record.snapshots.contains_key(&command.item_id)
        })
        .map(|record| record.session.clone())
        .ok_or(LearningStoreError::NotFound)?;
    data.source_opened.insert((session_id, command.item_id));
    remember_memory(data, user_id, command_key, hash, &session)?;
    Ok(session)
}

fn memory_reveal_hint(
    data: &mut MemoryLearningData,
    user_id: UserId,
    session_id: LearningSessionId,
    command: RevealLearningHintCommand,
    command_key: &str,
) -> Result<LearningHintReveal, LearningStoreError> {
    let hash = request_hash(&(session_id, command))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let snapshot = data
        .sessions
        .get(&session_id)
        .filter(|record| record.session.user_id == user_id)
        .and_then(|record| record.snapshots.get(&command.item_id))
        .ok_or(LearningStoreError::NotFound)?;
    let revealed = data
        .hints_revealed
        .entry((session_id, command.item_id))
        .or_default();
    let expected = u16::try_from(revealed.len())
        .unwrap_or(u16::MAX)
        .saturating_add(1);
    if command.position != expected {
        return Err(LearningStoreError::Conflict);
    }
    let hint = snapshot
        .revision
        .hints
        .iter()
        .find(|hint| hint.position == command.position)
        .cloned()
        .ok_or(LearningStoreError::NotFound)?;
    revealed.push(command.position);
    let result = LearningHintReveal {
        hint,
        revealed_count: u16::try_from(revealed.len()).unwrap_or(u16::MAX),
    };
    remember_memory(data, user_id, command_key, hash, &result)?;
    Ok(result)
}

fn memory_update_settings(
    data: &mut MemoryLearningData,
    user_id: UserId,
    command: &UpdateLearningSettingsCommand,
    command_key: &str,
) -> Result<LearningSettings, LearningStoreError> {
    let hash = request_hash(command)?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let current = data.settings.get(&user_id).cloned().unwrap_or_default();
    if current.object_revision != command.expected_revision {
        return Err(LearningStoreError::Conflict);
    }
    let settings = LearningSettings {
        scheduling_enabled: command.scheduling_enabled,
        daily_limit: command.daily_limit,
        manual_only: command.manual_only,
        object_revision: current.object_revision.saturating_add(1),
    };
    data.settings.insert(user_id, settings.clone());
    remember_memory(data, user_id, command_key, hash, &settings)?;
    Ok(settings)
}

fn default_source_settings(source_id: LearningSourceId) -> LearningSourceScheduleSettings {
    LearningSourceScheduleSettings {
        source_id,
        scheduling_enabled: true,
        paused_at: None,
        object_revision: 1,
    }
}

fn user_owns_memory_source(
    data: &MemoryLearningData,
    user_id: UserId,
    source_id: LearningSourceId,
) -> bool {
    data.items
        .values()
        .any(|record| record.owner_user_id == user_id && record.item.source_id == source_id)
        || data.completions.contains_key(&(user_id, source_id))
}

fn memory_set_source_paused(
    data: &mut MemoryLearningData,
    user_id: UserId,
    source_id: LearningSourceId,
    paused: bool,
    command_key: &str,
) -> Result<LearningSourceScheduleSettings, LearningStoreError> {
    let hash = request_hash(&(source_id, paused))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    if !user_owns_memory_source(data, user_id, source_id) {
        return Err(LearningStoreError::NotFound);
    }
    let now = now_timestamp_ms();
    let mut settings = data
        .source_schedule_settings
        .get(&(user_id, source_id))
        .cloned()
        .unwrap_or_else(|| default_source_settings(source_id));
    settings.paused_at = paused.then_some(now);
    settings.object_revision = settings.object_revision.saturating_add(1);
    data.source_schedule_settings
        .insert((user_id, source_id), settings.clone());
    for ((owner, item_id), schedule) in &mut data.schedules {
        if *owner != user_id
            || data
                .items
                .get(item_id)
                .is_none_or(|record| record.item.source_id != source_id)
        {
            continue;
        }
        if paused {
            schedule.algorithm_payload["state_before_pause"] =
                serde_json::json!(schedule_state_key(schedule.state));
            schedule.state = LearningScheduleState::Paused;
            schedule.paused_at = Some(now);
        } else if schedule.state == LearningScheduleState::Paused {
            schedule.state = state_before_pause(schedule);
            schedule.paused_at = None;
            // Resume exposes a bounded actionable item now instead of the
            // entire time-shifted backlog.
            schedule.due_at = schedule.due_at.max(now);
        }
        schedule.object_revision = schedule.object_revision.saturating_add(1);
    }
    remember_memory(data, user_id, command_key, hash, &settings)?;
    Ok(settings)
}

fn state_before_pause(schedule: &LearningSchedule) -> LearningScheduleState {
    schedule
        .algorithm_payload
        .get("state_before_pause")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_schedule_state_value)
        .unwrap_or({
            if schedule.repetitions == 0 {
                LearningScheduleState::New
            } else {
                LearningScheduleState::Review
            }
        })
}

fn memory_today(
    data: &MemoryLearningData,
    user_id: UserId,
    material_id: Option<MaterialId>,
) -> Result<LearningToday, LearningStoreError> {
    let settings = data.settings.get(&user_id).cloned().unwrap_or_default();
    let now = now_timestamp_ms();
    let mut due = Vec::new();
    let mut ready = Vec::new();
    let mut drafts = 0_u32;
    for record in data.items.values().filter(|record| {
        record.owner_user_id == user_id
            && data
                .sources
                .get(&record.item.source_id)
                .is_some_and(|source| {
                    material_id.is_none_or(|material| source.material_id == material)
                })
    }) {
        if record.item.status == LearningItemStatus::Draft {
            drafts = drafts.saturating_add(1);
            continue;
        }
        if record.item.status != LearningItemStatus::Active {
            continue;
        }
        let paused = data
            .source_schedule_settings
            .get(&(user_id, record.item.source_id))
            .is_some_and(|source| source.paused_at.is_some() || !source.scheduling_enabled);
        match data.schedules.get(&(user_id, record.item.id)) {
            Some(schedule)
                if !paused
                    && schedule.state != LearningScheduleState::Paused
                    && schedule.due_at <= now =>
            {
                due.push((schedule.due_at, record.item.id, record.item.source_id));
            }
            None if !paused => ready.push((record.item.id, record.item.source_id)),
            _ => {}
        }
    }
    due.sort_by_key(|(due_at, item_id, _)| (*due_at, *item_id));
    ready.sort_by_key(|(item_id, _)| *item_id);
    let counts = LearningChallengeCounts {
        due: u32::try_from(due.len()).unwrap_or(u32::MAX),
        ready: u32::try_from(ready.len()).unwrap_or(u32::MAX),
        drafts,
    };
    let due_items = if settings.scheduling_enabled && !settings.manual_only {
        due.into_iter()
            .take(usize::from(settings.daily_limit))
            .map(|(_, item_id, source_id)| (item_id, source_id))
            .collect()
    } else {
        Vec::new()
    };
    let ready_items = ready
        .into_iter()
        .take(usize::from(settings.daily_limit))
        .collect::<Vec<_>>();
    Ok(LearningToday {
        groups: challenge_groups(data, user_id, &due_items),
        ready_groups: challenge_groups(data, user_id, &ready_items),
        settings,
        counts,
        generated_at: now,
    })
}

fn challenge_groups(
    data: &MemoryLearningData,
    user_id: UserId,
    items: &[(LearningItemId, LearningSourceId)],
) -> Vec<LearningChallengeGroup> {
    let mut groups = Vec::<LearningChallengeGroup>::new();
    for (item_id, source_id) in items {
        let Some(source) = data.sources.get(source_id) else {
            continue;
        };
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.source_id == *source_id)
        {
            group.item_ids.push(*item_id);
            group.estimated_minutes =
                u16::try_from((group.item_ids.len() * 3).div_ceil(2)).unwrap_or(u16::MAX);
        } else {
            groups.push(LearningChallengeGroup {
                source_id: *source_id,
                material_id: source.material_id,
                title: source.title.clone(),
                item_ids: vec![*item_id],
                estimated_minutes: 2,
                paused: data
                    .source_schedule_settings
                    .get(&(user_id, *source_id))
                    .is_some_and(|settings| settings.paused_at.is_some()),
            });
        }
    }
    groups
}

fn memory_snooze_session(
    data: &mut MemoryLearningData,
    user_id: UserId,
    session_id: LearningSessionId,
    command: SnoozeLearningSessionCommand,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&(session_id, command))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let record = data
        .sessions
        .get_mut(&session_id)
        .filter(|record| {
            record.session.user_id == user_id
                && record.session.kind == LearningSessionKind::ScheduledReview
        })
        .ok_or(LearningStoreError::NotFound)?;
    let answered = record
        .session
        .attempts
        .iter()
        .map(|attempt| attempt.item_id)
        .collect::<HashSet<_>>();
    let delta = u64::from(command.days).saturating_mul(86_400_000);
    for item in &record.session.items {
        if answered.contains(&item.item_id) {
            continue;
        }
        if let Some(schedule) = data.schedules.get_mut(&(user_id, item.item_id)) {
            schedule.due_at = now_timestamp_ms().saturating_add(delta);
            schedule.object_revision = schedule.object_revision.saturating_add(1);
        }
    }
    data.snoozed_until
        .insert(session_id, now_timestamp_ms().saturating_add(delta));
    record.session.abandon()?;
    let session = record.session.clone();
    remember_memory(data, user_id, command_key, hash, &session)?;
    Ok(session)
}

fn conservative_rating(
    outcome: lumi_core::LearningAttemptOutcome,
    self_check: Option<lumi_core::SelfCheckRating>,
    source_opened: bool,
    hints_used: &[u16],
) -> LearningReviewRating {
    if outcome == lumi_core::LearningAttemptOutcome::Incorrect
        || self_check == Some(lumi_core::SelfCheckRating::NotRecalled)
    {
        LearningReviewRating::Again
    } else if source_opened
        || !hints_used.is_empty()
        || self_check == Some(lumi_core::SelfCheckRating::Partial)
    {
        LearningReviewRating::Hard
    } else {
        LearningReviewRating::Good
    }
}

fn memory_submit_attempt(
    data: &mut MemoryLearningData,
    scheduler: FsrsScheduler,
    user_id: UserId,
    session_id: LearningSessionId,
    item_id: LearningItemId,
    command: &SubmitLearningAttemptCommand,
    command_key: &str,
) -> Result<LearningAttempt, LearningStoreError> {
    let hash = request_hash(&(session_id, item_id, command))?;
    if let Some(value) = memory_idempotent(data, user_id, command_key, hash)? {
        return Ok(value);
    }
    let source_opened = data.source_opened.contains(&(session_id, item_id));
    let hints_used = data
        .hints_revealed
        .get(&(session_id, item_id))
        .cloned()
        .unwrap_or_default();
    let record = data
        .sessions
        .get_mut(&session_id)
        .filter(|record| record.session.user_id == user_id)
        .ok_or(LearningStoreError::NotFound)?;
    if record.session.state != LearningSessionState::InProgress {
        return Err(LearningStoreError::Conflict);
    }
    if record
        .session
        .attempts
        .iter()
        .any(|attempt| attempt.item_id == item_id)
    {
        return Err(LearningStoreError::Conflict);
    }
    let snapshot = record
        .snapshots
        .get(&item_id)
        .ok_or(LearningStoreError::NotFound)?;
    let feedback = grade_learning_answer(&snapshot.revision, &command.answer, command.self_check)?;
    let attempt = LearningAttempt {
        id: Uuid::now_v7(),
        session_id,
        item_id,
        item_revision_id: snapshot.revision.id,
        answer: command.answer.clone(),
        self_check: command.self_check,
        feedback,
        elapsed_ms: command.elapsed_ms.min(86_400_000),
        source_opened,
        hints_used: hints_used.clone(),
        review_rating: command.review_rating,
        created_at: now_timestamp_ms(),
    };
    record.session.attempts.push(attempt.clone());
    record.session.updated_at = attempt.created_at;
    let rating = command.review_rating.unwrap_or_else(|| {
        conservative_rating(
            attempt.feedback.outcome,
            command.self_check,
            source_opened,
            &hints_used,
        )
    });
    let previous = data.schedules.get(&(user_id, item_id)).cloned();
    let decision = scheduler.review_item(
        item_id,
        SchedulerReview {
            previous,
            rating,
            outcome: attempt.feedback.outcome,
            self_check: command.self_check,
            source_opened,
            hints_used,
            reviewed_at: attempt.created_at,
        },
    )?;
    data.schedules.insert((user_id, item_id), decision.schedule);
    remember_memory(data, user_id, command_key, hash, &attempt)?;
    Ok(attempt)
}

// PostgreSQL operations keep the same domain payloads in JSONB while indexed
// ownership, state and immutable revision columns remain relational.

async fn pg_material_context(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    command: &CompleteReadingScopeCommand,
) -> Result<LearningMaterialContext, LearningStoreError> {
    let row = query(
        "SELECT m.space_id, m.material_id, m.active_revision_id, m.canonical_title, m.title_override, dr.source_hash, np.payload
         FROM materials m
         JOIN document_revisions dr ON dr.revision_id = m.active_revision_id
         LEFT JOIN normalized_packages np ON np.revision_id = dr.revision_id
         WHERE m.owner_user_id = $1 AND m.material_id = $2 AND m.deleted_at IS NULL
         FOR SHARE OF m, dr",
    )
    .bind(user_id)
    .bind(command.material_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let package: Option<serde_json::Value> = row.try_get("payload").map_err(log_pg)?;
    let content_unit_ids = package
        .as_ref()
        .and_then(|payload| payload.get("units"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|unit| unit.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect();
    Ok(LearningMaterialContext {
        space_id: row.try_get("space_id").map_err(log_pg)?,
        owner_user_id: user_id,
        material_id: row.try_get("material_id").map_err(log_pg)?,
        active_revision_id: row.try_get("active_revision_id").map_err(log_pg)?,
        source_hash: row.try_get("source_hash").map_err(log_pg)?,
        title: row
            .try_get::<Option<String>, _>("title_override")
            .map_err(log_pg)?
            .unwrap_or(row.try_get("canonical_title").map_err(log_pg)?),
        content_unit_ids,
    })
}

async fn pg_complete(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    command: &CompleteReadingScopeCommand,
    command_key: &str,
) -> Result<CompleteReadingResponse, LearningStoreError> {
    let hash = request_hash(command)?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) = pg_read_mutation::<CompleteReadingResponse>(
        &mut tx,
        user_id,
        command_key,
        hash,
        "complete_reading",
    )
    .await?
    {
        return Ok(value);
    }
    let context = pg_material_context(&mut tx, user_id, command).await?;
    validate_context(&context, user_id, command)?;
    let scope_key = LearningSource::scope_key(
        command.revision_id,
        command.scope_kind,
        command.content_unit_id.as_deref(),
        command.anchor.as_ref(),
    );
    let source = pg_ensure_source(&mut tx, &context, command, &scope_key).await?;
    let existing = query(
        "SELECT payload FROM reading_completions
         WHERE user_id = $1 AND source_id = $2 AND completion_generation = 1",
    )
    .bind(user_id)
    .bind(source.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?;
    let (completion, created) = if let Some(row) = existing {
        (
            serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
                .map_err(|_| LearningStoreError::Unavailable)?,
            false,
        )
    } else {
        let completion = ReadingCompletion {
            id: Uuid::now_v7(),
            space_id: source.space_id,
            user_id,
            source_id: source.id,
            completion_generation: 1,
            trigger: command.trigger,
            completed_at: now_timestamp_ms(),
            offer_dismissed_at: None,
            offer_presented_at: None,
        };
        query(
            "INSERT INTO reading_completions
             (completion_id, space_id, user_id, source_id, completion_generation, trigger, command_key, request_hash, payload)
             VALUES ($1, $2, $3, $4, 1, $5, $6, $7, $8)",
        )
        .bind(completion.id)
        .bind(completion.space_id)
        .bind(user_id)
        .bind(source.id)
        .bind(trigger_key(completion.trigger))
        .bind(command_key)
        .bind(hash.as_slice())
        .bind(serde_json::to_value(&completion).map_err(|_| LearningStoreError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
        append_sync_change(
            &mut tx,
            source.space_id,
            user_id,
            device_id,
            "reading_completion",
            completion.id,
            1,
            "create",
            &completion,
            &format!("learning:completion:{command_key}"),
        )
        .await?;
        (completion, true)
    };
    let offer = pg_offer_tx(&mut tx, user_id, command.material_id, &source, &completion).await?;
    let result = CompleteReadingResponse { offer, created };
    if result.offer.should_offer {
        let mut stored_completion = completion.clone();
        stored_completion.offer_presented_at = Some(now_timestamp_ms());
        query(
            "UPDATE reading_completions
             SET offer_presented_at = now(), payload = $2
             WHERE completion_id = $1",
        )
        .bind(completion.id)
        .bind(
            serde_json::to_value(&stored_completion)
                .map_err(|_| LearningStoreError::Unavailable)?,
        )
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
    }
    pg_store_mutation(
        &mut tx,
        user_id,
        command_key,
        hash,
        "complete_reading",
        &result,
    )
    .await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(result)
}

async fn pg_ensure_source(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    context: &LearningMaterialContext,
    command: &CompleteReadingScopeCommand,
    scope_key: &str,
) -> Result<LearningSource, LearningStoreError> {
    if let Some(row) = query(
        "SELECT payload FROM learning_sources
         WHERE owner_user_id = $1 AND scope_key = $2 AND deleted_at IS NULL",
    )
    .bind(context.owner_user_id)
    .bind(scope_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    {
        return serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable);
    }
    let source = LearningSource {
        id: Uuid::now_v7(),
        space_id: context.space_id,
        material_id: context.material_id,
        document_revision_id: context.active_revision_id,
        scope_kind: command.scope_kind,
        scope_key: scope_key.to_owned(),
        content_unit_id: command.content_unit_id.clone(),
        anchor: command.anchor.clone(),
        source_hash: context.source_hash.clone(),
        title: context.title.clone(),
        created_at: now_timestamp_ms(),
    };
    query(
        "INSERT INTO learning_sources
         (source_id, space_id, owner_user_id, material_id, document_revision_id, scope_kind, scope_key,
          content_unit_id, anchor, source_hash, title, payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(source.id)
    .bind(source.space_id)
    .bind(context.owner_user_id)
    .bind(source.material_id)
    .bind(source.document_revision_id)
    .bind(scope_kind_key(source.scope_kind))
    .bind(&source.scope_key)
    .bind(&source.content_unit_id)
    .bind(
        source
            .anchor
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| LearningStoreError::Unavailable)?,
    )
    .bind(&source.source_hash)
    .bind(&source.title)
    .bind(serde_json::to_value(&source).map_err(|_| LearningStoreError::Unavailable)?)
    .execute(&mut **tx)
    .await
    .map_err(log_pg)?;
    Ok(source)
}

async fn pg_offer(
    pool: &PgPool,
    user_id: UserId,
    material_id: MaterialId,
    source_id: LearningSourceId,
) -> Result<LearningOffer, LearningStoreError> {
    let mut tx = pool.begin().await.map_err(log_pg)?;
    let source = pg_source(&mut tx, user_id, source_id).await?;
    if source.material_id != material_id {
        return Err(LearningStoreError::NotFound);
    }
    let completion = pg_completion(&mut tx, user_id, source_id).await?;
    let offer = pg_offer_tx(&mut tx, user_id, material_id, &source, &completion).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(offer)
}

async fn pg_source(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    source_id: LearningSourceId,
) -> Result<LearningSource, LearningStoreError> {
    let row = query(
        "SELECT payload FROM learning_sources
         WHERE owner_user_id = $1 AND source_id = $2 AND deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
        .map_err(|_| LearningStoreError::Unavailable)
}

async fn pg_completion(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    source_id: LearningSourceId,
) -> Result<ReadingCompletion, LearningStoreError> {
    let row = query(
        "SELECT payload, offer_presented_at, offer_dismissed_at FROM reading_completions
         WHERE user_id = $1 AND source_id = $2
         ORDER BY completion_generation DESC LIMIT 1",
    )
    .bind(user_id)
    .bind(source_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let mut completion: ReadingCompletion =
        serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?;
    completion.offer_dismissed_at = row
        .try_get::<Option<OffsetDateTime>, _>("offer_dismissed_at")
        .map_err(log_pg)?
        .map(time_to_ms);
    completion.offer_presented_at = row
        .try_get::<Option<OffsetDateTime>, _>("offer_presented_at")
        .map_err(log_pg)?
        .map(time_to_ms);
    Ok(completion)
}

async fn pg_offer_tx(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    material_id: MaterialId,
    source: &LearningSource,
    completion: &ReadingCompletion,
) -> Result<LearningOffer, LearningStoreError> {
    let offers_enabled = query(
        "SELECT completion_offers_enabled FROM learning_source_settings
         WHERE user_id = $1 AND material_id = $2",
    )
    .bind(user_id)
    .bind(material_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    .map(|row| row.try_get("completion_offers_enabled").map_err(log_pg))
    .transpose()?
    .unwrap_or(true);
    let active_item_count: i64 = query_scalar(
        "SELECT count(*) FROM learning_items
         WHERE owner_user_id = $1 AND source_id = $2 AND status = 'active' AND deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(source.id)
    .fetch_one(&mut **tx)
    .await
    .map_err(log_pg)?;
    Ok(LearningOffer {
        source: source.clone(),
        completion: completion.clone(),
        active_item_count: usize::try_from(active_item_count).unwrap_or_default(),
        should_offer: offers_enabled
            && completion.offer_dismissed_at.is_none()
            && completion.offer_presented_at.is_none(),
        offers_enabled,
    })
}

async fn pg_update_offer(
    pool: &PgPool,
    user_id: UserId,
    material_id: MaterialId,
    command: &UpdateLearningOfferCommand,
    command_key: &str,
) -> Result<LearningOffer, LearningStoreError> {
    let hash = request_hash(command)?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "update_offer").await?
    {
        return Ok(value);
    }
    let row = query(
        "SELECT rc.source_id
         FROM reading_completions rc
         JOIN learning_sources ls ON ls.source_id = rc.source_id
         WHERE rc.user_id = $1 AND rc.completion_id = $2
           AND ls.material_id = $3 AND ls.deleted_at IS NULL
         FOR UPDATE OF rc",
    )
    .bind(user_id)
    .bind(command.completion_id)
    .bind(material_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let source_id: Uuid = row.try_get("source_id").map_err(log_pg)?;
    if command.action == LearningOfferAction::DisableMaterialOffers {
        query(
            "INSERT INTO learning_source_settings
             (user_id, material_id, completion_offers_enabled)
             VALUES ($1, $2, false)
             ON CONFLICT (user_id, material_id) DO UPDATE
             SET completion_offers_enabled = false,
                 object_revision = learning_source_settings.object_revision + 1,
                 updated_at = now()",
        )
        .bind(user_id)
        .bind(material_id)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
    }
    query(
        "UPDATE reading_completions
         SET offer_dismissed_at = COALESCE(offer_dismissed_at, now())
         WHERE completion_id = $1",
    )
    .bind(command.completion_id)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let source = pg_source(&mut tx, user_id, source_id).await?;
    let completion = pg_completion(&mut tx, user_id, source_id).await?;
    let offer = pg_offer_tx(&mut tx, user_id, material_id, &source, &completion).await?;
    pg_store_mutation(&mut tx, user_id, command_key, hash, "update_offer", &offer).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(offer)
}

async fn pg_create_item(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    command: &CreateLearningItemCommand,
    command_key: &str,
) -> Result<LearningItem, LearningStoreError> {
    let hash = request_hash(command)?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "create_item").await?
    {
        return Ok(value);
    }
    let source = pg_source(&mut tx, user_id, command.source_id).await?;
    let now = now_timestamp_ms();
    let item_id = Uuid::now_v7();
    let revision = LearningItemRevision {
        id: Uuid::now_v7(),
        item_id,
        revision: 1,
        prompt: command.prompt.clone(),
        answer_spec: command.answer_spec.clone(),
        explanation: command.explanation.clone(),
        hints: command.hints.clone(),
        source_anchor: command.source_anchor.clone(),
        created_at: now,
    };
    revision.validate(source.document_revision_id)?;
    if !item_kind_matches(command.kind, &revision) {
        return Err(LearningStoreError::InvalidDetail(
            "item kind does not match answer specification".to_owned(),
        ));
    }
    let item = LearningItem {
        id: item_id,
        source_id: source.id,
        kind: command.kind,
        status: command.status,
        origin: LearningItemOrigin::User,
        current_revision: revision.clone(),
        object_revision: 1,
        created_at: now,
        updated_at: now,
    };
    query(
        "INSERT INTO learning_items
         (item_id, space_id, owner_user_id, source_id, kind, status, origin,
          current_revision_id, command_key, request_hash)
         VALUES ($1, $2, $3, $4, $5, $6, 'user', NULL, $7, $8)",
    )
    .bind(item.id)
    .bind(source.space_id)
    .bind(user_id)
    .bind(source.id)
    .bind(item_kind_key(item.kind))
    .bind(item_status_key(item.status))
    .bind(command_key)
    .bind(hash.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    query(
        "INSERT INTO learning_item_revisions
         (item_revision_id, item_id, revision, payload)
         VALUES ($1, $2, 1, $3)",
    )
    .bind(revision.id)
    .bind(item.id)
    .bind(serde_json::to_value(&revision).map_err(|_| LearningStoreError::Unavailable)?)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    query("UPDATE learning_items SET current_revision_id = $2 WHERE item_id = $1")
        .bind(item.id)
        .bind(revision.id)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
    append_sync_change(
        &mut tx,
        source.space_id,
        user_id,
        device_id,
        "learning_item",
        item.id,
        item.object_revision,
        "create",
        &item,
        &format!("learning:item:{command_key}"),
    )
    .await?;
    pg_store_mutation(&mut tx, user_id, command_key, hash, "create_item", &item).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(item)
}

async fn pg_publish_generated_drafts(
    pool: &PgPool,
    user_id: UserId,
    source_id: LearningSourceId,
    task_id: Uuid,
    artifact_id: Uuid,
    payload: &QuestionSetArtifactPayload,
) -> Result<Vec<LearningItem>, LearningStoreError> {
    let mut tx = pool.begin().await.map_err(log_pg)?;
    let source = pg_source(&mut tx, user_id, source_id).await?;
    let artifact_owned: bool = query(
        "SELECT EXISTS(
            SELECT 1 FROM ai_artifacts
            WHERE artifact_id = $1 AND task_id = $2 AND user_id = $3
              AND kind = 'question_set_artifact' AND status = 'candidate'
        ) AS owned",
    )
    .bind(artifact_id)
    .bind(task_id)
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(log_pg)?
    .try_get("owned")
    .map_err(log_pg)?;
    if !artifact_owned {
        return Err(LearningStoreError::NotFound);
    }
    let mut drafts = Vec::with_capacity(payload.items.len());
    for (index, generated) in payload.items.iter().enumerate() {
        let answer_spec: lumi_core::LearningAnswerSpec =
            serde_json::from_value(generated.answer_spec.clone())
                .map_err(|_| LearningStoreError::InvalidDetail("invalid answer_spec".to_owned()))?;
        let kind = parse_item_kind(&generated.kind)?;
        let now = now_timestamp_ms();
        let item_id = Uuid::now_v7();
        let revision = LearningItemRevision {
            id: Uuid::now_v7(),
            item_id,
            revision: 1,
            prompt: generated.prompt.clone(),
            answer_spec,
            explanation: generated.explanation.clone(),
            hints: Vec::new(),
            source_anchor: source.anchor.clone(),
            created_at: now,
        };
        revision.validate(source.document_revision_id)?;
        if !item_kind_matches(kind, &revision) {
            return Err(LearningStoreError::InvalidDetail(
                "generated item kind does not match answer specification".to_owned(),
            ));
        }
        let command_key = format!("ai-artifact:{artifact_id}:{index}");
        let request_hash = Sha256::digest(
            serde_json::to_vec(generated).map_err(|_| LearningStoreError::Unavailable)?,
        );
        let item = LearningItem {
            id: item_id,
            source_id,
            kind,
            status: LearningItemStatus::Draft,
            origin: LearningItemOrigin::AiGenerated,
            current_revision: revision.clone(),
            object_revision: 1,
            created_at: now,
            updated_at: now,
        };
        query(
            "INSERT INTO learning_items
             (item_id, space_id, owner_user_id, source_id, kind, status, origin,
              current_revision_id, command_key, request_hash)
             VALUES ($1, $2, $3, $4, $5, 'draft', 'ai_generated', NULL, $6, $7)
             ON CONFLICT (owner_user_id, command_key) DO NOTHING",
        )
        .bind(item.id)
        .bind(source.space_id)
        .bind(user_id)
        .bind(source_id)
        .bind(item_kind_key(kind))
        .bind(&command_key)
        .bind(request_hash.as_slice())
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
        query(
            "INSERT INTO learning_item_revisions
             (item_revision_id, item_id, revision, payload)
             VALUES ($1, $2, 1, $3)",
        )
        .bind(revision.id)
        .bind(item.id)
        .bind(serde_json::to_value(&revision).map_err(|_| LearningStoreError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
        query("UPDATE learning_items SET current_revision_id = $2 WHERE item_id = $1")
            .bind(item.id)
            .bind(revision.id)
            .execute(&mut *tx)
            .await
            .map_err(log_pg)?;
        query(
            "INSERT INTO learning_generated_item_provenance
             (item_id, artifact_id, task_id, citation_ids)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(item.id)
        .bind(artifact_id)
        .bind(task_id)
        .bind(
            serde_json::to_value(&generated.citation_ids)
                .map_err(|_| LearningStoreError::Unavailable)?,
        )
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
        drafts.push(item);
    }
    tx.commit().await.map_err(log_pg)?;
    Ok(drafts)
}

async fn pg_get_item(
    pool: &PgPool,
    user_id: UserId,
    item_id: LearningItemId,
) -> Result<LearningItem, LearningStoreError> {
    let row = query(
        "SELECT li.item_id, li.source_id, li.kind, li.status, li.origin,
                li.object_revision, li.created_at, li.updated_at, lir.payload
         FROM learning_items li
         JOIN learning_item_revisions lir ON lir.item_revision_id = li.current_revision_id
         WHERE li.owner_user_id = $1 AND li.item_id = $2 AND li.deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(pool)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    item_from_row(&row)
}

fn item_from_row(row: &sqlx_postgres::PgRow) -> Result<LearningItem, LearningStoreError> {
    Ok(LearningItem {
        id: row.try_get("item_id").map_err(log_pg)?,
        source_id: row.try_get("source_id").map_err(log_pg)?,
        kind: parse_item_kind(&row.try_get::<String, _>("kind").map_err(log_pg)?)?,
        status: parse_item_status(&row.try_get::<String, _>("status").map_err(log_pg)?)?,
        origin: parse_item_origin(&row.try_get::<String, _>("origin").map_err(log_pg)?)?,
        current_revision: serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        object_revision: u64::try_from(row.try_get::<i64, _>("object_revision").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_pg)?),
        updated_at: time_to_ms(row.try_get("updated_at").map_err(log_pg)?),
    })
}

async fn pg_list_items(
    pool: &PgPool,
    user_id: UserId,
    material_id: Option<MaterialId>,
    source_id: Option<LearningSourceId>,
    status: Option<LearningItemStatus>,
    cursor: Option<LearningItemId>,
    limit: usize,
) -> Result<LearningItemPage, LearningStoreError> {
    let rows = query(
        "SELECT li.item_id, li.source_id, li.kind, li.status, li.origin,
                li.object_revision, li.created_at, li.updated_at, lir.payload
         FROM learning_items li
         JOIN learning_sources ls ON ls.source_id = li.source_id
         JOIN learning_item_revisions lir ON lir.item_revision_id = li.current_revision_id
         WHERE li.owner_user_id = $1 AND li.deleted_at IS NULL
           AND ($2::uuid IS NULL OR ls.material_id = $2)
           AND ($3::uuid IS NULL OR li.source_id = $3)
           AND ($4::text IS NULL OR li.status = $4)
           AND ($5::uuid IS NULL OR li.item_id > $5)
         ORDER BY li.item_id
         LIMIT $6",
    )
    .bind(user_id)
    .bind(material_id)
    .bind(source_id)
    .bind(status.map(item_status_key))
    .bind(cursor)
    .bind(i64::try_from(limit.saturating_add(1)).unwrap_or(101))
    .fetch_all(pool)
    .await
    .map_err(log_pg)?;
    let has_more = rows.len() > limit;
    let mut items = rows
        .iter()
        .take(limit)
        .map(item_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = has_more
        .then(|| items.last().map(|item| item.id.to_string()))
        .flatten();
    items.shrink_to_fit();
    Ok(LearningItemPage { items, next_cursor })
}

async fn pg_update_item(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    item_id: LearningItemId,
    command: &UpdateLearningItemCommand,
    command_key: &str,
) -> Result<LearningItem, LearningStoreError> {
    let hash = request_hash(&(item_id, command))?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "update_item").await?
    {
        return Ok(value);
    }
    let row = query(
        "SELECT li.source_id, li.kind, li.object_revision, lir.revision
         FROM learning_items li
         JOIN learning_item_revisions lir ON lir.item_revision_id = li.current_revision_id
         WHERE li.owner_user_id = $1 AND li.item_id = $2 AND li.deleted_at IS NULL
         FOR UPDATE OF li",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let object_revision = u64::try_from(row.try_get::<i64, _>("object_revision").map_err(log_pg)?)
        .map_err(|_| LearningStoreError::Unavailable)?;
    if object_revision != command.expected_revision {
        return Err(LearningStoreError::Conflict);
    }
    let source_id: Uuid = row.try_get("source_id").map_err(log_pg)?;
    let source = pg_source(&mut tx, user_id, source_id).await?;
    let kind = parse_item_kind(&row.try_get::<String, _>("kind").map_err(log_pg)?)?;
    let revision_number = u64::try_from(row.try_get::<i64, _>("revision").map_err(log_pg)?)
        .map_err(|_| LearningStoreError::Unavailable)?
        .saturating_add(1);
    let revision = LearningItemRevision {
        id: Uuid::now_v7(),
        item_id,
        revision: revision_number,
        prompt: command.prompt.clone(),
        answer_spec: command.answer_spec.clone(),
        explanation: command.explanation.clone(),
        hints: command.hints.clone(),
        source_anchor: command.source_anchor.clone(),
        created_at: now_timestamp_ms(),
    };
    revision.validate(source.document_revision_id)?;
    if !item_kind_matches(kind, &revision) {
        return Err(LearningStoreError::InvalidDetail(
            "item kind does not match answer specification".to_owned(),
        ));
    }
    query(
        "INSERT INTO learning_item_revisions
         (item_revision_id, item_id, revision, payload)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(revision.id)
    .bind(item_id)
    .bind(
        i64::try_from(revision.revision).map_err(|_| {
            LearningStoreError::InvalidDetail("item revision is too large".to_owned())
        })?,
    )
    .bind(serde_json::to_value(&revision).map_err(|_| LearningStoreError::Unavailable)?)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    query(
        "UPDATE learning_items
         SET current_revision_id = $2, object_revision = object_revision + 1, updated_at = now()
         WHERE item_id = $1",
    )
    .bind(item_id)
    .bind(revision.id)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let item = pg_get_item_tx(&mut tx, user_id, item_id).await?;
    append_sync_change(
        &mut tx,
        source.space_id,
        user_id,
        device_id,
        "learning_item",
        item.id,
        item.object_revision,
        "update",
        &item,
        &format!("learning:item-update:{command_key}"),
    )
    .await?;
    pg_store_mutation(&mut tx, user_id, command_key, hash, "update_item", &item).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(item)
}

async fn pg_change_item_status(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    item_id: LearningItemId,
    status: LearningItemStatus,
    command: ChangeLearningItemStatusCommand,
    command_key: &str,
) -> Result<LearningItem, LearningStoreError> {
    let hash = request_hash(&(item_id, status, command))?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "item_status").await?
    {
        return Ok(value);
    }
    let row = query(
        "SELECT li.object_revision, ls.space_id
         FROM learning_items li
         JOIN learning_sources ls ON ls.source_id = li.source_id
         WHERE li.owner_user_id = $1 AND li.item_id = $2 AND li.deleted_at IS NULL
         FOR UPDATE OF li",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let revision = u64::try_from(row.try_get::<i64, _>("object_revision").map_err(log_pg)?)
        .map_err(|_| LearningStoreError::Unavailable)?;
    if revision != command.expected_revision {
        return Err(LearningStoreError::Conflict);
    }
    let space_id: Uuid = row.try_get("space_id").map_err(log_pg)?;
    query(
        "UPDATE learning_items
         SET status = $2, object_revision = object_revision + 1, updated_at = now()
         WHERE item_id = $1",
    )
    .bind(item_id)
    .bind(item_status_key(status))
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let item = pg_get_item_tx(&mut tx, user_id, item_id).await?;
    append_sync_change(
        &mut tx,
        space_id,
        user_id,
        device_id,
        "learning_item",
        item.id,
        item.object_revision,
        "update",
        &item,
        &format!("learning:item-status:{command_key}"),
    )
    .await?;
    pg_store_mutation(&mut tx, user_id, command_key, hash, "item_status", &item).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(item)
}

async fn pg_get_item_tx(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    item_id: LearningItemId,
) -> Result<LearningItem, LearningStoreError> {
    let row = query(
        "SELECT li.item_id, li.source_id, li.kind, li.status, li.origin,
                li.object_revision, li.created_at, li.updated_at, lir.payload
         FROM learning_items li
         JOIN learning_item_revisions lir ON lir.item_revision_id = li.current_revision_id
         WHERE li.owner_user_id = $1 AND li.item_id = $2 AND li.deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    item_from_row(&row)
}

async fn pg_create_session(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    command: CreateLearningSessionCommand,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&command)?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "create_session").await?
    {
        return Ok(value);
    }
    let source = pg_source(&mut tx, user_id, command.source_id).await?;
    let rows = query(
        "SELECT li.item_id, li.kind, lir.item_revision_id, lir.payload
         FROM learning_items li
         JOIN learning_item_revisions lir ON lir.item_revision_id = li.current_revision_id
         WHERE li.owner_user_id = $1 AND li.source_id = $2
           AND li.status = 'active' AND li.deleted_at IS NULL
           AND (
               ($3 <> 'explain_back' OR li.kind = 'explain_back_prompt')
               AND
               $3 <> 'scheduled_review'
               OR EXISTS (
                   SELECT 1 FROM learning_schedules lsch
                   WHERE lsch.user_id = $1 AND lsch.item_id = li.item_id
                     AND lsch.state <> 'paused' AND lsch.due_at <= now()
               )
           )
         ORDER BY li.item_id
         LIMIT 7",
    )
    .bind(user_id)
    .bind(command.source_id)
    .bind(session_kind_key(command.kind))
    .fetch_all(&mut *tx)
    .await
    .map_err(log_pg)?;
    if rows.is_empty() {
        return Err(LearningStoreError::InvalidDetail(
            "source has no active learning items".to_owned(),
        ));
    }
    let now = now_timestamp_ms();
    let session_id = Uuid::now_v7();
    query(
        "INSERT INTO learning_sessions
         (session_id, space_id, user_id, source_id, kind, state, command_key, request_hash)
         VALUES ($1, $2, $3, $4, $5, 'offered', $6, $7)",
    )
    .bind(session_id)
    .bind(source.space_id)
    .bind(user_id)
    .bind(source.id)
    .bind(session_kind_key(command.kind))
    .bind(command_key)
    .bind(hash.as_slice())
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let mut items = Vec::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        let revision: LearningItemRevision =
            serde_json::from_value(row.try_get("payload").map_err(log_pg)?)
                .map_err(|_| LearningStoreError::Unavailable)?;
        let kind = parse_item_kind(&row.try_get::<String, _>("kind").map_err(log_pg)?)?;
        let snapshot = SessionItemSnapshot { kind, revision };
        let item = public_snapshot(&snapshot, position);
        query(
            "INSERT INTO learning_session_items
             (session_id, item_id, item_revision_id, position, selection_reason, snapshot)
             VALUES ($1, $2, $3, $4, 'source_active_stable_order', $5)",
        )
        .bind(session_id)
        .bind(item.item_id)
        .bind(item.item_revision_id)
        .bind(i16::try_from(position).unwrap_or(i16::MAX))
        .bind(serde_json::to_value(&snapshot).map_err(|_| LearningStoreError::Unavailable)?)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
        items.push(item);
    }
    let session = LearningSession {
        id: session_id,
        user_id,
        source,
        kind: command.kind,
        state: LearningSessionState::Offered,
        items,
        attempts: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    append_sync_change(
        &mut tx,
        session.source.space_id,
        user_id,
        device_id,
        "learning_session",
        session.id,
        1,
        "create",
        &session,
        &format!("learning:session:{command_key}"),
    )
    .await?;
    pg_store_mutation(
        &mut tx,
        user_id,
        command_key,
        hash,
        "create_session",
        &session,
    )
    .await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(session)
}

async fn pg_get_session(
    pool: &PgPool,
    user_id: UserId,
    session_id: LearningSessionId,
) -> Result<LearningSession, LearningStoreError> {
    let mut tx = pool.begin().await.map_err(log_pg)?;
    let session = pg_get_session_tx(&mut tx, user_id, session_id).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(session)
}

async fn pg_get_session_tx(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    session_id: LearningSessionId,
) -> Result<LearningSession, LearningStoreError> {
    let row = query(
        "SELECT lsess.session_id, lsess.kind, lsess.state, lsess.created_at, lsess.updated_at,
                lsrc.payload AS source_payload
         FROM learning_sessions lsess
         JOIN learning_sources lsrc ON lsrc.source_id = lsess.source_id
         WHERE lsess.user_id = $1 AND lsess.session_id = $2",
    )
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let item_rows = query(
        "SELECT position, snapshot
         FROM learning_session_items
         WHERE session_id = $1 ORDER BY position",
    )
    .bind(session_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(log_pg)?;
    let mut items = item_rows
        .iter()
        .map(|item_row| {
            let snapshot: SessionItemSnapshot =
                serde_json::from_value(item_row.try_get("snapshot").map_err(log_pg)?)
                    .map_err(|_| LearningStoreError::Unavailable)?;
            let position: i16 = item_row.try_get("position").map_err(log_pg)?;
            Ok(public_snapshot(
                &snapshot,
                usize::try_from(position).unwrap_or_default(),
            ))
        })
        .collect::<Result<Vec<_>, LearningStoreError>>()?;
    let hint_rows = query(
        "SELECT payload ->> 'item_id' AS item_id, count(*) AS hint_count
         FROM learning_attempt_events
         WHERE user_id = $1 AND session_id = $2 AND event_kind = 'hint_revealed'
         GROUP BY payload ->> 'item_id'",
    )
    .bind(user_id)
    .bind(session_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(log_pg)?;
    for hint_row in &hint_rows {
        let item_id = hint_row
            .try_get::<String, _>("item_id")
            .map_err(log_pg)?
            .parse::<Uuid>()
            .map_err(|_| LearningStoreError::Unavailable)?;
        let count = hint_row.try_get::<i64, _>("hint_count").map_err(log_pg)?;
        if let Some(item) = items.iter_mut().find(|item| item.item_id == item_id) {
            item.revealed_hint_count = u16::try_from(count).unwrap_or(u16::MAX);
        }
    }
    let attempt_rows = query(
        "SELECT payload FROM learning_attempts
         WHERE user_id = $1 AND session_id = $2 ORDER BY created_at, attempt_id",
    )
    .bind(user_id)
    .bind(session_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(log_pg)?;
    let attempts = attempt_rows
        .iter()
        .map(|attempt_row| {
            serde_json::from_value(attempt_row.try_get("payload").map_err(log_pg)?)
                .map_err(|_| LearningStoreError::Unavailable)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(LearningSession {
        id: session_id,
        user_id,
        source: serde_json::from_value(row.try_get("source_payload").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        kind: parse_session_kind(&row.try_get::<String, _>("kind").map_err(log_pg)?)?,
        state: parse_session_state(&row.try_get::<String, _>("state").map_err(log_pg)?)?,
        items,
        attempts,
        created_at: time_to_ms(row.try_get("created_at").map_err(log_pg)?),
        updated_at: time_to_ms(row.try_get("updated_at").map_err(log_pg)?),
    })
}

async fn pg_transition_session(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    session_id: LearningSessionId,
    transition: SessionTransition,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&(session_id, transition_key(transition)))?;
    let operation = format!("session_{}", transition_key(transition));
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) = pg_read_mutation(&mut tx, user_id, command_key, hash, &operation).await? {
        return Ok(value);
    }
    query(
        "SELECT session_id FROM learning_sessions
         WHERE user_id = $1 AND session_id = $2 FOR UPDATE",
    )
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let mut session = pg_get_session_tx(&mut tx, user_id, session_id).await?;
    match transition {
        SessionTransition::Start => session.start()?,
        SessionTransition::Complete => session.complete()?,
        SessionTransition::Abandon => session.abandon()?,
    }
    let object_revision: i64 = query_scalar(
        "UPDATE learning_sessions
         SET state = $2, object_revision = object_revision + 1, updated_at = now(),
             completed_at = CASE WHEN $2 = 'completed' THEN now() ELSE completed_at END
         WHERE session_id = $1
         RETURNING object_revision",
    )
    .bind(session_id)
    .bind(session_state_key(session.state))
    .fetch_one(&mut *tx)
    .await
    .map_err(log_pg)?;
    session = pg_get_session_tx(&mut tx, user_id, session_id).await?;
    append_sync_change(
        &mut tx,
        session.source.space_id,
        user_id,
        device_id,
        "learning_session",
        session.id,
        u64::try_from(object_revision).map_err(|_| LearningStoreError::Unavailable)?,
        "update",
        &session,
        &format!("learning:session-transition:{command_key}"),
    )
    .await?;
    pg_store_mutation(&mut tx, user_id, command_key, hash, &operation, &session).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(session)
}

async fn pg_source_opened(
    pool: &PgPool,
    user_id: UserId,
    session_id: LearningSessionId,
    command: RecordLearningSourceOpenedCommand,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&(session_id, command))?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "source_opened").await?
    {
        return Ok(value);
    }
    let exists = query(
        "SELECT 1
         FROM learning_sessions ls
         JOIN learning_session_items lsi ON lsi.session_id = ls.session_id
         WHERE ls.user_id = $1 AND ls.session_id = $2 AND lsi.item_id = $3",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(command.item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .is_some();
    if !exists {
        return Err(LearningStoreError::NotFound);
    }
    query(
        "INSERT INTO learning_attempt_events
         (event_id, session_id, user_id, event_kind, payload)
         VALUES ($1, $2, $3, 'source_opened', $4)",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(user_id)
    .bind(serde_json::json!({ "item_id": command.item_id }))
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let session = pg_get_session_tx(&mut tx, user_id, session_id).await?;
    pg_store_mutation(
        &mut tx,
        user_id,
        command_key,
        hash,
        "source_opened",
        &session,
    )
    .await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(session)
}

async fn pg_reveal_hint(
    pool: &PgPool,
    user_id: UserId,
    session_id: LearningSessionId,
    command: RevealLearningHintCommand,
    command_key: &str,
) -> Result<LearningHintReveal, LearningStoreError> {
    let hash = request_hash(&(session_id, command))?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "reveal_hint").await?
    {
        return Ok(value);
    }
    let row = query(
        "SELECT lsi.snapshot
         FROM learning_sessions ls
         JOIN learning_session_items lsi ON lsi.session_id = ls.session_id
         WHERE ls.user_id = $1 AND ls.session_id = $2 AND lsi.item_id = $3
         FOR UPDATE OF ls",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(command.item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let snapshot: SessionItemSnapshot =
        serde_json::from_value(row.try_get("snapshot").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?;
    let count: i64 = query_scalar(
        "SELECT count(*) FROM learning_attempt_events
         WHERE user_id = $1 AND session_id = $2 AND event_kind = 'hint_revealed'
           AND payload ->> 'item_id' = $3",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(command.item_id.to_string())
    .fetch_one(&mut *tx)
    .await
    .map_err(log_pg)?;
    let expected = u16::try_from(count).unwrap_or(u16::MAX).saturating_add(1);
    if command.position != expected {
        return Err(LearningStoreError::Conflict);
    }
    let hint = snapshot
        .revision
        .hints
        .iter()
        .find(|hint| hint.position == command.position)
        .cloned()
        .ok_or(LearningStoreError::NotFound)?;
    query(
        "INSERT INTO learning_attempt_events
         (event_id, session_id, user_id, event_kind, payload)
         VALUES ($1, $2, $3, 'hint_revealed', $4)",
    )
    .bind(Uuid::now_v7())
    .bind(session_id)
    .bind(user_id)
    .bind(serde_json::json!({
        "item_id": command.item_id,
        "position": command.position
    }))
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let result = LearningHintReveal {
        hint,
        revealed_count: command.position,
    };
    pg_store_mutation(&mut tx, user_id, command_key, hash, "reveal_hint", &result).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(result)
}

async fn pg_settings(
    pool: &PgPool,
    user_id: UserId,
) -> Result<LearningSettings, LearningStoreError> {
    let row = query(
        "SELECT scheduling_enabled, daily_limit, manual_only, object_revision
         FROM learning_settings WHERE user_id = $1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .map_err(log_pg)?;
    row.as_ref().map_or_else(
        || Ok(LearningSettings::default()),
        learning_settings_from_row,
    )
}

async fn pg_update_settings(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    command: &UpdateLearningSettingsCommand,
    command_key: &str,
) -> Result<LearningSettings, LearningStoreError> {
    let hash = request_hash(command)?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "update_settings").await?
    {
        return Ok(value);
    }
    let current = query(
        "SELECT object_revision FROM learning_settings
         WHERE user_id = $1 FOR UPDATE",
    )
    .bind(user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?;
    let current_revision = current
        .as_ref()
        .map(|row| row.try_get::<i64, _>("object_revision").map_err(log_pg))
        .transpose()?
        .map_or(1, |value| u64::try_from(value).unwrap_or(u64::MAX));
    if current_revision != command.expected_revision {
        return Err(LearningStoreError::Conflict);
    }
    let object_revision = current_revision.saturating_add(1);
    query(
        "INSERT INTO learning_settings
         (user_id, scheduling_enabled, daily_limit, manual_only, object_revision, updated_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (user_id) DO UPDATE SET
           scheduling_enabled = EXCLUDED.scheduling_enabled,
           daily_limit = EXCLUDED.daily_limit,
           manual_only = EXCLUDED.manual_only,
           object_revision = EXCLUDED.object_revision,
           updated_at = now()",
    )
    .bind(user_id)
    .bind(command.scheduling_enabled)
    .bind(i16::try_from(command.daily_limit).unwrap_or(i16::MAX))
    .bind(command.manual_only)
    .bind(i64::try_from(object_revision).unwrap_or(i64::MAX))
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let settings = LearningSettings {
        scheduling_enabled: command.scheduling_enabled,
        daily_limit: command.daily_limit,
        manual_only: command.manual_only,
        object_revision,
    };
    let space_id: Uuid = query_scalar(
        "SELECT space_id FROM sync_spaces
         WHERE owner_user_id = $1 AND kind = 'personal'
         ORDER BY created_at LIMIT 1",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(log_pg)?;
    append_sync_change(
        &mut tx,
        space_id,
        user_id,
        device_id,
        "learning_settings",
        user_id,
        object_revision,
        "update",
        &settings,
        &format!("learning:settings:{command_key}"),
    )
    .await?;
    pg_store_mutation(
        &mut tx,
        user_id,
        command_key,
        hash,
        "update_settings",
        &settings,
    )
    .await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(settings)
}

async fn pg_source_schedule_settings(
    pool: &PgPool,
    user_id: UserId,
    source_id: LearningSourceId,
) -> Result<LearningSourceScheduleSettings, LearningStoreError> {
    let row = query(
        "SELECT lsrc.source_id, lsss.scheduling_enabled, lsss.paused_at,
                lsss.object_revision
         FROM learning_sources lsrc
         LEFT JOIN learning_source_schedule_settings lsss
           ON lsss.user_id = $1 AND lsss.source_id = lsrc.source_id
         WHERE lsrc.owner_user_id = $1 AND lsrc.source_id = $2
           AND lsrc.deleted_at IS NULL",
    )
    .bind(user_id)
    .bind(source_id)
    .fetch_optional(pool)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    if row
        .try_get::<Option<i64>, _>("object_revision")
        .map_err(log_pg)?
        .is_none()
    {
        return Ok(default_source_settings(source_id));
    }
    source_settings_from_row(&row)
}

async fn pg_set_source_paused(
    pool: &PgPool,
    user_id: UserId,
    device_id: Uuid,
    source_id: LearningSourceId,
    paused: bool,
    command_key: &str,
) -> Result<LearningSourceScheduleSettings, LearningStoreError> {
    let hash = request_hash(&(source_id, paused))?;
    let operation = if paused {
        "pause_source"
    } else {
        "resume_source"
    };
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) = pg_read_mutation(&mut tx, user_id, command_key, hash, operation).await? {
        return Ok(value);
    }
    let space_id: Uuid = query_scalar(
        "SELECT space_id FROM learning_sources
         WHERE owner_user_id = $1 AND source_id = $2 AND deleted_at IS NULL
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let previous_revision: Option<i64> = query_scalar(
        "SELECT object_revision FROM learning_source_schedule_settings
         WHERE user_id = $1 AND source_id = $2 FOR UPDATE",
    )
    .bind(user_id)
    .bind(source_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?;
    let object_revision = previous_revision
        .map_or(2, |revision| revision.saturating_add(1))
        .max(1);
    query(
        "INSERT INTO learning_source_schedule_settings
         (user_id, source_id, scheduling_enabled, paused_at, object_revision, updated_at)
         VALUES ($1, $2, true, CASE WHEN $3 THEN now() ELSE NULL END, $4, now())
         ON CONFLICT (user_id, source_id) DO UPDATE SET
           paused_at = CASE WHEN $3 THEN now() ELSE NULL END,
           object_revision = $4, updated_at = now()",
    )
    .bind(user_id)
    .bind(source_id)
    .bind(paused)
    .bind(object_revision)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    if paused {
        query(
            "UPDATE learning_schedules
             SET algorithm_payload = jsonb_set(
                   algorithm_payload, '{state_before_pause}', to_jsonb(state), true
                 ),
                 state = 'paused', paused_at = now(),
                 object_revision = object_revision + 1, updated_at = now()
             WHERE user_id = $1 AND source_id = $2 AND state <> 'paused'",
        )
        .bind(user_id)
        .bind(source_id)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
    } else {
        query(
            "UPDATE learning_schedules
             SET state = CASE algorithm_payload ->> 'state_before_pause'
                   WHEN 'new' THEN 'new'
                   WHEN 'learning' THEN 'learning'
                   WHEN 'relearning' THEN 'relearning'
                   ELSE 'review'
                 END,
                 paused_at = NULL, due_at = GREATEST(due_at, now()),
                 object_revision = object_revision + 1, updated_at = now()
             WHERE user_id = $1 AND source_id = $2 AND state = 'paused'",
        )
        .bind(user_id)
        .bind(source_id)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
    }
    let settings = LearningSourceScheduleSettings {
        source_id,
        scheduling_enabled: true,
        paused_at: paused.then_some(now_timestamp_ms()),
        object_revision: u64::try_from(object_revision).unwrap_or(u64::MAX),
    };
    append_sync_change(
        &mut tx,
        space_id,
        user_id,
        device_id,
        "learning_source_settings",
        source_id,
        settings.object_revision,
        "update",
        &settings,
        &format!("learning:source-settings:{command_key}"),
    )
    .await?;
    pg_store_mutation(&mut tx, user_id, command_key, hash, operation, &settings).await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(settings)
}

async fn pg_today(
    pool: &PgPool,
    user_id: UserId,
    material_id: Option<MaterialId>,
) -> Result<LearningToday, LearningStoreError> {
    let settings = pg_settings(pool, user_id).await?;
    let rows = query(
        "SELECT li.item_id, li.status, lsrc.source_id, lsrc.material_id, lsrc.title,
                lsch.state AS schedule_state, lsch.due_at,
                lsss.paused_at AS source_paused_at,
                COALESCE(lsss.scheduling_enabled, true) AS source_enabled
         FROM learning_items li
         JOIN learning_sources lsrc ON lsrc.source_id = li.source_id
         LEFT JOIN learning_schedules lsch
           ON lsch.user_id = $1 AND lsch.item_id = li.item_id
         LEFT JOIN learning_source_schedule_settings lsss
           ON lsss.user_id = $1 AND lsss.source_id = lsrc.source_id
         WHERE li.owner_user_id = $1 AND li.deleted_at IS NULL
           AND ($2::uuid IS NULL OR lsrc.material_id = $2)
         ORDER BY lsch.due_at NULLS LAST, li.item_id",
    )
    .bind(user_id)
    .bind(material_id)
    .fetch_all(pool)
    .await
    .map_err(log_pg)?;
    let now = OffsetDateTime::now_utc();
    let mut due = Vec::new();
    let mut ready = Vec::new();
    let mut drafts = 0_u32;
    for row in &rows {
        let status = parse_item_status(&row.try_get::<String, _>("status").map_err(log_pg)?)?;
        if status == LearningItemStatus::Draft {
            drafts = drafts.saturating_add(1);
            continue;
        }
        if status != LearningItemStatus::Active {
            continue;
        }
        let paused = row
            .try_get::<Option<OffsetDateTime>, _>("source_paused_at")
            .map_err(log_pg)?
            .is_some()
            || !row.try_get::<bool, _>("source_enabled").map_err(log_pg)?;
        if paused {
            continue;
        }
        let item_id: Uuid = row.try_get("item_id").map_err(log_pg)?;
        let source_id: Uuid = row.try_get("source_id").map_err(log_pg)?;
        let schedule_state = row
            .try_get::<Option<String>, _>("schedule_state")
            .map_err(log_pg)?;
        let due_at = row
            .try_get::<Option<OffsetDateTime>, _>("due_at")
            .map_err(log_pg)?;
        match (schedule_state.as_deref(), due_at) {
            (Some(state), Some(due_at)) if state != "paused" && due_at <= now => {
                due.push((item_id, source_id));
            }
            (None, None) => ready.push((item_id, source_id)),
            _ => {}
        }
    }
    let counts = LearningChallengeCounts {
        due: u32::try_from(due.len()).unwrap_or(u32::MAX),
        ready: u32::try_from(ready.len()).unwrap_or(u32::MAX),
        drafts,
    };
    let due = if settings.scheduling_enabled && !settings.manual_only {
        due.into_iter()
            .take(usize::from(settings.daily_limit))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    ready.truncate(usize::from(settings.daily_limit));
    Ok(LearningToday {
        groups: pg_challenge_groups(&rows, &due)?,
        ready_groups: pg_challenge_groups(&rows, &ready)?,
        settings,
        counts,
        generated_at: now_timestamp_ms(),
    })
}

fn pg_challenge_groups(
    rows: &[sqlx_postgres::PgRow],
    items: &[(LearningItemId, LearningSourceId)],
) -> Result<Vec<LearningChallengeGroup>, LearningStoreError> {
    let mut groups = Vec::<LearningChallengeGroup>::new();
    for (item_id, source_id) in items {
        let row = rows
            .iter()
            .find(|row| row.try_get::<Uuid, _>("item_id").ok() == Some(*item_id))
            .ok_or(LearningStoreError::Unavailable)?;
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.source_id == *source_id)
        {
            group.item_ids.push(*item_id);
            group.estimated_minutes =
                u16::try_from((group.item_ids.len() * 3).div_ceil(2)).unwrap_or(u16::MAX);
        } else {
            groups.push(LearningChallengeGroup {
                source_id: *source_id,
                material_id: row.try_get("material_id").map_err(log_pg)?,
                title: row.try_get("title").map_err(log_pg)?,
                item_ids: vec![*item_id],
                estimated_minutes: 2,
                paused: false,
            });
        }
    }
    Ok(groups)
}

async fn pg_list_schedules(
    pool: &PgPool,
    user_id: UserId,
    material_id: Option<MaterialId>,
) -> Result<Vec<LearningSchedule>, LearningStoreError> {
    let rows = query(
        "SELECT lsch.item_id, lsch.state, lsch.due_at, lsch.stability,
                lsch.difficulty, lsch.last_review_at, lsch.repetitions,
                lsch.lapses, lsch.algorithm, lsch.algorithm_version,
                lsch.algorithm_payload, lsch.object_revision, lsch.paused_at
         FROM learning_schedules lsch
         JOIN learning_sources lsrc ON lsrc.source_id = lsch.source_id
         WHERE lsch.user_id = $1
           AND ($2::uuid IS NULL OR lsrc.material_id = $2)
         ORDER BY lsch.due_at, lsch.item_id",
    )
    .bind(user_id)
    .bind(material_id)
    .fetch_all(pool)
    .await
    .map_err(log_pg)?;
    rows.iter().map(schedule_from_row).collect()
}

async fn pg_snooze_session(
    pool: &PgPool,
    user_id: UserId,
    session_id: LearningSessionId,
    command: SnoozeLearningSessionCommand,
    command_key: &str,
) -> Result<LearningSession, LearningStoreError> {
    let hash = request_hash(&(session_id, command))?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "snooze_session").await?
    {
        return Ok(value);
    }
    let kind: String = query_scalar(
        "SELECT kind FROM learning_sessions
         WHERE user_id = $1 AND session_id = $2 FOR UPDATE",
    )
    .bind(user_id)
    .bind(session_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    if parse_session_kind(&kind)? != LearningSessionKind::ScheduledReview {
        return Err(LearningStoreError::Conflict);
    }
    let snoozed_until = OffsetDateTime::now_utc() + time::Duration::days(i64::from(command.days));
    query(
        "UPDATE learning_schedules lsch
         SET due_at = $3, object_revision = object_revision + 1, updated_at = now()
         FROM learning_session_items lsi
         WHERE lsi.session_id = $2 AND lsi.item_id = lsch.item_id
           AND lsch.user_id = $1
           AND NOT EXISTS (
             SELECT 1 FROM learning_attempts la
             WHERE la.session_id = $2 AND la.item_id = lsi.item_id
           )",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(snoozed_until)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    query(
        "INSERT INTO learning_session_snoozes
         (session_id, user_id, snoozed_until)
         VALUES ($1, $2, $3)
         ON CONFLICT (session_id) DO UPDATE SET snoozed_until = EXCLUDED.snoozed_until",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(snoozed_until)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    query(
        "UPDATE learning_sessions
         SET state = 'abandoned', object_revision = object_revision + 1, updated_at = now()
         WHERE user_id = $1 AND session_id = $2",
    )
    .bind(user_id)
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let session = pg_get_session_tx(&mut tx, user_id, session_id).await?;
    pg_store_mutation(
        &mut tx,
        user_id,
        command_key,
        hash,
        "snooze_session",
        &session,
    )
    .await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(session)
}

#[expect(
    clippy::too_many_arguments,
    reason = "transactional submit carries explicit owner, session, item, command and scheduler boundaries"
)]
async fn pg_submit_attempt(
    pool: &PgPool,
    scheduler: FsrsScheduler,
    user_id: UserId,
    device_id: Uuid,
    session_id: LearningSessionId,
    item_id: LearningItemId,
    command: &SubmitLearningAttemptCommand,
    command_key: &str,
) -> Result<LearningAttempt, LearningStoreError> {
    let hash = request_hash(&(session_id, item_id, command))?;
    let mut tx = pool.begin().await.map_err(log_pg)?;
    if let Some(value) =
        pg_read_mutation(&mut tx, user_id, command_key, hash, "submit_attempt").await?
    {
        return Ok(value);
    }
    let row = query(
        "SELECT ls.state, ls.space_id, ls.source_id, lsi.item_revision_id, lsi.snapshot
         FROM learning_sessions ls
         JOIN learning_session_items lsi ON lsi.session_id = ls.session_id
         WHERE ls.user_id = $1 AND ls.session_id = $2 AND lsi.item_id = $3
         FOR UPDATE OF ls",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .ok_or(LearningStoreError::NotFound)?;
    let state = parse_session_state(&row.try_get::<String, _>("state").map_err(log_pg)?)?;
    if state != LearningSessionState::InProgress {
        return Err(LearningStoreError::Conflict);
    }
    let snapshot: SessionItemSnapshot =
        serde_json::from_value(row.try_get("snapshot").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?;
    let feedback: LearningFeedback =
        grade_learning_answer(&snapshot.revision, &command.answer, command.self_check)?;
    let source_opened = query(
        "SELECT 1 FROM learning_attempt_events
         WHERE user_id = $1 AND session_id = $2 AND event_kind = 'source_opened'
           AND payload ->> 'item_id' = $3
         LIMIT 1",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(item_id.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?
    .is_some();
    let hint_rows = query(
        "SELECT payload FROM learning_attempt_events
         WHERE user_id = $1 AND session_id = $2 AND event_kind = 'hint_revealed'
           AND payload ->> 'item_id' = $3
         ORDER BY created_at, event_id",
    )
    .bind(user_id)
    .bind(session_id)
    .bind(item_id.to_string())
    .fetch_all(&mut *tx)
    .await
    .map_err(log_pg)?;
    let hints_used = hint_rows
        .iter()
        .filter_map(|hint_row| hint_row.try_get::<serde_json::Value, _>("payload").ok())
        .filter_map(|payload| payload.get("position").and_then(serde_json::Value::as_u64))
        .filter_map(|position| u16::try_from(position).ok())
        .collect::<Vec<_>>();
    let attempt = LearningAttempt {
        id: Uuid::now_v7(),
        session_id,
        item_id,
        item_revision_id: snapshot.revision.id,
        answer: command.answer.clone(),
        self_check: command.self_check,
        feedback,
        elapsed_ms: command.elapsed_ms.min(86_400_000),
        source_opened,
        hints_used: hints_used.clone(),
        review_rating: command.review_rating,
        created_at: now_timestamp_ms(),
    };
    query(
        "INSERT INTO learning_attempts
         (attempt_id, session_id, user_id, item_id, item_revision_id,
          command_key, request_hash, payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(attempt.id)
    .bind(session_id)
    .bind(user_id)
    .bind(item_id)
    .bind(attempt.item_revision_id)
    .bind(command_key)
    .bind(hash.as_slice())
    .bind(serde_json::to_value(&attempt).map_err(|_| LearningStoreError::Unavailable)?)
    .execute(&mut *tx)
    .await
    .map_err(map_pg_conflict)?;
    let schedule_row = query(
        "SELECT item_id, state, due_at, stability, difficulty, last_review_at,
                repetitions, lapses, algorithm, algorithm_version, algorithm_payload,
                object_revision, paused_at
         FROM learning_schedules
         WHERE user_id = $1 AND item_id = $2
         FOR UPDATE",
    )
    .bind(user_id)
    .bind(item_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_pg)?;
    let previous = schedule_row.as_ref().map(schedule_from_row).transpose()?;
    let rating = command.review_rating.unwrap_or_else(|| {
        conservative_rating(
            attempt.feedback.outcome,
            command.self_check,
            source_opened,
            &hints_used,
        )
    });
    let decision = scheduler.review_item(
        item_id,
        SchedulerReview {
            previous,
            rating,
            outcome: attempt.feedback.outcome,
            self_check: command.self_check,
            source_opened,
            hints_used,
            reviewed_at: attempt.created_at,
        },
    )?;
    let schedule = &decision.schedule;
    let source_id: Uuid = row.try_get("source_id").map_err(log_pg)?;
    query(
        "INSERT INTO learning_schedules
         (user_id, item_id, source_id, state, due_at, stability, difficulty,
          last_review_at, repetitions, lapses, algorithm, algorithm_version,
          algorithm_payload, object_revision, paused_at, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NULL, now())
         ON CONFLICT (user_id, item_id) DO UPDATE SET
          state = EXCLUDED.state, due_at = EXCLUDED.due_at,
          stability = EXCLUDED.stability, difficulty = EXCLUDED.difficulty,
          last_review_at = EXCLUDED.last_review_at, repetitions = EXCLUDED.repetitions,
          lapses = EXCLUDED.lapses, algorithm = EXCLUDED.algorithm,
          algorithm_version = EXCLUDED.algorithm_version,
          algorithm_payload = EXCLUDED.algorithm_payload,
          object_revision = EXCLUDED.object_revision, paused_at = NULL, updated_at = now()",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(source_id)
    .bind(schedule_state_key(schedule.state))
    .bind(ms_to_time(schedule.due_at)?)
    .bind(schedule.stability)
    .bind(schedule.difficulty)
    .bind(schedule.last_review_at.map(ms_to_time).transpose()?)
    .bind(i32::try_from(schedule.repetitions).unwrap_or(i32::MAX))
    .bind(i32::try_from(schedule.lapses).unwrap_or(i32::MAX))
    .bind(&schedule.algorithm)
    .bind(&schedule.algorithm_version)
    .bind(&schedule.algorithm_payload)
    .bind(i64::try_from(schedule.object_revision).unwrap_or(i64::MAX))
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    for event_kind in ["answer_submitted", "feedback_received"] {
        query(
            "INSERT INTO learning_attempt_events
             (event_id, attempt_id, session_id, user_id, event_kind)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(Uuid::now_v7())
        .bind(attempt.id)
        .bind(session_id)
        .bind(user_id)
        .bind(event_kind)
        .execute(&mut *tx)
        .await
        .map_err(log_pg)?;
    }
    query(
        "UPDATE learning_sessions
         SET object_revision = object_revision + 1, updated_at = now()
         WHERE session_id = $1",
    )
    .bind(session_id)
    .execute(&mut *tx)
    .await
    .map_err(log_pg)?;
    let space_id: Uuid = row.try_get("space_id").map_err(log_pg)?;
    append_sync_change(
        &mut tx,
        space_id,
        user_id,
        device_id,
        "learning_attempt",
        attempt.id,
        1,
        "append",
        &attempt,
        &format!("learning:attempt:{command_key}"),
    )
    .await?;
    append_sync_change(
        &mut tx,
        space_id,
        user_id,
        device_id,
        "learning_schedule",
        schedule.item_id,
        schedule.object_revision,
        "update",
        schedule,
        &format!("learning:schedule:{command_key}"),
    )
    .await?;
    pg_store_mutation(
        &mut tx,
        user_id,
        command_key,
        hash,
        "submit_attempt",
        &attempt,
    )
    .await?;
    tx.commit().await.map_err(log_pg)?;
    Ok(attempt)
}

async fn pg_read_mutation<T: for<'de> Deserialize<'de>>(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    command_key: &str,
    hash: [u8; 32],
    operation: &str,
) -> Result<Option<T>, LearningStoreError> {
    let row = query(
        "SELECT operation, request_hash, response_payload
         FROM learning_mutations WHERE user_id = $1 AND command_key = $2",
    )
    .bind(user_id)
    .bind(command_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(log_pg)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let stored_operation: String = row.try_get("operation").map_err(log_pg)?;
    let stored_hash: Vec<u8> = row.try_get("request_hash").map_err(log_pg)?;
    if stored_operation != operation || stored_hash.as_slice() != hash {
        return Err(LearningStoreError::Conflict);
    }
    serde_json::from_value(row.try_get("response_payload").map_err(log_pg)?)
        .map(Some)
        .map_err(|_| LearningStoreError::Unavailable)
}

async fn pg_store_mutation<T: Serialize>(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    user_id: UserId,
    command_key: &str,
    hash: [u8; 32],
    operation: &str,
    value: &T,
) -> Result<(), LearningStoreError> {
    query(
        "INSERT INTO learning_mutations
         (user_id, command_key, operation, request_hash, response_payload)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(user_id)
    .bind(command_key)
    .bind(operation)
    .bind(hash.as_slice())
    .bind(serde_json::to_value(value).map_err(|_| LearningStoreError::Unavailable)?)
    .execute(&mut **tx)
    .await
    .map_err(map_pg_conflict)?;
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "sync change rows mirror the canonical relational change contract"
)]
async fn append_sync_change<T: Serialize>(
    tx: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
    space_id: Uuid,
    user_id: UserId,
    device_id: Uuid,
    object_type: &str,
    object_id: Uuid,
    object_revision: u64,
    change_kind: &str,
    payload: &T,
    idempotency_key: &str,
) -> Result<(), LearningStoreError> {
    query(
        "INSERT INTO sync_changes
         (change_id, space_id, object_type, object_id, object_revision, change_kind,
          payload, device_id, hlc, schema_version, idempotency_key)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(Uuid::now_v7())
    .bind(space_id)
    .bind(object_type)
    .bind(object_id)
    .bind(i64::try_from(object_revision).map_err(|_| LearningStoreError::Unavailable)?)
    .bind(change_kind)
    .bind(serde_json::to_value(payload).map_err(|_| LearningStoreError::Unavailable)?)
    .bind(device_id)
    .bind(format!("{}:{user_id}", now_timestamp_ms()))
    .bind(lumi_core::DOMAIN_SCHEMA_VERSION)
    .bind(idempotency_key)
    .execute(&mut **tx)
    .await
    .map_err(map_pg_conflict)?;
    Ok(())
}

fn scope_kind_key(value: LearningScopeKind) -> &'static str {
    match value {
        LearningScopeKind::Material => "material",
        LearningScopeKind::ContentUnit => "content_unit",
        LearningScopeKind::Anchor => "anchor",
    }
}

fn trigger_key(value: lumi_core::ReadingCompletionTrigger) -> &'static str {
    match value {
        lumi_core::ReadingCompletionTrigger::ReaderBoundary => "reader_boundary",
        lumi_core::ReadingCompletionTrigger::ExplicitUserAction => "explicit_user_action",
    }
}

fn item_kind_key(value: LearningItemKind) -> &'static str {
    match value {
        LearningItemKind::QuizSingleChoice => "quiz_single_choice",
        LearningItemKind::QuizMultipleChoice => "quiz_multiple_choice",
        LearningItemKind::QuizTrueFalse => "quiz_true_false",
        LearningItemKind::OpenQuestion => "open_question",
        LearningItemKind::Flashcard => "flashcard",
        LearningItemKind::Cloze => "cloze",
        LearningItemKind::HintedQuestion => "hinted_question",
        LearningItemKind::ExplainBackPrompt => "explain_back_prompt",
        LearningItemKind::ReflectionPrompt => "reflection_prompt",
    }
}

fn parse_item_kind(value: &str) -> Result<LearningItemKind, LearningStoreError> {
    match value {
        "quiz_single_choice" => Ok(LearningItemKind::QuizSingleChoice),
        "quiz_multiple_choice" => Ok(LearningItemKind::QuizMultipleChoice),
        "quiz_true_false" => Ok(LearningItemKind::QuizTrueFalse),
        "open_question" => Ok(LearningItemKind::OpenQuestion),
        "flashcard" => Ok(LearningItemKind::Flashcard),
        "cloze" => Ok(LearningItemKind::Cloze),
        "hinted_question" => Ok(LearningItemKind::HintedQuestion),
        "explain_back_prompt" => Ok(LearningItemKind::ExplainBackPrompt),
        "reflection_prompt" => Ok(LearningItemKind::ReflectionPrompt),
        _ => Err(LearningStoreError::Unavailable),
    }
}

fn item_status_key(value: LearningItemStatus) -> &'static str {
    match value {
        LearningItemStatus::Draft => "draft",
        LearningItemStatus::Active => "active",
        LearningItemStatus::Archived => "archived",
        LearningItemStatus::Rejected => "rejected",
    }
}

fn parse_item_status(value: &str) -> Result<LearningItemStatus, LearningStoreError> {
    match value {
        "draft" => Ok(LearningItemStatus::Draft),
        "active" => Ok(LearningItemStatus::Active),
        "archived" => Ok(LearningItemStatus::Archived),
        "rejected" => Ok(LearningItemStatus::Rejected),
        _ => Err(LearningStoreError::Unavailable),
    }
}

fn parse_item_origin(value: &str) -> Result<LearningItemOrigin, LearningStoreError> {
    match value {
        "user" => Ok(LearningItemOrigin::User),
        "author" => Ok(LearningItemOrigin::Author),
        "ai_generated" => Ok(LearningItemOrigin::AiGenerated),
        "converted_annotation" => Ok(LearningItemOrigin::ConvertedAnnotation),
        _ => Err(LearningStoreError::Unavailable),
    }
}

fn session_kind_key(value: LearningSessionKind) -> &'static str {
    match value {
        LearningSessionKind::ImmediateRecall => "immediate_recall",
        LearningSessionKind::ScheduledReview => "scheduled_review",
        LearningSessionKind::ExplainBack => "explain_back",
        LearningSessionKind::ManualPractice => "manual_practice",
    }
}

fn parse_session_kind(value: &str) -> Result<LearningSessionKind, LearningStoreError> {
    match value {
        "immediate_recall" => Ok(LearningSessionKind::ImmediateRecall),
        "scheduled_review" => Ok(LearningSessionKind::ScheduledReview),
        "explain_back" => Ok(LearningSessionKind::ExplainBack),
        "manual_practice" => Ok(LearningSessionKind::ManualPractice),
        _ => Err(LearningStoreError::Unavailable),
    }
}

fn session_state_key(value: LearningSessionState) -> &'static str {
    match value {
        LearningSessionState::Offered => "offered",
        LearningSessionState::InProgress => "in_progress",
        LearningSessionState::Completed => "completed",
        LearningSessionState::Dismissed => "dismissed",
        LearningSessionState::Abandoned => "abandoned",
    }
}

fn parse_session_state(value: &str) -> Result<LearningSessionState, LearningStoreError> {
    match value {
        "offered" => Ok(LearningSessionState::Offered),
        "in_progress" => Ok(LearningSessionState::InProgress),
        "completed" => Ok(LearningSessionState::Completed),
        "dismissed" => Ok(LearningSessionState::Dismissed),
        "abandoned" => Ok(LearningSessionState::Abandoned),
        _ => Err(LearningStoreError::Unavailable),
    }
}

fn schedule_state_key(value: LearningScheduleState) -> &'static str {
    match value {
        LearningScheduleState::New => "new",
        LearningScheduleState::Learning => "learning",
        LearningScheduleState::Review => "review",
        LearningScheduleState::Relearning => "relearning",
        LearningScheduleState::Paused => "paused",
    }
}

fn parse_schedule_state_value(value: &str) -> Option<LearningScheduleState> {
    match value {
        "new" => Some(LearningScheduleState::New),
        "learning" => Some(LearningScheduleState::Learning),
        "review" => Some(LearningScheduleState::Review),
        "relearning" => Some(LearningScheduleState::Relearning),
        "paused" => Some(LearningScheduleState::Paused),
        _ => None,
    }
}

fn parse_schedule_state(value: &str) -> Result<LearningScheduleState, LearningStoreError> {
    parse_schedule_state_value(value).ok_or(LearningStoreError::Unavailable)
}

fn learning_settings_from_row(
    row: &sqlx_postgres::PgRow,
) -> Result<LearningSettings, LearningStoreError> {
    Ok(LearningSettings {
        scheduling_enabled: row.try_get("scheduling_enabled").map_err(log_pg)?,
        daily_limit: u16::try_from(row.try_get::<i16, _>("daily_limit").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        manual_only: row.try_get("manual_only").map_err(log_pg)?,
        object_revision: u64::try_from(row.try_get::<i64, _>("object_revision").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
    })
}

fn source_settings_from_row(
    row: &sqlx_postgres::PgRow,
) -> Result<LearningSourceScheduleSettings, LearningStoreError> {
    Ok(LearningSourceScheduleSettings {
        source_id: row.try_get("source_id").map_err(log_pg)?,
        scheduling_enabled: row
            .try_get::<Option<bool>, _>("scheduling_enabled")
            .map_err(log_pg)?
            .unwrap_or(true),
        paused_at: row
            .try_get::<Option<OffsetDateTime>, _>("paused_at")
            .map_err(log_pg)?
            .map(time_to_ms),
        object_revision: u64::try_from(
            row.try_get::<Option<i64>, _>("object_revision")
                .map_err(log_pg)?
                .unwrap_or(1),
        )
        .map_err(|_| LearningStoreError::Unavailable)?,
    })
}

fn schedule_from_row(row: &sqlx_postgres::PgRow) -> Result<LearningSchedule, LearningStoreError> {
    Ok(LearningSchedule {
        item_id: row.try_get("item_id").map_err(log_pg)?,
        state: parse_schedule_state(&row.try_get::<String, _>("state").map_err(log_pg)?)?,
        due_at: time_to_ms(row.try_get("due_at").map_err(log_pg)?),
        stability: row.try_get("stability").map_err(log_pg)?,
        difficulty: row.try_get("difficulty").map_err(log_pg)?,
        last_review_at: row
            .try_get::<Option<OffsetDateTime>, _>("last_review_at")
            .map_err(log_pg)?
            .map(time_to_ms),
        repetitions: u32::try_from(row.try_get::<i32, _>("repetitions").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        lapses: u32::try_from(row.try_get::<i32, _>("lapses").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        algorithm: row.try_get("algorithm").map_err(log_pg)?,
        algorithm_version: row.try_get("algorithm_version").map_err(log_pg)?,
        algorithm_payload: row.try_get("algorithm_payload").map_err(log_pg)?,
        object_revision: u64::try_from(row.try_get::<i64, _>("object_revision").map_err(log_pg)?)
            .map_err(|_| LearningStoreError::Unavailable)?,
        paused_at: row
            .try_get::<Option<OffsetDateTime>, _>("paused_at")
            .map_err(log_pg)?
            .map(time_to_ms),
    })
}

fn ms_to_time(value: u64) -> Result<OffsetDateTime, LearningStoreError> {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000)
        .map_err(|_| LearningStoreError::Unavailable)
}

fn time_to_ms(value: OffsetDateTime) -> u64 {
    let millis = value.unix_timestamp_nanos() / 1_000_000;
    u64::try_from(millis).unwrap_or_default()
}

fn map_pg_conflict(error: sqlx_core::Error) -> LearningStoreError {
    if error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation())
    {
        LearningStoreError::Conflict
    } else {
        log_pg(error)
    }
}

fn log_pg(error: sqlx_core::Error) -> LearningStoreError {
    tracing::error!(%error, "learning repository operation failed");
    LearningStoreError::Unavailable
}

#[cfg(test)]
mod tests {
    use super::*;
    use lumi_core::{
        LearningAnswer, LearningAnswerSpec, LearningHint, LearningOption, ReadingCompletionTrigger,
        SelfCheckRating,
    };

    fn context(
        user_id: UserId,
        material_id: MaterialId,
        revision_id: DocumentRevisionId,
    ) -> LearningMaterialContext {
        LearningMaterialContext {
            space_id: Uuid::now_v7(),
            owner_user_id: user_id,
            material_id,
            active_revision_id: revision_id,
            source_hash: "source-hash".to_owned(),
            title: "Fixture".to_owned(),
            content_unit_ids: HashSet::from(["chapter-1".to_owned()]),
        }
    }

    #[tokio::test]
    async fn retry_does_not_duplicate_completion() -> Result<(), LearningStoreError> {
        let runtime = LearningRuntime::memory();
        let user_id = Uuid::now_v7();
        let material_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let command = CompleteReadingScopeCommand {
            material_id,
            revision_id,
            scope_kind: LearningScopeKind::ContentUnit,
            content_unit_id: Some("chapter-1".to_owned()),
            anchor: None,
            trigger: ReadingCompletionTrigger::ReaderBoundary,
        };

        let first = runtime
            .complete_reading(
                user_id,
                Uuid::now_v7(),
                Some(context(user_id, material_id, revision_id)),
                &command,
                "completion-key",
            )
            .await?;
        let second = runtime
            .complete_reading(
                user_id,
                Uuid::now_v7(),
                Some(context(user_id, material_id, revision_id)),
                &command,
                "completion-key",
            )
            .await?;

        assert_eq!(first.offer.completion.id, second.offer.completion.id);
        Ok(())
    }

    #[tokio::test]
    async fn session_snapshot_survives_item_revision() -> Result<(), LearningStoreError> {
        let runtime = LearningRuntime::memory();
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let material_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let completion = runtime
            .complete_reading(
                user_id,
                device_id,
                Some(context(user_id, material_id, revision_id)),
                &CompleteReadingScopeCommand {
                    material_id,
                    revision_id,
                    scope_kind: LearningScopeKind::ContentUnit,
                    content_unit_id: Some("chapter-1".to_owned()),
                    anchor: None,
                    trigger: ReadingCompletionTrigger::ReaderBoundary,
                },
                "completion",
            )
            .await?;
        let item = runtime
            .create_item(
                user_id,
                device_id,
                &CreateLearningItemCommand {
                    source_id: completion.offer.source.id,
                    kind: LearningItemKind::QuizSingleChoice,
                    status: LearningItemStatus::Active,
                    prompt: "Старая формулировка".to_owned(),
                    answer_spec: LearningAnswerSpec::SingleChoice {
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
                        correct_option_id: "a".to_owned(),
                    },
                    explanation: "Пояснение".to_owned(),
                    hints: Vec::new(),
                    source_anchor: None,
                },
                "item",
            )
            .await?;
        let session = runtime
            .create_session(
                user_id,
                device_id,
                CreateLearningSessionCommand {
                    source_id: completion.offer.source.id,
                    kind: LearningSessionKind::ImmediateRecall,
                },
                "session",
            )
            .await?;
        runtime
            .update_item(
                user_id,
                device_id,
                item.id,
                &UpdateLearningItemCommand {
                    expected_revision: item.object_revision,
                    prompt: "Новая формулировка".to_owned(),
                    answer_spec: item.current_revision.answer_spec,
                    explanation: "Новое пояснение".to_owned(),
                    hints: Vec::new(),
                    source_anchor: None,
                },
                "item-update",
            )
            .await?;
        let restored = runtime.get_session(user_id, session.id).await?;
        let evaluation_revision = runtime
            .session_item_revision(user_id, session.id, item.id)
            .await?;

        assert_eq!(restored.items[0].prompt, "Старая формулировка");
        assert_eq!(evaluation_revision.prompt, "Старая формулировка");
        assert_eq!(evaluation_revision.explanation, "Пояснение");
        Ok(())
    }

    #[tokio::test]
    async fn ordered_hint_becomes_attempt_and_schedule_evidence() -> Result<(), LearningStoreError>
    {
        let runtime = LearningRuntime::memory();
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let material_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let completion = runtime
            .complete_reading(
                user_id,
                device_id,
                Some(context(user_id, material_id, revision_id)),
                &CompleteReadingScopeCommand {
                    material_id,
                    revision_id,
                    scope_kind: LearningScopeKind::ContentUnit,
                    content_unit_id: Some("chapter-1".to_owned()),
                    anchor: None,
                    trigger: ReadingCompletionTrigger::ReaderBoundary,
                },
                "hint-completion",
            )
            .await?;
        let item = runtime
            .create_item(
                user_id,
                device_id,
                &CreateLearningItemCommand {
                    source_id: completion.offer.source.id,
                    kind: LearningItemKind::HintedQuestion,
                    status: LearningItemStatus::Active,
                    prompt: "Что хранит schedule?".to_owned(),
                    answer_spec: LearningAnswerSpec::HintedSelfCheck {
                        sample_answer: "Версию алгоритма и memory state.".to_owned(),
                    },
                    explanation: "Schedule остаётся заменяемой projection.".to_owned(),
                    hints: vec![
                        LearningHint {
                            position: 1,
                            text: "Вспомните versioning.".to_owned(),
                            source_anchor: None,
                        },
                        LearningHint {
                            position: 2,
                            text: "Нужен opaque payload.".to_owned(),
                            source_anchor: None,
                        },
                    ],
                    source_anchor: None,
                },
                "hint-item",
            )
            .await?;
        let session = runtime
            .create_session(
                user_id,
                device_id,
                CreateLearningSessionCommand {
                    source_id: completion.offer.source.id,
                    kind: LearningSessionKind::ImmediateRecall,
                },
                "hint-session",
            )
            .await?;
        runtime
            .transition_session(
                user_id,
                device_id,
                session.id,
                SessionTransition::Start,
                "hint-start",
            )
            .await?;
        let out_of_order = runtime
            .reveal_hint(
                user_id,
                session.id,
                RevealLearningHintCommand {
                    item_id: item.id,
                    position: 2,
                },
                "hint-two-first",
            )
            .await;
        assert!(matches!(out_of_order, Err(LearningStoreError::Conflict)));
        runtime
            .reveal_hint(
                user_id,
                session.id,
                RevealLearningHintCommand {
                    item_id: item.id,
                    position: 1,
                },
                "hint-one",
            )
            .await?;
        let attempt = runtime
            .submit_attempt(
                user_id,
                device_id,
                session.id,
                item.id,
                &SubmitLearningAttemptCommand {
                    answer: LearningAnswer::Text {
                        text: "Версия и state.".to_owned(),
                    },
                    self_check: Some(SelfCheckRating::Partial),
                    elapsed_ms: 100,
                    review_rating: Some(LearningReviewRating::Hard),
                },
                "hint-attempt",
            )
            .await?;
        let schedules = runtime.list_schedules(user_id, None).await?;

        assert_eq!(attempt.hints_used, vec![1]);
        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].algorithm_version, "fsrs-4.5-lumi-v1");
        runtime
            .set_source_paused(
                user_id,
                device_id,
                completion.offer.source.id,
                true,
                "pause-source",
            )
            .await?;
        assert_eq!(
            runtime.list_schedules(user_id, None).await?[0].state,
            LearningScheduleState::Paused
        );
        runtime
            .set_source_paused(
                user_id,
                device_id,
                completion.offer.source.id,
                false,
                "resume-source",
            )
            .await?;
        if let LearningBackend::Memory(data) = &runtime.backend {
            let mut data = lock_memory(data)?;
            let schedule = data
                .schedules
                .get_mut(&(user_id, item.id))
                .ok_or(LearningStoreError::NotFound)?;
            schedule.due_at = now_timestamp_ms().saturating_sub(1);
        }
        let today = runtime.today(user_id, None).await?;
        assert_eq!(today.counts.due, 1);
        assert_eq!(today.groups.len(), 1);
        runtime
            .update_settings(
                user_id,
                device_id,
                &UpdateLearningSettingsCommand {
                    expected_revision: 1,
                    scheduling_enabled: true,
                    daily_limit: 5,
                    manual_only: true,
                },
                "manual-only",
            )
            .await?;
        let manual_today = runtime.today(user_id, None).await?;
        assert_eq!(manual_today.counts.due, 1);
        assert!(manual_today.groups.is_empty());
        runtime
            .update_settings(
                user_id,
                device_id,
                &UpdateLearningSettingsCommand {
                    expected_revision: 2,
                    scheduling_enabled: true,
                    daily_limit: 5,
                    manual_only: false,
                },
                "automatic-today",
            )
            .await?;
        let scheduled = runtime
            .create_session(
                user_id,
                device_id,
                CreateLearningSessionCommand {
                    source_id: completion.offer.source.id,
                    kind: LearningSessionKind::ScheduledReview,
                },
                "scheduled-session",
            )
            .await?;
        runtime
            .transition_session(
                user_id,
                device_id,
                scheduled.id,
                SessionTransition::Start,
                "scheduled-start",
            )
            .await?;
        let snoozed = runtime
            .snooze_session(
                user_id,
                scheduled.id,
                SnoozeLearningSessionCommand { days: 1 },
                "snooze",
            )
            .await?;
        assert_eq!(snoozed.state, LearningSessionState::Abandoned);
        assert!(runtime.list_schedules(user_id, None).await?[0].due_at > now_timestamp_ms());
        Ok(())
    }

    #[tokio::test]
    async fn performance_learning_projection_session_and_attempt_fit_release_budget(
    ) -> Result<(), LearningStoreError> {
        let runtime = LearningRuntime::memory();
        let user_id = Uuid::now_v7();
        let device_id = Uuid::now_v7();
        let material_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let completion = runtime
            .complete_reading(
                user_id,
                device_id,
                Some(context(user_id, material_id, revision_id)),
                &CompleteReadingScopeCommand {
                    material_id,
                    revision_id,
                    scope_kind: LearningScopeKind::ContentUnit,
                    content_unit_id: Some("chapter-1".to_owned()),
                    anchor: None,
                    trigger: ReadingCompletionTrigger::ReaderBoundary,
                },
                "performance-completion",
            )
            .await?;
        for index in 0..100 {
            runtime
                .create_item(
                    user_id,
                    device_id,
                    &CreateLearningItemCommand {
                        source_id: completion.offer.source.id,
                        kind: LearningItemKind::QuizTrueFalse,
                        status: LearningItemStatus::Active,
                        prompt: format!("Performance item {index}"),
                        answer_spec: LearningAnswerSpec::TrueFalse { correct: true },
                        explanation: "Bounded fixture.".to_owned(),
                        hints: Vec::new(),
                        source_anchor: None,
                    },
                    &format!("performance-item-{index}"),
                )
                .await?;
        }

        let session_started = std::time::Instant::now();
        let learning_session = runtime
            .create_session(
                user_id,
                device_id,
                CreateLearningSessionCommand {
                    source_id: completion.offer.source.id,
                    kind: LearningSessionKind::ManualPractice,
                },
                "performance-session",
            )
            .await?;
        let session_elapsed = session_started.elapsed();
        runtime
            .transition_session(
                user_id,
                device_id,
                learning_session.id,
                SessionTransition::Start,
                "performance-start",
            )
            .await?;
        let first_item = learning_session
            .items
            .first()
            .ok_or(LearningStoreError::NotFound)?;
        let attempt_started = std::time::Instant::now();
        runtime
            .submit_attempt(
                user_id,
                device_id,
                learning_session.id,
                first_item.item_id,
                &SubmitLearningAttemptCommand {
                    answer: LearningAnswer::TrueFalse { value: true },
                    self_check: None,
                    elapsed_ms: 250,
                    review_rating: Some(LearningReviewRating::Good),
                },
                "performance-attempt",
            )
            .await?;
        let attempt_elapsed = attempt_started.elapsed();
        let projection_started = std::time::Instant::now();
        let today = runtime.today(user_id, None).await?;
        let projection_elapsed = projection_started.elapsed();

        assert_eq!(today.counts.ready, 99);
        if std::env::var_os("LUMI_PERFORMANCE").is_some() {
            assert!(session_elapsed < std::time::Duration::from_millis(100));
            assert!(attempt_elapsed < std::time::Duration::from_millis(100));
            assert!(projection_elapsed < std::time::Duration::from_millis(100));
        }
        Ok(())
    }

    #[tokio::test]
    async fn unanswered_items_are_not_graded_on_completion() -> Result<(), LearningStoreError> {
        let _unused_answer = LearningAnswer::TrueFalse { value: true };
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
