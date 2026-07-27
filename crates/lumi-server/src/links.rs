//! Owner-scoped wikilink resolution and rebuildable backlink projection.

use axum::{
    extract::{Extension, Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use lumi_core::{
    extract_wikilinks, resolve_link_candidates, Anchor, AnnotationBacklink, AnnotationKind,
    AnnotationLink, AnnotationLinkState, LinkTarget, LinkTargetType, ResolveAnnotationLinkCommand,
    TextRange, WikilinkReference,
};
use serde::{Deserialize, Serialize};
use sqlx_core::{query::query, row::Row, transaction::Transaction};
use sqlx_postgres::{PgPool, Postgres};
use uuid::Uuid;

use crate::{account::AuthenticatedSession, imports::ImportServiceError, AppError, AppState};

const MAX_LINK_SUGGESTIONS: usize = 24;

#[derive(Deserialize)]
struct SuggestQuery {
    q: String,
    material_id: Option<Uuid>,
}

pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/links/suggest", get(suggest_links))
        .route("/links/resolve", post(resolve_link))
        .route("/links/rebuild", post(rebuild_links))
        .route(
            "/links/backlinks/{object_type}/{object_id}",
            get(list_backlinks),
        )
        .route(
            "/materials/{material_id}/annotation-links",
            get(list_material_links),
        )
}

#[derive(Serialize)]
struct LinkRebuildResult {
    annotations_scanned: usize,
    links_projected: i64,
}

async fn rebuild_links(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<LinkRebuildResult>, AppError> {
    let pool = state
        .imports
        .as_ref()
        .ok_or(AppError::Unavailable("link resolver"))?
        .pool();
    let mut tx = pool.begin().await.map_err(log_db)?;
    let rows = query(
        "SELECT a.annotation_id, a.material_id, a.kind
           FROM annotations a
           JOIN sync_spaces s ON s.space_id = a.space_id
          WHERE s.owner_user_id = $1 AND a.deleted_at IS NULL
          ORDER BY a.annotation_id
          FOR UPDATE OF a",
    )
    .bind(session.user_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(log_db)?;
    for row in &rows {
        let kind: AnnotationKind = serde_json::from_value(row.try_get("kind").map_err(log_db)?)
            .map_err(|_| AppError::Unavailable("annotation link projection"))?;
        replace_annotation_links(
            &mut tx,
            session.user_id,
            row.try_get("annotation_id").map_err(log_db)?,
            row.try_get("material_id").map_err(log_db)?,
            match &kind {
                AnnotationKind::Note { body } => Some(body.as_str()),
                AnnotationKind::Highlight { .. } | AnnotationKind::VoiceNote { .. } => None,
            },
        )
        .await
        .map_err(crate::map_import_error)?;
    }
    let links_projected: i64 = sqlx_core::query_scalar::query_scalar(
        "SELECT count(*) FROM annotation_links WHERE owner_id = $1",
    )
    .bind(session.user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(log_db)?;
    tx.commit().await.map_err(log_db)?;
    Ok(Json(LinkRebuildResult {
        annotations_scanned: rows.len(),
        links_projected,
    }))
}

pub(crate) async fn replace_annotation_links(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    source_annotation_id: Uuid,
    material_id: Uuid,
    body: Option<&str>,
) -> Result<(), ImportServiceError> {
    let preserved_targets = query(
        "SELECT ordinal, raw_text, target_type, target_id, material_id
           FROM annotation_links
          WHERE source_annotation_id = $1 AND owner_id = $2 AND state = 'resolved'",
    )
    .bind(source_annotation_id)
    .bind(owner_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(|_| ImportServiceError::Unavailable)?;
    query("DELETE FROM annotation_links WHERE source_annotation_id = $1 AND owner_id = $2")
        .bind(source_annotation_id)
        .bind(owner_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
    let Some(body) = body else {
        return Ok(());
    };
    for (ordinal, reference) in extract_wikilinks(body).into_iter().enumerate() {
        let candidates = find_candidates(&mut **tx, owner_id, Some(material_id), &reference)
            .await
            .map_err(|_| ImportServiceError::Unavailable)?;
        let preserved = preserved_targets.iter().find_map(|row| {
            let previous_ordinal = row.try_get::<i16, _>("ordinal").ok()?;
            let previous_raw = row.try_get::<String, _>("raw_text").ok()?;
            if previous_ordinal != i16::try_from(ordinal).ok()?
                || previous_raw != reference.raw_text
            {
                return None;
            }
            let object_type = match row.try_get::<String, _>("target_type").ok()?.as_str() {
                "material" => LinkTargetType::Material,
                "annotation" => LinkTargetType::Annotation,
                "anchor" => LinkTargetType::Anchor,
                _ => return None,
            };
            let object_id = row.try_get::<Uuid, _>("target_id").ok()?;
            let target_material_id = row.try_get::<Option<Uuid>, _>("material_id").ok()?;
            candidates
                .iter()
                .find(|candidate| {
                    candidate.object_type == object_type
                        && candidate.object_id == object_id
                        && candidate.material_id == target_material_id
                })
                .cloned()
        });
        let resolution = preserved.map_or_else(
            || resolve_link_candidates(candidates),
            |target| lumi_core::LinkResolution {
                state: AnnotationLinkState::Resolved,
                target: Some(target),
                candidates: Vec::new(),
            },
        );
        let state = resolution.state;
        let target = resolution.target.as_ref();
        query(
            "INSERT INTO annotation_links
             (link_id, owner_id, source_annotation_id, ordinal, raw_text, target_text,
              heading, alias, display_path, state, target_type, target_id, material_id,
              anchor)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        )
        .bind(Uuid::now_v7())
        .bind(owner_id)
        .bind(source_annotation_id)
        .bind(i16::try_from(ordinal).map_err(|_| ImportServiceError::Unavailable)?)
        .bind(&reference.raw_text)
        .bind(&reference.target_text)
        .bind(reference.heading.as_deref())
        .bind(reference.alias.as_deref())
        .bind(display_input(&reference))
        .bind(state.as_str())
        .bind(target.map(|value| value.object_type.as_str()))
        .bind(target.map(|value| value.object_id))
        .bind(target.and_then(|value| value.material_id))
        .bind(
            target
                .and_then(|value| value.anchor.as_ref())
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| ImportServiceError::Unavailable)?,
        )
        .execute(&mut **tx)
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
    }
    Ok(())
}

pub(crate) async fn delete_annotation_links(
    tx: &mut Transaction<'_, Postgres>,
    owner_id: Uuid,
    annotation_id: Uuid,
) -> Result<(), ImportServiceError> {
    query("DELETE FROM annotation_links WHERE owner_id = $1 AND source_annotation_id = $2")
        .bind(owner_id)
        .bind(annotation_id)
        .execute(&mut **tx)
        .await
        .map_err(|_| ImportServiceError::Unavailable)?;
    Ok(())
}

async fn suggest_links(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(parameters): Query<SuggestQuery>,
) -> Result<Json<Vec<LinkTarget>>, AppError> {
    let query_text = parameters.q.trim();
    if query_text.is_empty() || query_text.len() > 1_000 {
        return Ok(Json(Vec::new()));
    }
    let reference = reference_from_query(query_text);
    let pool = state
        .imports
        .as_ref()
        .ok_or(AppError::Unavailable("link resolver"))?
        .pool();
    let mut candidates =
        find_suggestions(pool, session.user_id, parameters.material_id, &reference).await?;
    candidates.truncate(MAX_LINK_SUGGESTIONS);
    Ok(Json(candidates))
}

async fn resolve_link(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(command): Json<ResolveAnnotationLinkCommand>,
) -> Result<Json<AnnotationLink>, AppError> {
    let pool = state
        .imports
        .as_ref()
        .ok_or(AppError::Unavailable("link resolver"))?
        .pool();
    let mut tx = pool.begin().await.map_err(log_db)?;
    let row = query(
        "SELECT l.target_text, l.heading, a.material_id
           FROM annotation_links l
           JOIN annotations a ON a.annotation_id = l.source_annotation_id
          WHERE l.link_id = $1 AND l.owner_id = $2 AND a.deleted_at IS NULL
          FOR UPDATE",
    )
    .bind(command.link_id)
    .bind(session.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("annotation_link"))?;
    let reference = WikilinkReference {
        raw_text: String::new(),
        target_text: row.try_get("target_text").map_err(log_db)?,
        heading: row.try_get("heading").map_err(log_db)?,
        alias: None,
        byte_start: 0,
        byte_end: 0,
    };
    let material_id: Uuid = row.try_get("material_id").map_err(log_db)?;
    let candidates =
        find_candidates(&mut *tx, session.user_id, Some(material_id), &reference).await?;
    let selected = candidates.iter().find(|candidate| {
        candidate.object_type == command.target.object_type
            && candidate.object_id == command.target.object_id
            && candidate.material_id == command.target.material_id
    });
    let Some(selected) = selected else {
        return Err(AppError::BadRequest(
            "link target is not an accessible current candidate".to_owned(),
        ));
    };
    query(
        "UPDATE annotation_links
            SET state = 'resolved', target_type = $3, target_id = $4,
                material_id = $5, anchor = $6, display_path = $7, updated_at = now()
          WHERE link_id = $1 AND owner_id = $2",
    )
    .bind(command.link_id)
    .bind(session.user_id)
    .bind(selected.object_type.as_str())
    .bind(selected.object_id)
    .bind(selected.material_id)
    .bind(
        selected
            .anchor
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| AppError::Unavailable("link anchor"))?,
    )
    .bind(&selected.display_path)
    .execute(&mut *tx)
    .await
    .map_err(log_db)?;
    tx.commit().await.map_err(log_db)?;
    load_link(pool, session.user_id, command.link_id)
        .await
        .map(Json)
}

async fn list_material_links(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path(material_id): Path<Uuid>,
) -> Result<Json<Vec<AnnotationLink>>, AppError> {
    let pool = state
        .imports
        .as_ref()
        .ok_or(AppError::Unavailable("link resolver"))?
        .pool();
    let owned: bool = sqlx_core::query_scalar::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM materials
             WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL
        )",
    )
    .bind(material_id)
    .bind(session.user_id)
    .fetch_one(pool)
    .await
    .map_err(log_db)?;
    if !owned {
        return Err(AppError::NotFound("material"));
    }
    let ids: Vec<Uuid> = sqlx_core::query_scalar::query_scalar(
        "SELECT l.link_id
           FROM annotation_links l
           JOIN annotations a ON a.annotation_id = l.source_annotation_id
          WHERE l.owner_id = $1 AND a.material_id = $2 AND a.deleted_at IS NULL
          ORDER BY a.updated_at DESC, l.ordinal",
    )
    .bind(session.user_id)
    .bind(material_id)
    .fetch_all(pool)
    .await
    .map_err(log_db)?;
    let mut links = Vec::with_capacity(ids.len());
    for id in ids {
        links.push(load_link(pool, session.user_id, id).await?);
    }
    Ok(Json(links))
}

async fn list_backlinks(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Path((object_type, object_id)): Path<(String, Uuid)>,
) -> Result<Json<Vec<AnnotationBacklink>>, AppError> {
    let target_type = parse_target_type(&object_type)?;
    let pool = state
        .imports
        .as_ref()
        .ok_or(AppError::Unavailable("link resolver"))?
        .pool();
    ensure_target_owned(pool, session.user_id, target_type, object_id).await?;
    let rows = query(
        "SELECT l.link_id, l.source_annotation_id, a.material_id, l.raw_text,
                concat(coalesce(m.title_override, m.canonical_title), ' / Записи / ',
                       coalesce(a.title, 'Запись ' || left(a.annotation_id::text, 8)))
                   AS source_display_path
           FROM annotation_links l
           JOIN annotations a ON a.annotation_id = l.source_annotation_id
           JOIN materials m ON m.material_id = a.material_id
          WHERE l.owner_id = $1 AND l.state = 'resolved'
            AND l.target_type = $2 AND l.target_id = $3
            AND a.deleted_at IS NULL AND m.deleted_at IS NULL
          ORDER BY a.updated_at DESC, l.link_id",
    )
    .bind(session.user_id)
    .bind(target_type.as_str())
    .bind(object_id)
    .fetch_all(pool)
    .await
    .map_err(log_db)?;
    let backlinks = rows
        .into_iter()
        .map(|row| {
            Ok(AnnotationBacklink {
                link_id: row.try_get("link_id").map_err(log_db)?,
                source_annotation_id: row.try_get("source_annotation_id").map_err(log_db)?,
                source_material_id: row.try_get("material_id").map_err(log_db)?,
                source_display_path: row.try_get("source_display_path").map_err(log_db)?,
                raw_text: row.try_get("raw_text").map_err(log_db)?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(backlinks))
}

async fn load_link(
    pool: &PgPool,
    owner_id: Uuid,
    link_id: Uuid,
) -> Result<AnnotationLink, AppError> {
    let row = query(
        "SELECT l.link_id, l.source_annotation_id, l.raw_text, l.target_text,
                l.heading, l.display_path, l.state, l.target_type, l.target_id,
                l.material_id, l.anchor, a.material_id AS source_material_id
           FROM annotation_links l
           JOIN annotations a ON a.annotation_id = l.source_annotation_id
          WHERE l.link_id = $1 AND l.owner_id = $2 AND a.deleted_at IS NULL",
    )
    .bind(link_id)
    .bind(owner_id)
    .fetch_optional(pool)
    .await
    .map_err(log_db)?
    .ok_or(AppError::NotFound("annotation_link"))?;
    let state = parse_link_state(&row.try_get::<String, _>("state").map_err(log_db)?)?;
    let source_material_id: Uuid = row.try_get("source_material_id").map_err(log_db)?;
    let reference = WikilinkReference {
        raw_text: row.try_get("raw_text").map_err(log_db)?,
        target_text: row.try_get("target_text").map_err(log_db)?,
        heading: row.try_get("heading").map_err(log_db)?,
        alias: None,
        byte_start: 0,
        byte_end: 0,
    };
    let target = if state == AnnotationLinkState::Resolved {
        let object_type =
            parse_target_type(&row.try_get::<String, _>("target_type").map_err(log_db)?)?;
        let object_id = row.try_get("target_id").map_err(log_db)?;
        Some(
            current_target(
                pool,
                owner_id,
                object_type,
                object_id,
                row.try_get("material_id").map_err(log_db)?,
                row.try_get("anchor").map_err(log_db)?,
            )
            .await?,
        )
    } else {
        None
    };
    let candidates = if state == AnnotationLinkState::Ambiguous {
        find_candidates(pool, owner_id, Some(source_material_id), &reference).await?
    } else {
        Vec::new()
    };
    let display_path = target.as_ref().map_or_else(
        || display_input(&reference),
        |value| value.display_path.clone(),
    );
    Ok(AnnotationLink {
        id: link_id,
        source_annotation_id: row.try_get("source_annotation_id").map_err(log_db)?,
        raw_text: reference.raw_text,
        display_path,
        state,
        target,
        candidates,
    })
}

async fn find_suggestions<'e, E>(
    executor: E,
    owner_id: Uuid,
    material_id: Option<Uuid>,
    reference: &WikilinkReference,
) -> Result<Vec<LinkTarget>, AppError>
where
    E: sqlx_core::executor::Executor<'e, Database = Postgres>,
{
    let needle = reference.target_text.trim();
    let rows = query(
        "SELECT 'material' AS target_type, m.material_id AS target_id,
                m.material_id, m.active_revision_id,
                coalesce(m.title_override, m.canonical_title) AS title,
                CASE WHEN m.material_id = $3 THEN 0 ELSE 1 END AS context_rank
           FROM materials m
          WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL
            AND position(lower($2) in lower(coalesce(m.title_override, m.canonical_title))) > 0
         UNION ALL
         SELECT 'annotation', a.annotation_id, a.material_id, m.active_revision_id,
                concat(coalesce(m.title_override, m.canonical_title), ' / Записи / ', a.title),
                CASE WHEN a.material_id = $3 THEN 0 ELSE 1 END
           FROM annotations a
           JOIN materials m ON m.material_id = a.material_id
          WHERE m.owner_user_id = $1 AND a.deleted_at IS NULL AND m.deleted_at IS NULL
            AND a.title IS NOT NULL
            AND position(lower($2) in lower(a.title)) > 0
          ORDER BY context_rank, title, target_id
          LIMIT 24",
    )
    .bind(owner_id)
    .bind(needle)
    .bind(material_id)
    .fetch_all(executor)
    .await
    .map_err(log_db)?;
    rows.into_iter()
        .map(|row| target_from_search_row(&row))
        .collect()
}

async fn find_candidates<'e, E>(
    executor: E,
    owner_id: Uuid,
    material_id: Option<Uuid>,
    reference: &WikilinkReference,
) -> Result<Vec<LinkTarget>, AppError>
where
    E: sqlx_core::executor::Executor<'e, Database = Postgres>,
{
    if reference.heading.is_some() {
        return find_anchor_candidates(executor, owner_id, reference).await;
    }
    let input = reference.target_text.trim();
    let short_name = input
        .rsplit('/')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(input);
    let rows = query(
        "SELECT 'material' AS target_type, m.material_id AS target_id,
                m.material_id, m.active_revision_id,
                coalesce(m.title_override, m.canonical_title) AS title,
                CASE WHEN m.material_id = $4 THEN 0 ELSE 1 END AS context_rank
           FROM materials m
          WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL
            AND lower(coalesce(m.title_override, m.canonical_title)) = lower($2)
         UNION ALL
         SELECT 'annotation', a.annotation_id, a.material_id, m.active_revision_id,
                concat(coalesce(m.title_override, m.canonical_title), ' / Записи / ', a.title),
                CASE WHEN a.material_id = $4 THEN 0 ELSE 1 END
           FROM annotations a
           JOIN materials m ON m.material_id = a.material_id
          WHERE m.owner_user_id = $1 AND a.deleted_at IS NULL AND m.deleted_at IS NULL
            AND a.title IS NOT NULL AND lower(a.title) = lower($3)
          ORDER BY context_rank, title, target_id",
    )
    .bind(owner_id)
    .bind(input)
    .bind(short_name)
    .bind(material_id)
    .fetch_all(executor)
    .await
    .map_err(log_db)?;
    rows.into_iter()
        .map(|row| target_from_search_row(&row))
        .collect()
}

async fn find_anchor_candidates<'e, E>(
    executor: E,
    owner_id: Uuid,
    reference: &WikilinkReference,
) -> Result<Vec<LinkTarget>, AppError>
where
    E: sqlx_core::executor::Executor<'e, Database = Postgres>,
{
    let rows = query(
        "SELECT m.material_id, m.active_revision_id,
                coalesce(m.title_override, m.canonical_title) AS title, p.payload
           FROM materials m
           JOIN normalized_packages p ON p.revision_id = m.active_revision_id
           JOIN document_revisions r ON r.revision_id = m.active_revision_id
          WHERE m.owner_user_id = $1 AND m.deleted_at IS NULL
            AND r.source_format <> 'pdf'
            AND lower(coalesce(m.title_override, m.canonical_title)) = lower($2)",
    )
    .bind(owner_id)
    .bind(reference.target_text.trim())
    .fetch_all(executor)
    .await
    .map_err(log_db)?;
    let heading = reference.heading.as_deref().unwrap_or_default();
    let mut targets = Vec::new();
    for row in rows {
        let package: lumi_core::NormalizedContentPackage =
            serde_json::from_value(row.try_get("payload").map_err(log_db)?)
                .map_err(|_| AppError::Unavailable("normalized package"))?;
        let navigation = package
            .navigation
            .iter()
            .find(|item| item.label.eq_ignore_ascii_case(heading));
        let Some(navigation) = navigation else {
            continue;
        };
        let Some(block) = package
            .blocks
            .iter()
            .find(|block| block.node_path == navigation.target_path)
        else {
            continue;
        };
        let quote = block.text.clone().unwrap_or_default();
        let end = quote.chars().count();
        if end == 0 {
            continue;
        }
        let revision_id: Uuid = row.try_get("active_revision_id").map_err(log_db)?;
        let anchor = Anchor {
            revision_id,
            node_path: block.node_path.clone(),
            end_node_path: block.node_path.clone(),
            text_range: Some(TextRange { start: 0, end }),
            quote,
            prefix: String::new(),
            suffix: String::new(),
            content_hash: block.content_hash.clone(),
            source_locator: Some(block.source_locator.clone()),
            end_source_locator: Some(block.source_locator.clone()),
            page_rects: Vec::new(),
        };
        let title: String = row.try_get("title").map_err(log_db)?;
        targets.push(LinkTarget {
            object_type: LinkTargetType::Anchor,
            object_id: revision_id,
            material_id: Some(row.try_get("material_id").map_err(log_db)?),
            anchor: Some(anchor),
            display_path: format!("{title}#{heading}"),
        });
    }
    Ok(targets)
}

fn target_from_search_row(row: &sqlx_postgres::PgRow) -> Result<LinkTarget, AppError> {
    let object_type = parse_target_type(&row.try_get::<String, _>("target_type").map_err(log_db)?)?;
    Ok(LinkTarget {
        object_type,
        object_id: row.try_get("target_id").map_err(log_db)?,
        material_id: Some(row.try_get("material_id").map_err(log_db)?),
        anchor: None,
        display_path: row.try_get("title").map_err(log_db)?,
    })
}

async fn current_target(
    pool: &PgPool,
    owner_id: Uuid,
    object_type: LinkTargetType,
    object_id: Uuid,
    material_id: Option<Uuid>,
    anchor: Option<serde_json::Value>,
) -> Result<LinkTarget, AppError> {
    let display_path: String = match object_type {
        LinkTargetType::Material => sqlx_core::query_scalar::query_scalar(
            "SELECT coalesce(title_override, canonical_title)
               FROM materials
              WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL",
        )
        .bind(object_id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await
        .map_err(log_db)?
        .ok_or(AppError::NotFound("link_target"))?,
        LinkTargetType::Annotation => sqlx_core::query_scalar::query_scalar(
            "SELECT concat(coalesce(m.title_override, m.canonical_title),
                           ' / Записи / ',
                           coalesce(a.title, 'Запись ' || left(a.annotation_id::text, 8)))
               FROM annotations a
               JOIN materials m ON m.material_id = a.material_id
              WHERE a.annotation_id = $1 AND m.owner_user_id = $2
                AND a.deleted_at IS NULL AND m.deleted_at IS NULL",
        )
        .bind(object_id)
        .bind(owner_id)
        .fetch_optional(pool)
        .await
        .map_err(log_db)?
        .ok_or(AppError::NotFound("link_target"))?,
        LinkTargetType::Anchor => {
            let title: String = sqlx_core::query_scalar::query_scalar(
                "SELECT coalesce(title_override, canonical_title)
                   FROM materials
                  WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL",
            )
            .bind(material_id.ok_or(AppError::NotFound("link_target"))?)
            .bind(owner_id)
            .fetch_optional(pool)
            .await
            .map_err(log_db)?
            .ok_or(AppError::NotFound("link_target"))?;
            let anchor_value = anchor.as_ref().ok_or(AppError::NotFound("link_target"))?;
            let parsed: Anchor = serde_json::from_value(anchor_value.clone())
                .map_err(|_| AppError::Unavailable("link anchor"))?;
            format!("{title}#{}", parsed.quote)
        }
    };
    Ok(LinkTarget {
        object_type,
        object_id,
        material_id,
        anchor: anchor
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| AppError::Unavailable("link anchor"))?,
        display_path,
    })
}

async fn ensure_target_owned(
    pool: &PgPool,
    owner_id: Uuid,
    object_type: LinkTargetType,
    object_id: Uuid,
) -> Result<(), AppError> {
    let owned: bool = match object_type {
        LinkTargetType::Material => sqlx_core::query_scalar::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM materials
                 WHERE material_id = $1 AND owner_user_id = $2 AND deleted_at IS NULL
            )",
        )
        .bind(object_id)
        .bind(owner_id)
        .fetch_one(pool)
        .await
        .map_err(log_db)?,
        LinkTargetType::Annotation => sqlx_core::query_scalar::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM annotations a
                JOIN materials m ON m.material_id = a.material_id
                 WHERE a.annotation_id = $1 AND m.owner_user_id = $2
                   AND a.deleted_at IS NULL AND m.deleted_at IS NULL
            )",
        )
        .bind(object_id)
        .bind(owner_id)
        .fetch_one(pool)
        .await
        .map_err(log_db)?,
        LinkTargetType::Anchor => sqlx_core::query_scalar::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM document_revisions r
                JOIN materials m ON m.material_id = r.material_id
                 WHERE r.revision_id = $1 AND m.owner_user_id = $2
                   AND m.deleted_at IS NULL
            )",
        )
        .bind(object_id)
        .bind(owner_id)
        .fetch_one(pool)
        .await
        .map_err(log_db)?,
    };
    if owned {
        Ok(())
    } else {
        Err(AppError::NotFound("link_target"))
    }
}

