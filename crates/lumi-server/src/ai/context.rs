//! Permission-aware explicit source context without indexed retrieval.

use std::collections::{HashMap, HashSet};

use lumi_core::{
    content_hash, AiContextFragment, AiContextPack, AiMessageId, AiPermissionDecision,
    AiPermissionSnapshot, AiSourceLocator, AiSourceScope, Anchor, FixedLayoutContentPackage,
    NormalizedContentPackage, SourceCitation, SourceLocator, UserId,
    AI_CONTEXT_PACK_SCHEMA_VERSION, EXPLICIT_CONTEXT_LIMITS_VERSION,
    SOURCE_CITATION_SCHEMA_VERSION,
};
use serde_json::Value;
use sqlx_core::row::Row;
use sqlx_postgres::PgPool;
use sqlx_postgres::Postgres;
use thiserror::Error;
use uuid::Uuid;

mod sqlx {
    pub(crate) use sqlx_core::query::query;
}

const POLICY_VERSION: &str = "personal-source-owner.v1";

/// Explicit-context resolution failure safe for HTTP/application mapping.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SourceContextError {
    /// Material/revision is absent, foreign, deleted, or no longer active.
    #[error("source revision is unavailable")]
    NotFound,
    /// Exact selection no longer resolves to the immutable source.
    #[error("source selection is stale")]
    StaleSelection,
    /// Requested chapter or page does not exist.
    #[error("source scope is unavailable")]
    ScopeNotFound,
    /// Source has no usable text.
    #[error("source has no usable text layer")]
    MissingText,
    /// Context would violate the frozen hard bounds.
    #[error("source context exceeds the configured limits")]
    LimitExceeded,
    /// Persisted normalized source data is unavailable.
    #[error("source context repository is unavailable")]
    Storage,
}

/// Production resolver for deterministic selection/chapter/material context.
#[derive(Clone)]
pub struct SourceContextResolver {
    pool: PgPool,
}

impl SourceContextResolver {
    /// Build an owner-scoped resolver.
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Resolve and validate one immutable pack for a durable chat message.
    ///
    /// Authorization and exact active-revision checks happen in the same SQL
    /// read that obtains the normalized package. No foreign text is loaded
    /// into process memory.
    ///
    /// # Errors
    ///
    /// Returns a typed failure for foreign/stale sources, missing text, invalid
    /// scopes, corrupt packages, or frozen context-limit violations.
    pub async fn resolve_for_message(
        &self,
        owner_id: UserId,
        message_id: AiMessageId,
        scope: AiSourceScope,
    ) -> Result<AiContextPack, SourceContextError> {
        self.resolve(owner_id, ContextOwner::Message(message_id), scope)
            .await
    }

    /// Resolve one immutable pack for a durable AI task.
    ///
    /// The task id is also used as the stable citation namespace. The caller
    /// persists the pack through `PgAiRepository`, which rechecks exact task
    /// ownership and source binding.
    pub async fn resolve_for_task(
        &self,
        owner_id: UserId,
        task_id: Uuid,
        scope: AiSourceScope,
    ) -> Result<AiContextPack, SourceContextError> {
        self.resolve(owner_id, ContextOwner::Task(task_id), scope)
            .await
    }

