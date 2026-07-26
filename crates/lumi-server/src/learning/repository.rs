//! Durable and in-memory repositories behind one learning application service.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use lumi_core::{
    grade_learning_answer, now_timestamp_ms, ChangeLearningItemStatusCommand,
    CompleteReadingResponse, CompleteReadingScopeCommand, CreateLearningItemCommand,
    CreateLearningSessionCommand, DocumentRevisionId, LearningAttempt, LearningFeedback,
    LearningItem, LearningItemId, LearningItemKind, LearningItemOrigin, LearningItemPage,
    LearningItemRevision, LearningItemStatus, LearningOffer, LearningOfferAction,
    LearningScopeKind, LearningSession, LearningSessionId, LearningSessionItem,
    LearningSessionKind, LearningSessionState, LearningSource, LearningSourceId,
    LearningValidationError, MaterialId, ReadingCompletion, RecordLearningSourceOpenedCommand,
    SubmitLearningAttemptCommand, UpdateLearningItemCommand, UpdateLearningOfferCommand, UserId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx_core::{query::query, query_scalar::query_scalar, row::Row};
use sqlx_postgres::{PgPool, Postgres};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

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
}

impl LearningRuntime {
    pub(crate) fn memory() -> Self {
        Self {
            backend: LearningBackend::Memory(Arc::new(Mutex::new(MemoryLearningData::default()))),
        }
    }

    pub(crate) fn postgres(pool: PgPool) -> Self {
        Self {
            backend: LearningBackend::Postgres(pool),
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
    data.sessions
        .get(&session_id)
        .filter(|record| record.session.user_id == user_id)
        .map(|record| record.session.clone())
        .ok_or(LearningStoreError::NotFound)
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

fn memory_submit_attempt(
    data: &mut MemoryLearningData,
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
        created_at: now_timestamp_ms(),
    };
    record.session.attempts.push(attempt.clone());
    record.session.updated_at = attempt.created_at;
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
         ORDER BY li.item_id
         LIMIT 7",
    )
    .bind(user_id)
    .bind(command.source_id)
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
    let items = item_rows
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

async fn pg_submit_attempt(
    pool: &PgPool,
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
        "SELECT ls.state, ls.space_id, lsi.item_revision_id, lsi.snapshot
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
    use lumi_core::{LearningAnswer, LearningAnswerSpec, LearningOption, ReadingCompletionTrigger};

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
                    source_anchor: None,
                },
                "item-update",
            )
            .await?;
        let restored = runtime.get_session(user_id, session.id).await?;

        assert_eq!(restored.items[0].prompt, "Старая формулировка");
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