fn reference_from_query(value: &str) -> WikilinkReference {
    let (target_text, heading) =
        value
            .split_once('#')
            .map_or((value.trim(), None), |(target, heading)| {
                let heading = heading.trim();
                (
                    target.trim(),
                    (!heading.is_empty()).then(|| heading.to_owned()),
                )
            });
    WikilinkReference {
        raw_text: format!("[[{value}]]"),
        target_text: target_text.to_owned(),
        heading,
        alias: None,
        byte_start: 0,
        byte_end: value.len() + 4,
    }
}

fn display_input(reference: &WikilinkReference) -> String {
    reference.heading.as_ref().map_or_else(
        || reference.target_text.clone(),
        |heading| format!("{}#{heading}", reference.target_text),
    )
}

fn parse_target_type(value: &str) -> Result<LinkTargetType, AppError> {
    match value {
        "material" => Ok(LinkTargetType::Material),
        "annotation" => Ok(LinkTargetType::Annotation),
        "anchor" => Ok(LinkTargetType::Anchor),
        _ => Err(AppError::BadRequest(
            "unsupported link target type".to_owned(),
        )),
    }
}

fn parse_link_state(value: &str) -> Result<AnnotationLinkState, AppError> {
    match value {
        "resolved" => Ok(AnnotationLinkState::Resolved),
        "ambiguous" => Ok(AnnotationLinkState::Ambiguous),
        "unresolved" => Ok(AnnotationLinkState::Unresolved),
        _ => Err(AppError::Unavailable("annotation link state")),
    }
}

fn log_db(error: sqlx_core::error::Error) -> AppError {
    tracing::error!(error = %error, "link persistence operation failed");
    AppError::Unavailable("link persistence")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_reference_keeps_optional_heading() {
        let reference = reference_from_query("Книга#Глава");

        assert_eq!(reference.target_text, "Книга");
        assert_eq!(reference.heading.as_deref(), Some("Глава"));
    }
}