    async fn resolve(
        &self,
        owner_id: UserId,
        context_owner: ContextOwner,
        scope: AiSourceScope,
    ) -> Result<AiContextPack, SourceContextError> {
        scope
            .validate()
            .map_err(|_| SourceContextError::StaleSelection)?;
        let row = sqlx::query(
            "SELECT m.space_id, r.source_format, p.payload \
             FROM materials AS m \
             JOIN document_revisions AS r \
               ON r.revision_id = m.active_revision_id \
              AND r.material_id = m.material_id \
              AND r.space_id = m.space_id \
             JOIN normalized_packages AS p ON p.revision_id = r.revision_id \
             WHERE m.material_id = $1 \
               AND m.owner_user_id = $2 \
               AND m.active_revision_id = $3 \
               AND m.deleted_at IS NULL \
               AND m.library_state <> 'deleted' \
               AND m.import_status = 'ready'",
        )
        .bind(scope.material_id())
        .bind(owner_id)
        .bind(scope.revision_id())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SourceContextError::Storage)?
        .ok_or(SourceContextError::NotFound)?;
        let source_format: String = row
            .try_get("source_format")
            .map_err(|_| SourceContextError::Storage)?;
        let payload: Value = row
            .try_get("payload")
            .map_err(|_| SourceContextError::Storage)?;
        let resolved = if source_format == "pdf" {
            let package: FixedLayoutContentPackage =
                serde_json::from_value(payload).map_err(|_| SourceContextError::Storage)?;
            resolve_pdf(&package, &scope)?
        } else {
            let package: NormalizedContentPackage =
                serde_json::from_value(payload).map_err(|_| SourceContextError::Storage)?;
            resolve_reflowable(&package, &scope)?
        };
        build_pack(owner_id, context_owner, scope, resolved)
    }

    /// Persist a chat-owned context pack after the message itself is durable.
    ///
    /// # Errors
    ///
    /// Returns an error if the pack is invalid, foreign, stale, duplicated, or
    /// cannot be written.
    pub async fn store_message_pack(&self, pack: &AiContextPack) -> Result<(), SourceContextError> {
        pack.validate()
            .map_err(|_| SourceContextError::LimitExceeded)?;
        let message_id = pack.message_id.ok_or(SourceContextError::Storage)?;
        let space_id: Uuid = sqlx_core::query_scalar::query_scalar(
            "SELECT m.space_id \
             FROM ai_messages AS message \
             JOIN ai_conversations AS conversation \
               ON conversation.conversation_id = message.conversation_id \
              AND conversation.user_id = message.user_id \
             JOIN materials AS m \
               ON m.material_id = $1 \
              AND m.owner_user_id = message.user_id \
             WHERE message.message_id = $2 \
               AND message.user_id = $3 \
               AND m.active_revision_id = $4 \
               AND m.deleted_at IS NULL",
        )
        .bind(pack.scope.material_id())
        .bind(message_id)
        .bind(pack.owner_id)
        .bind(pack.scope.revision_id())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| SourceContextError::Storage)?
        .ok_or(SourceContextError::NotFound)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| SourceContextError::Storage)?;
        Self::store_message_pack_in_transaction(&mut transaction, pack, space_id).await?;
        transaction
            .commit()
            .await
            .map_err(|_| SourceContextError::Storage)
    }

    pub(crate) async fn store_message_pack_in_transaction(
        transaction: &mut sqlx_core::transaction::Transaction<'_, Postgres>,
        pack: &AiContextPack,
        space_id: Uuid,
    ) -> Result<(), SourceContextError> {
        pack.validate()
            .map_err(|_| SourceContextError::LimitExceeded)?;
        let message_id = pack.message_id.ok_or(SourceContextError::Storage)?;
        let payload = serde_json::to_value(pack).map_err(|_| SourceContextError::Storage)?;
        let source_refs =
            serde_json::to_value(&pack.citations).map_err(|_| SourceContextError::Storage)?;
        sqlx::query(
            "INSERT INTO ai_context_packs (
                context_pack_id, user_id, space_id, task_id, message_id,
                schema_version, limits_version, source_material_id,
                source_revision_id, pack_hash, permission_snapshot,
                source_refs, payload
             ) VALUES ($1, $2, $3, NULL, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(pack.context_pack_id)
        .bind(pack.owner_id)
        .bind(space_id)
        .bind(message_id)
        .bind(&pack.schema_version)
        .bind(&pack.limits_version)
        .bind(pack.scope.material_id())
        .bind(pack.scope.revision_id())
        .bind(&pack.pack_hash)
        .bind(
            serde_json::to_value(&pack.permission_snapshot)
                .map_err(|_| SourceContextError::Storage)?,
        )
        .bind(source_refs)
        .bind(payload)
        .execute(&mut **transaction)
        .await
        .map_err(|_| SourceContextError::Storage)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ContextOwner {
    Message(Uuid),
    Task(Uuid),
}

impl ContextOwner {
    const fn id(self) -> Uuid {
        match self {
            Self::Message(id) | Self::Task(id) => id,
        }
    }

    const fn task_id(self) -> Option<Uuid> {
        match self {
            Self::Task(id) => Some(id),
            Self::Message(_) => None,
        }
    }

    const fn message_id(self) -> Option<Uuid> {
        match self {
            Self::Message(id) => Some(id),
            Self::Task(_) => None,
        }
    }
}

#[derive(Clone)]
struct ResolvedFragment {
    unit_id: String,
    block_id: String,
    label: Option<String>,
    text: String,
    locator: AiSourceLocator,
    anchor: Option<Box<Anchor>>,
    quote_start: usize,
    quote_end: usize,
}

fn resolve_reflowable(
    package: &NormalizedContentPackage,
    scope: &AiSourceScope,
) -> Result<Vec<ResolvedFragment>, SourceContextError> {
    if package.revision_id != scope.revision_id() {
        return Err(SourceContextError::NotFound);
    }
    let blocks_by_id = package
        .blocks
        .iter()
        .map(|block| (block.id.as_str(), block))
        .collect::<HashMap<_, _>>();
    let mut ordered = Vec::new();
    for unit in &package.units {
        for block_id in &unit.block_ids {
            if let Some(block) = blocks_by_id.get(block_id.as_str()) {
                ordered.push((unit, *block));
            }
        }
    }
    match scope {
        AiSourceScope::Selection { anchor, .. } => {
            let start = ordered
                .iter()
                .position(|(_, block)| block.node_path == anchor.node_path)
                .ok_or(SourceContextError::StaleSelection)?;
            let end = ordered
                .iter()
                .position(|(_, block)| block.node_path == anchor.effective_end_node_path())
                .ok_or(SourceContextError::StaleSelection)?;
            if end < start {
                return Err(SourceContextError::StaleSelection);
            }
            validate_reflowable_quote(&ordered, start, end, anchor)?;
            let from = start.saturating_sub(1);
            let to = (end + 1).min(ordered.len().saturating_sub(1));
            resolved_reflowable_range(&ordered[from..=to], Some((start - from, anchor)))
        }
        AiSourceScope::Chapter { scope_ref, .. } => {
            let unit = package
                .units
                .iter()
                .find(|unit| unit.id == *scope_ref)
                .ok_or(SourceContextError::ScopeNotFound)?;
            let selected = unit
                .block_ids
                .iter()
                .filter_map(|id| blocks_by_id.get(id.as_str()).map(|block| (unit, *block)))
                .collect::<Vec<_>>();
            resolved_reflowable_range(&selected, None)
        }
        AiSourceScope::Material { .. } => resolved_reflowable_range(&ordered, None),
    }
}

fn validate_reflowable_quote(
    ordered: &[(&lumi_core::ContentUnit, &lumi_core::ContentBlock)],
    start: usize,
    end: usize,
    anchor: &Anchor,
) -> Result<(), SourceContextError> {
    let range = anchor
        .text_range
        .ok_or(SourceContextError::StaleSelection)?;
    let mut parts = Vec::new();
    for (relative, (_, block)) in ordered[start..=end].iter().enumerate() {
        let text = block.text.as_deref().unwrap_or_default();
        let from = if relative == 0 { range.start } else { 0 };
        let to = if start + relative == end {
            range.end
        } else {
            text.chars().count()
        };
        if from > to || to > text.chars().count() {
            return Err(SourceContextError::StaleSelection);
        }
        parts.push(
            text.chars()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect::<String>(),
        );
    }
    if parts.join("\n") == anchor.quote {
        Ok(())
    } else {
        Err(SourceContextError::StaleSelection)
    }
}

fn resolved_reflowable_range(
    selected: &[(&lumi_core::ContentUnit, &lumi_core::ContentBlock)],
    selection: Option<(usize, &Anchor)>,
) -> Result<Vec<ResolvedFragment>, SourceContextError> {
    let mut resolved = Vec::new();
    for (index, (unit, block)) in selected.iter().enumerate() {
        let Some(text) = block.text.as_deref().filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        let text = truncate_utf8(text, AiContextPack::MAX_FRAGMENT_BYTES);
        let locator = source_locator(&block.source_locator)?;
        let selected_anchor = selection
            .filter(|(selection_index, _)| *selection_index == index)
            .map(|(_, anchor)| Box::new(anchor.clone()));
        let (quote_start, quote_end) = selected_anchor
            .as_ref()
            .and_then(|anchor| {
                text.find(&anchor.quote)
                    .map(|start| (start, start + anchor.quote.len()))
            })
            .unwrap_or((0, text.len()));
        resolved.push(ResolvedFragment {
            unit_id: unit.id.clone(),
            block_id: block.id.clone(),
            label: Some(unit.title.clone()),
            text,
            locator,
            anchor: selected_anchor,
            quote_start,
            quote_end,
        });
    }
    if resolved.is_empty() {
        Err(SourceContextError::MissingText)
    } else {
        Ok(resolved)
    }
}

fn resolve_pdf(
    package: &FixedLayoutContentPackage,
    scope: &AiSourceScope,
) -> Result<Vec<ResolvedFragment>, SourceContextError> {
    if package.revision_id != scope.revision_id() {
        return Err(SourceContextError::NotFound);
    }
    let selected_pages = match scope {
        AiSourceScope::Selection { anchor, .. } => {
            let locator = match anchor.source_locator.as_ref() {
                Some(SourceLocator::Pdf(locator)) => locator,
                _ => return Err(SourceContextError::StaleSelection),
            };
            vec![(locator.page_index, Some(anchor.as_ref()))]
        }
        AiSourceScope::Chapter { scope_ref, .. } => {
            let page = scope_ref
                .strip_prefix("page:")
                .unwrap_or(scope_ref)
                .parse::<u32>()
                .map_err(|_| SourceContextError::ScopeNotFound)?;
            vec![(page, None)]
        }
        AiSourceScope::Material { .. } => package
            .text_layers
            .iter()
            .map(|layer| (layer.page_index, None))
            .collect(),
    };
    let mut resolved = Vec::new();
    for (page_index, anchor) in selected_pages {
        let layer = package
            .text_layers
            .iter()
            .find(|layer| layer.page_index == page_index)
            .ok_or(SourceContextError::MissingText)?;
        let page_label = package
            .pages
            .iter()
            .find(|page| page.page_index == page_index)
            .map_or_else(
                || (page_index + 1).to_string(),
                |page| page.page_label.clone(),
            );
        let page_text = layer
            .blocks
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(anchor) = anchor {
            if anchor.quote.trim().is_empty() || !page_text.contains(&anchor.quote) {
                return Err(SourceContextError::StaleSelection);
            }
        }
        for block in &layer.blocks {
            if block.text.trim().is_empty() {
                continue;
            }
            let text = truncate_utf8(&block.text, AiContextPack::MAX_FRAGMENT_BYTES);
            let selected_anchor = anchor
                .filter(|anchor| text.contains(&anchor.quote))
                .map(|anchor| Box::new(anchor.clone()));
            let (quote_start, quote_end) = selected_anchor
                .as_ref()
                .and_then(|anchor| {
                    text.find(&anchor.quote)
                        .map(|start| (start, start + anchor.quote.len()))
                })
                .unwrap_or((0, text.len()));
            resolved.push(ResolvedFragment {
                unit_id: format!("page-{page_index}"),
                block_id: format!("page-{page_index}-block-{}", block.block_index),
                label: Some(format!("Страница {page_label}")),
                text,
                locator: AiSourceLocator::Pdf {
                    page_index,
                    page_label: page_label.clone(),
                    text_block_start: Some(block.block_index),
                    text_block_end: Some(block.block_index),
                },
                anchor: selected_anchor,
                quote_start,
                quote_end,
            });
        }
    }
    if resolved.is_empty() {
        Err(SourceContextError::MissingText)
    } else {
        Ok(resolved)
    }
}

fn build_pack(
    owner_id: UserId,
    context_owner: ContextOwner,
    scope: AiSourceScope,
    resolved: Vec<ResolvedFragment>,
) -> Result<AiContextPack, SourceContextError> {
    let context_id = context_owner.id();
    let mut remaining = AiContextPack::MAX_TEXT_BYTES;
    let mut fragments = Vec::new();
    let mut citations = Vec::new();
    let mut seen = HashSet::new();
    for source in resolved {
        if remaining == 0 || fragments.len() >= AiContextPack::MAX_FRAGMENTS {
            break;
        }
        if !seen.insert((source.unit_id.clone(), source.block_id.clone())) {
            continue;
        }
        let text = truncate_utf8(
            &source.text,
            remaining.min(AiContextPack::MAX_FRAGMENT_BYTES),
        );
        if text.is_empty() {
            continue;
        }
        remaining = remaining.saturating_sub(text.len());
        let citation_id = format!("ctx:{}:{}", context_id, fragments.len().saturating_add(1));
        let quote_end = source.quote_end.min(text.len());
        let quote_start = source.quote_start.min(quote_end);
        let text_hash = content_hash(text.as_bytes());
        fragments.push(AiContextFragment {
            citation_id: citation_id.clone(),
            unit_id: source.unit_id.clone(),
            block_id: source.block_id.clone(),
            label: source.label,
            text,
            text_hash,
            source_locator: source.locator.clone(),
        });
        citations.push(SourceCitation {
            schema_version: SOURCE_CITATION_SCHEMA_VERSION.to_owned(),
            citation_id,
            material_id: scope.material_id(),
            revision_id: scope.revision_id(),
            unit_id: source.unit_id,
            block_id: source.block_id,
            source_locator: source.locator,
            anchor: source.anchor,
            quote_hash: content_hash(
                fragments
                    .last()
                    .map(|fragment| &fragment.text.as_bytes()[quote_start..quote_end])
                    .unwrap_or_default(),
            ),
            fragment_byte_start: quote_start,
            fragment_byte_end: quote_end,
        });
    }
    if fragments.is_empty() {
        return Err(SourceContextError::MissingText);
    }
    let hash_payload = serde_json::to_vec(&serde_json::json!({
        "owner_id": owner_id,
        "context_id": context_id,
        "scope": &scope,
        "fragments": &fragments,
        "citations": &citations,
        "limits_version": EXPLICIT_CONTEXT_LIMITS_VERSION,
        "policy_version": POLICY_VERSION,
    }))
    .map_err(|_| SourceContextError::Storage)?;
    let pack = AiContextPack {
        schema_version: AI_CONTEXT_PACK_SCHEMA_VERSION.to_owned(),
        context_pack_id: Uuid::now_v7(),
        owner_id,
        task_id: context_owner.task_id(),
        message_id: context_owner.message_id(),
        scope,
        permission_snapshot: AiPermissionSnapshot {
            actor_id: owner_id,
            decision: AiPermissionDecision::Allowed,
            policy_version: POLICY_VERSION.to_owned(),
        },
        limits_version: EXPLICIT_CONTEXT_LIMITS_VERSION.to_owned(),
        fragments,
        citations,
        pack_hash: content_hash(&hash_payload),
    };
    pack.validate()
        .map_err(|_| SourceContextError::LimitExceeded)?;
    Ok(pack)
}

fn source_locator(locator: &SourceLocator) -> Result<AiSourceLocator, SourceContextError> {
    match locator {
        SourceLocator::Epub(locator) => Ok(AiSourceLocator::Epub {
            href: locator.content_href.clone(),
            byte_start: locator.text_offset_start,
            byte_end: locator.text_offset_end,
        }),
        SourceLocator::Markdown(locator) => Ok(AiSourceLocator::Markdown {
            file_path: locator.file_path.clone(),
            byte_start: locator.byte_start,
            byte_end: locator.byte_end,
        }),
        SourceLocator::Lum(locator) => Ok(AiSourceLocator::Lum {
            file_path: locator.source_path.clone(),
            byte_start: locator.byte_start,
            byte_end: locator.byte_end,
        }),
        SourceLocator::Pdf(locator) => Ok(AiSourceLocator::Pdf {
            page_index: locator.page_index,
            page_label: locator.page_label.clone(),
            text_block_start: locator.text_block_start,
            text_block_end: locator.text_block_end,
        }),
        SourceLocator::Web(locator) => Ok(AiSourceLocator::Web {
            canonical_url: locator
                .canonical_url
                .clone()
                .unwrap_or_else(|| locator.original_url.clone()),
            block_id: Some(locator.dom_path.clone()),
        }),
        SourceLocator::Telegram(locator) => Ok(AiSourceLocator::Telegram {
            chat_id: locator.chat_id.to_string(),
            message_id: locator.message_id,
        }),
        SourceLocator::Normalized { node_path } => Ok(AiSourceLocator::Web {
            canonical_url: format!("lumi://normalized/{}", node_path.join("/")),
            block_id: node_path.last().cloned(),
        }),
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_truncation_preserves_cyrillic_and_emoji_boundaries() {
        let value = "Привет 📚";

        let truncated = truncate_utf8(value, value.len().saturating_sub(2));

        assert_eq!(truncated, "Привет ");
    }

    #[test]
    fn normalized_locator_remains_source_backed() {
        let locator = source_locator(&SourceLocator::Normalized {
            node_path: vec!["chapter".to_owned(), "block".to_owned()],
        });

        assert!(matches!(
            locator,
            Ok(AiSourceLocator::Web { canonical_url, block_id })
                if canonical_url == "lumi://normalized/chapter/block"
                    && block_id.as_deref() == Some("block")
        ));
    }
}
