//! Rebuildable owner-scoped Desk projection and HTTP query boundary.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use lumi_core::{
    Anchor, DeskItem, DeskItemFilter, DeskItemPage, DeskLearningState, DeskMaterial,
    DeskMaterialPage, DeskObjectType, DeskRebuildReceipt, DeskSort, ImportedFixture, MaterialDesk,
    SearchOpenTarget, UserId, DESK_PROJECTION_VERSION, MAX_DESK_PAGE_SIZE,
};
use serde::Deserialize;
use serde_json::Value;
use sqlx_core::row::Row;
use sqlx_postgres::{PgPool, PgRow};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::account::AuthenticatedSession;
use crate::{AppError, AppState};

const DEFAULT_PAGE_SIZE: usize = 30;
const MAX_PREVIEW_CHARS: usize = 360;

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

/// PostgreSQL-backed or deterministic fixture Desk runtime.
pub(crate) struct DeskRuntime {
    pool: Option<PgPool>,
    fixture_materials: Vec<DeskMaterial>,
}

impl DeskRuntime {
    pub(crate) fn empty_memory() -> Self {
        Self {
            pool: None,
            fixture_materials: Vec::new(),
        }
    }

    pub(crate) fn memory(imported: &ImportedFixture) -> Self {
        Self {
            pool: None,
            fixture_materials: vec![DeskMaterial {
                material_id: imported.material.id,
                title: imported.material.display_title().to_owned(),
                record_count: 0,
                learning_count: 0,
                artifact_count: 0,
                attention_count: 0,
                last_activity_at: None,
                projection_generation: 1,
            }],
        }
    }

    pub(crate) fn postgres(pool: PgPool) -> Self {
        Self {
            pool: Some(pool),
            fixture_materials: Vec::new(),
        }
    }

    pub(crate) async fn list_materials(
        &self,
        owner_id: UserId,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<DeskMaterialPage, DeskError> {
        let limit = validate_limit(limit)?;
        let offset = decode_cursor(cursor)?;
        let generation = self.generation(owner_id).await?;
        if let Some(pool) = &self.pool {
            let rows = sqlx::query(
                "SELECT projection.material_id,
                        COALESCE(material.title_override, material.canonical_title) AS title,
                        projection.record_count,
                        projection.learning_count,
                        projection.artifact_count,
                        projection.attention_count,
                        projection.last_activity_at
                   FROM desk_material_projection projection
                   JOIN materials material
                     ON material.material_id = projection.material_id
                  WHERE projection.user_id = $1
                    AND material.owner_user_id = $1
                    AND material.deleted_at IS NULL
                  ORDER BY projection.last_activity_at DESC NULLS LAST,
                           lower(COALESCE(material.title_override, material.canonical_title)),
                           projection.material_id
                  LIMIT $2 OFFSET $3",
            )
            .bind(owner_id)
            .bind(i64::try_from(limit.saturating_add(1)).map_err(|_| DeskError::Invalid)?)
            .bind(i64::try_from(offset).map_err(|_| DeskError::Invalid)?)
            .fetch_all(pool)
            .await
            .map_err(storage)?;
            let mut items = rows
                .iter()
                .map(|row| material_from_row(row, generation))
                .collect::<Result<Vec<_>, _>>()?;
            let next_cursor = page_cursor(&mut items, limit, offset);
            return Ok(DeskMaterialPage {
                items,
                next_cursor,
                projection_version: DESK_PROJECTION_VERSION.to_owned(),
                projection_generation: generation,
            });
        }

        let mut items = self
            .fixture_materials
            .iter()
            .skip(offset)
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();
        let next_cursor = page_cursor(&mut items, limit, offset);
        Ok(DeskMaterialPage {
            items,
            next_cursor,
            projection_version: DESK_PROJECTION_VERSION.to_owned(),
            projection_generation: generation,
        })
    }

    pub(crate) async fn material(
        &self,
        owner_id: UserId,
        material_id: Uuid,
    ) -> Result<DeskMaterial, DeskError> {
        if let Some(pool) = &self.pool {
            let generation = self.generation(owner_id).await?;
            let row = sqlx::query(
                "SELECT projection.material_id,
                        COALESCE(material.title_override, material.canonical_title) AS title,
                        projection.record_count,
                        projection.learning_count,
                        projection.artifact_count,
                        projection.attention_count,
                        projection.last_activity_at
                   FROM desk_material_projection projection
                   JOIN materials material
                     ON material.material_id = projection.material_id
                  WHERE projection.user_id = $1
                    AND projection.material_id = $2
                    AND material.owner_user_id = $1
                    AND material.deleted_at IS NULL",
            )
            .bind(owner_id)
            .bind(material_id)
            .fetch_optional(pool)
            .await
            .map_err(storage)?
            .ok_or(DeskError::NotFound)?;
            return material_from_row(&row, generation);
        }
        self.fixture_materials
            .iter()
            .find(|material| material.material_id == material_id)
            .cloned()
            .ok_or(DeskError::NotFound)
    }

    pub(crate) async fn list_items(
        &self,
        owner_id: UserId,
        mut filter: DeskItemFilter,
    ) -> Result<DeskItemPage, DeskError> {
        filter
            .normalize_and_validate()
            .map_err(|_| DeskError::Invalid)?;
        let offset = decode_cursor(filter.cursor.as_deref())?;
        let generation = self.generation(owner_id).await?;
        let Some(pool) = &self.pool else {
            return Ok(DeskItemPage {
                items: Vec::new(),
                next_cursor: None,
                projection_version: DESK_PROJECTION_VERSION.to_owned(),
                projection_generation: generation,
            });
        };
        let object_types = (!filter.object_types.is_empty()).then(|| {
            filter
                .object_types
                .iter()
                .map(|value| value.as_str().to_owned())
                .collect::<Vec<_>>()
        });
        let learning_state = filter.learning_state.map(learning_state_token);
        let order = match filter.sort {
            DeskSort::Updated => "projection.updated_at DESC, projection.object_type, projection.object_id",
            DeskSort::Created => "projection.created_at DESC, projection.object_type, projection.object_id",
            DeskSort::MaterialTitle => "lower(COALESCE(material.title_override, material.canonical_title)), projection.source_order, projection.object_type, projection.object_id",
            DeskSort::SourceOrder => "projection.source_order, projection.object_type, projection.object_id",
        };
        let statement = format!(
            "SELECT projection.user_id, projection.object_type, projection.object_id,
                    projection.material_id, projection.item_kind, projection.status,
                    projection.tags, projection.learning_state, projection.due_at,
                    projection.attention, projection.object_revision,
                    projection.created_at, projection.updated_at,
                    COALESCE(material.title_override, material.canonical_title) AS material_title
               FROM desk_item_projection projection
               JOIN materials material ON material.material_id = projection.material_id
              WHERE projection.user_id = $1
                AND material.owner_user_id = $1
                AND material.deleted_at IS NULL
                AND ($2::uuid IS NULL OR projection.material_id = $2)
                AND ($3::text[] IS NULL OR projection.object_type = ANY($3))
                AND ($4::text IS NULL OR projection.status = $4)
                AND ($5::text IS NULL OR $5 = ANY(projection.tags))
                AND ($6::text IS NULL OR projection.learning_state = $6)
                AND (NOT $7 OR projection.attention)
              ORDER BY {order}
              LIMIT $8 OFFSET $9"
        );
        let rows = sqlx::query(&statement)
            .bind(owner_id)
            .bind(filter.material_id)
            .bind(object_types)
            .bind(filter.status.as_deref())
            .bind(filter.tag.as_deref())
            .bind(learning_state)
            .bind(filter.attention_only)
            .bind(i64::try_from(filter.limit.saturating_add(1)).map_err(|_| DeskError::Invalid)?)
            .bind(i64::try_from(offset).map_err(|_| DeskError::Invalid)?)
            .fetch_all(pool)
            .await
            .map_err(storage)?;
        let mut items = Vec::with_capacity(rows.len());
        for row in &rows {
            items.push(self.enrich_item(row).await?);
        }
        let next_cursor = page_cursor(&mut items, filter.limit, offset);
        Ok(DeskItemPage {
            items,
            next_cursor,
            projection_version: DESK_PROJECTION_VERSION.to_owned(),
            projection_generation: generation,
        })
    }

    pub(crate) async fn item(
        &self,
        owner_id: UserId,
        object_type: DeskObjectType,
        object_id: Uuid,
    ) -> Result<DeskItem, DeskError> {
        let Some(pool) = &self.pool else {
            return Err(DeskError::NotFound);
        };
        let row = sqlx::query(
            "SELECT projection.user_id, projection.object_type, projection.object_id,
                    projection.material_id, projection.item_kind, projection.status,
                    projection.tags, projection.learning_state, projection.due_at,
                    projection.attention, projection.object_revision,
                    projection.created_at, projection.updated_at,
                    COALESCE(material.title_override, material.canonical_title) AS material_title
               FROM desk_item_projection projection
               JOIN materials material ON material.material_id = projection.material_id
              WHERE projection.user_id = $1
                AND projection.object_type = $2
                AND projection.object_id = $3
                AND material.owner_user_id = $1
                AND material.deleted_at IS NULL",
        )
        .bind(owner_id)
        .bind(object_type.as_str())
        .bind(object_id)
        .fetch_optional(pool)
        .await
        .map_err(storage)?
        .ok_or(DeskError::NotFound)?;
        self.enrich_item(&row).await
    }

    pub(crate) async fn rebuild(&self, owner_id: UserId) -> Result<DeskRebuildReceipt, DeskError> {
        let Some(pool) = &self.pool else {
            return Ok(DeskRebuildReceipt {
                projection_generation: 1,
                projection_version: DESK_PROJECTION_VERSION.to_owned(),
            });
        };
        let row = sqlx::query("SELECT lumi_rebuild_desk_projection($1) AS generation")
            .bind(owner_id)
            .fetch_one(pool)
            .await
            .map_err(storage)?;
        Ok(DeskRebuildReceipt {
            projection_generation: row_generation(&row)?,
            projection_version: DESK_PROJECTION_VERSION.to_owned(),
        })
    }

    async fn generation(&self, owner_id: UserId) -> Result<u64, DeskError> {
        let Some(pool) = &self.pool else {
            return Ok(1);
        };
        let row = sqlx::query(
            "SELECT generation
               FROM desk_projection_state
              WHERE user_id = $1",
        )
        .bind(owner_id)
        .fetch_optional(pool)
        .await
        .map_err(storage)?;
        row.as_ref().map_or(Ok(0), row_generation)
    }

    async fn enrich_item(&self, row: &PgRow) -> Result<DeskItem, DeskError> {
        let object_type = parse_object_type(row.try_get("object_type").map_err(storage)?)?;
        let object_id: Uuid = row.try_get("object_id").map_err(storage)?;
        let material_id: Uuid = row.try_get("material_id").map_err(storage)?;
        let (title, preview, structural_path, attempt_count) = match object_type {
            DeskObjectType::Annotation => self.annotation_copy(object_id).await?,
            DeskObjectType::LearningItem => self.learning_copy(object_id).await?,
            DeskObjectType::AiArtifact => self.artifact_copy(object_id).await?,
        };
        Ok(DeskItem {
            object_type,
            object_id,
            material_id,
            material_title: row.try_get("material_title").map_err(storage)?,
            title,
            preview,
            item_kind: row.try_get("item_kind").map_err(storage)?,
            status: row.try_get("status").map_err(storage)?,
            tags: row.try_get("tags").map_err(storage)?,
            structural_path,
            learning_state: row
                .try_get::<Option<String>, _>("learning_state")
                .map_err(storage)?
                .as_deref()
                .map(parse_learning_state)
                .transpose()?,
            due_at: optional_time(row, "due_at")?,
            attempt_count,
            object_revision: positive_u64(row, "object_revision")?,
            created_at: required_time(row, "created_at")?,
            updated_at: required_time(row, "updated_at")?,
            attention: row.try_get("attention").map_err(storage)?,
            open_target: match object_type {
                DeskObjectType::Annotation => SearchOpenTarget::Annotation {
                    material_id,
                    annotation_id: object_id,
                },
                DeskObjectType::LearningItem => {
                    SearchOpenTarget::LearningItem { item_id: object_id }
                }
                DeskObjectType::AiArtifact => SearchOpenTarget::AiArtifact {
                    artifact_id: object_id,
                },
            },
        })
    }

    async fn annotation_copy(
        &self,
        object_id: Uuid,
    ) -> Result<(String, String, Vec<String>, u64), DeskError> {
        let pool = self.pool.as_ref().ok_or(DeskError::Storage)?;
        let row = sqlx::query(
            "SELECT title, annotation_type, kind, anchor
               FROM annotations
              WHERE annotation_id = $1
                AND deleted_at IS NULL",
        )
        .bind(object_id)
        .fetch_optional(pool)
        .await
        .map_err(storage)?
        .ok_or(DeskError::NotFound)?;
        let kind: Value = row.try_get("kind").map_err(storage)?;
        let anchor: Anchor = serde_json::from_value(row.try_get("anchor").map_err(storage)?)
            .map_err(|_| DeskError::Storage)?;
        let item_kind: String = row.try_get("annotation_type").map_err(storage)?;
        let title = row
            .try_get::<Option<String>, _>("title")
            .map_err(storage)?
            .unwrap_or_else(|| annotation_kind_label(&item_kind).to_owned());
        let preview = kind
            .get("body")
            .and_then(Value::as_str)
            .or_else(|| kind.get("transcript").and_then(Value::as_str))
            .unwrap_or(&anchor.quote);
        Ok((title, bounded_plain(preview), anchor.node_path, 0))
    }

    async fn learning_copy(
        &self,
        object_id: Uuid,
    ) -> Result<(String, String, Vec<String>, u64), DeskError> {
        let pool = self.pool.as_ref().ok_or(DeskError::Storage)?;
        let row = sqlx::query(
            "SELECT revision.payload,
                    source.anchor,
                    (SELECT count(*)
                       FROM learning_attempts attempt
                      WHERE attempt.item_id = item.item_id
                        AND attempt.user_id = item.owner_user_id) AS attempt_count
               FROM learning_items item
               JOIN learning_item_revisions revision
                 ON revision.item_revision_id = item.current_revision_id
               JOIN learning_sources source ON source.source_id = item.source_id
              WHERE item.item_id = $1
                AND item.status = 'active'
                AND item.deleted_at IS NULL",
        )
        .bind(object_id)
        .fetch_optional(pool)
        .await
        .map_err(storage)?
        .ok_or(DeskError::NotFound)?;
        let payload: Value = row.try_get("payload").map_err(storage)?;
        let prompt = payload
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("Учебный вопрос");
        let explanation = payload
            .get("explanation")
            .and_then(Value::as_str)
            .unwrap_or(prompt);
        let structural_path = row
            .try_get::<Option<Value>, _>("anchor")
            .map_err(storage)?
            .and_then(|anchor| serde_json::from_value::<Anchor>(anchor).ok())
            .map(|anchor| anchor.node_path)
            .unwrap_or_default();
        Ok((
            bounded_plain(prompt),
            bounded_plain(explanation),
            structural_path,
            positive_u64(&row, "attempt_count")?,
        ))
    }

    async fn artifact_copy(
        &self,
        object_id: Uuid,
    ) -> Result<(String, String, Vec<String>, u64), DeskError> {
        let pool = self.pool.as_ref().ok_or(DeskError::Storage)?;
        let row = sqlx::query(
            "SELECT kind, payload, scope_ref
               FROM ai_artifacts
              WHERE artifact_id = $1
                AND status = 'active'",
        )
        .bind(object_id)
        .fetch_optional(pool)
        .await
        .map_err(storage)?
        .ok_or(DeskError::NotFound)?;
        let kind: String = row.try_get("kind").map_err(storage)?;
        let payload: Value = row.try_get("payload").map_err(storage)?;
        let title = payload
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| artifact_kind_label(&kind).to_owned());
        let preview = ["markdown", "summary", "text", "content"]
            .iter()
            .find_map(|field| payload.get(field).and_then(Value::as_str))
            .map_or_else(|| payload.to_string(), str::to_owned);
        let structural_path = row
            .try_get::<Option<String>, _>("scope_ref")
            .map_err(storage)?
            .into_iter()
            .collect();
        Ok((title, bounded_plain(&preview), structural_path, 0))
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DeskError {
    #[error("invalid desk query")]
    Invalid,
    #[error("desk item not found")]
    NotFound,
    #[error("desk projection storage unavailable")]
    Storage,
}

/// Protected Desk query and rebuild routes.
pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/desk/materials", get(list_materials))
        .route("/desk/materials/{material_id}", get(get_material_desk))
        .route("/desk/items", get(list_items))
        .route("/desk/items/{object_type}/{object_id}", get(get_item))
        .route("/desk/rebuild", post(rebuild))
}

#[derive(Deserialize)]
struct PageQuery {
    cursor: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct ItemQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    material_id: Option<Uuid>,
    r#type: Option<String>,
    status: Option<String>,
    tag: Option<String>,
    learning_state: Option<String>,
    attention: Option<bool>,
    sort: Option<String>,
}

async fn list_materials(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(query): Query<PageQuery>,
) -> Result<Json<DeskMaterialPage>, AppError> {
    state
        .desk
        .list_materials(
            session.user_id,
            query.cursor.as_deref(),
            query.limit.unwrap_or(DEFAULT_PAGE_SIZE),
        )
        .await
        .map(Json)
        .map_err(map_desk_error)
}

async fn get_material_desk(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<Uuid>,
) -> Result<Json<MaterialDesk>, AppError> {
    let material = state
        .desk
        .material(session.user_id, material_id)
        .await
        .map_err(map_desk_error)?;
    let items = state
        .desk
        .list_items(
            session.user_id,
            DeskItemFilter {
                material_id: Some(material_id),
                limit: DEFAULT_PAGE_SIZE,
                ..DeskItemFilter::default()
            },
        )
        .await
        .map_err(map_desk_error)?;
    Ok(Json(MaterialDesk { material, items }))
}

async fn list_items(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(query): Query<ItemQuery>,
) -> Result<Json<DeskItemPage>, AppError> {
    let filter = item_filter(query)?;
    state
        .desk
        .list_items(session.user_id, filter)
        .await
        .map(Json)
        .map_err(map_desk_error)
}

async fn get_item(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((object_type, object_id)): Path<(String, Uuid)>,
) -> Result<Json<DeskItem>, AppError> {
    state
        .desk
        .item(
            session.user_id,
            parse_object_type(&object_type).map_err(map_desk_error)?,
            object_id,
        )
        .await
        .map(Json)
        .map_err(map_desk_error)
}

async fn rebuild(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<DeskRebuildReceipt>, AppError> {
    state
        .desk
        .rebuild(session.user_id)
        .await
        .map(Json)
        .map_err(map_desk_error)
}

fn item_filter(query: ItemQuery) -> Result<DeskItemFilter, AppError> {
    let object_types = query
        .r#type
        .as_deref()
        .map(|values| {
            values
                .split(',')
                .map(parse_object_type)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()
        .map_err(map_desk_error)?
        .unwrap_or_default();
    let learning_state = query
        .learning_state
        .as_deref()
        .map(parse_learning_state)
        .transpose()
        .map_err(map_desk_error)?;
    let sort = match query.sort.as_deref() {
        None | Some("updated") => DeskSort::Updated,
        Some("created") => DeskSort::Created,
        Some("material_title") => DeskSort::MaterialTitle,
        Some("source_order") => DeskSort::SourceOrder,
        Some(_) => return Err(AppError::BadRequest("invalid Desk sort".to_owned())),
    };
    Ok(DeskItemFilter {
        material_id: query.material_id,
        object_types,
        status: query.status,
        tag: query.tag,
        learning_state,
        attention_only: query.attention.unwrap_or(false),
        sort,
        cursor: query.cursor,
        limit: query.limit.unwrap_or(DEFAULT_PAGE_SIZE),
    })
}

fn material_from_row(row: &PgRow, generation: u64) -> Result<DeskMaterial, DeskError> {
    Ok(DeskMaterial {
        material_id: row.try_get("material_id").map_err(storage)?,
        title: row.try_get("title").map_err(storage)?,
        record_count: positive_u64(row, "record_count")?,
        learning_count: positive_u64(row, "learning_count")?,
        artifact_count: positive_u64(row, "artifact_count")?,
        attention_count: positive_u64(row, "attention_count")?,
        last_activity_at: optional_time(row, "last_activity_at")?,
        projection_generation: generation,
    })
}

fn parse_object_type(value: &str) -> Result<DeskObjectType, DeskError> {
    match value {
        "annotation" | "records" => Ok(DeskObjectType::Annotation),
        "learning_item" | "learning" => Ok(DeskObjectType::LearningItem),
        "ai_artifact" | "artifacts" => Ok(DeskObjectType::AiArtifact),
        _ => Err(DeskError::Invalid),
    }
}

fn parse_learning_state(value: &str) -> Result<DeskLearningState, DeskError> {
    match value {
        "scheduled" => Ok(DeskLearningState::Scheduled),
        "due" => Ok(DeskLearningState::Due),
        "missed" => Ok(DeskLearningState::Missed),
        "skipped" => Ok(DeskLearningState::Skipped),
        "completed" => Ok(DeskLearningState::Completed),
        _ => Err(DeskError::Invalid),
    }
}

fn learning_state_token(value: DeskLearningState) -> &'static str {
    match value {
        DeskLearningState::Scheduled => "scheduled",
        DeskLearningState::Due => "due",
        DeskLearningState::Missed => "missed",
        DeskLearningState::Skipped => "skipped",
        DeskLearningState::Completed => "completed",
    }
}

fn validate_limit(limit: usize) -> Result<usize, DeskError> {
    if limit == 0 || limit > MAX_DESK_PAGE_SIZE {
        return Err(DeskError::Invalid);
    }
    Ok(limit)
}

fn decode_cursor(cursor: Option<&str>) -> Result<usize, DeskError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    cursor
        .strip_prefix("v1:")
        .and_then(|value| value.parse().ok())
        .ok_or(DeskError::Invalid)
}

fn page_cursor<T>(items: &mut Vec<T>, limit: usize, offset: usize) -> Option<String> {
    let has_more = items.len() > limit;
    items.truncate(limit);
    has_more.then(|| format!("v1:{}", offset.saturating_add(limit)))
}

fn bounded_plain(value: &str) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let preview = chars.by_ref().take(MAX_PREVIEW_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

fn annotation_kind_label(value: &str) -> &'static str {
    match value {
        "highlight" => "Выделение",
        "note" => "Заметка",
        "margin_note" => "Запись на полях",
        "voice_note" => "Голосовая заметка",
        _ => "Запись",
    }
}

fn artifact_kind_label(value: &str) -> &'static str {
    match value {
        "summary_artifact" => "Сохранённое саммари",
        "abridgement_artifact" => "Сокращение",
        "question_set_artifact" => "Набор вопросов",
        "open_answer_evaluation" => "Разбор ответа",
        _ => "Сохранённый AI-артефакт",
    }
}

fn required_time(row: &PgRow, field: &str) -> Result<String, DeskError> {
    row.try_get::<OffsetDateTime, _>(field)
        .map(|value| value.to_string())
        .map_err(storage)
}

fn optional_time(row: &PgRow, field: &str) -> Result<Option<String>, DeskError> {
    row.try_get::<Option<OffsetDateTime>, _>(field)
        .map(|value| value.map(|value| value.to_string()))
        .map_err(storage)
}

fn positive_u64(row: &PgRow, field: &str) -> Result<u64, DeskError> {
    let value: i64 = row.try_get(field).map_err(storage)?;
    u64::try_from(value).map_err(|_| DeskError::Storage)
}

fn row_generation(row: &PgRow) -> Result<u64, DeskError> {
    positive_u64(row, "generation")
}

fn storage<T>(_error: T) -> DeskError {
    DeskError::Storage
}

fn map_desk_error(error: DeskError) -> AppError {
    match error {
        DeskError::Invalid => AppError::BadRequest("invalid Desk query".to_owned()),
        DeskError::NotFound => AppError::NotFound("Desk item"),
        DeskError::Storage => AppError::Internal("Desk projection"),
    }
}

#[cfg(test)]
mod tests {
    use lumi_core::rich_epub_fixture;

    use super::*;

    #[tokio::test]
    async fn memory_desk_lists_seeded_material() -> Result<(), Box<dyn std::error::Error>> {
        let imported = lumi_core::import_epub_fixture(Uuid::now_v7(), &rich_epub_fixture())?;
        let runtime = DeskRuntime::memory(&imported);

        let page = runtime
            .list_materials(imported.material.owner_id, None, 20)
            .await?;

        assert_eq!(
            page.items.first().map(|item| item.material_id),
            Some(imported.material.id)
        );
        Ok(())
    }

    #[test]
    fn cursor_rejects_wrong_version() {
        assert!(matches!(
            decode_cursor(Some("v2:20")),
            Err(DeskError::Invalid)
        ));
    }
}
