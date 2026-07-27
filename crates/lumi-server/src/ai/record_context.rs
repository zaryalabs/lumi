//! Record-only retrieval adapter for the durable global chat lifecycle.

use std::collections::HashSet;

use lumi_core::{
    content_hash, AiContextFragment, AiContextPack, AiMessageId, AiPermissionDecision,
    AiPermissionSnapshot, AiSourceScope, RecordContextSource, RecordSearchScope,
    RetrievalContextPolicy, RetrievalRequest, SearchRankingProfile, SearchRequest, SearchScope,
    SourceCitation, UserId, AI_CONTEXT_PACK_SCHEMA_VERSION, EXPLICIT_CONTEXT_LIMITS_VERSION,
    RECORD_RAG_PROMPT_VERSION,
};
use thiserror::Error;
use uuid::Uuid;

use crate::search::{SearchError, SearchRuntime};

const POLICY_VERSION: &str = "personal-record-owner.v1";
const MAX_RECORD_CONTEXT_BYTES: usize = 64 * 1024;
const MAX_RECORD_CONTEXT_CHUNKS: usize = 16;
const MAX_CHUNKS_PER_SOURCE: usize = 2;
const MIN_RETRIEVAL_SCORE: f32 = 0.05;

/// Record-context resolution failure safe for application mapping.
#[derive(Debug, Error)]
pub(crate) enum RecordContextError {
    /// Scope or fallback query violates the public contract.
    #[error("record context scope is invalid")]
    InvalidScope,
    /// No citation-ready record is strong enough to ground an answer.
    #[error("record context is insufficient")]
    InsufficientContext,
    /// Search derived state cannot serve permission-aware retrieval.
    #[error("record retrieval is unavailable")]
    Unavailable,
}

/// Resolve and pack one permission-filtered record scope for a chat message.
pub(crate) async fn resolve_for_message(
    search: &SearchRuntime,
    owner_id: UserId,
    message_id: AiMessageId,
    mut scope: RecordSearchScope,
    user_message: &str,
) -> Result<AiContextPack, RecordContextError> {
    scope
        .validate()
        .map_err(|_| RecordContextError::InvalidScope)?;
    let query = scope
        .query
        .as_deref()
        .unwrap_or(user_message)
        .trim()
        .to_owned();
    if query.is_empty() {
        return Err(RecordContextError::InvalidScope);
    }
    scope.query = Some(query.clone());
    let request = RetrievalRequest {
        search: SearchRequest {
            query,
            scope: SearchScope::Records,
            source_types: scope.record_types.clone(),
            tags: scope.tags.clone(),
            material_ids: scope.material_ids.clone(),
            statuses: scope.statuses.clone(),
            updated_range: scope.updated_range,
            ranking: SearchRankingProfile::AiRetrieval,
            cursor: None,
            limit: MAX_RECORD_CONTEXT_CHUNKS,
        },
        top_k: MAX_RECORD_CONTEXT_CHUNKS,
        context_policy: RetrievalContextPolicy {
            max_bytes: MAX_RECORD_CONTEXT_BYTES,
            max_chunks_per_source: MAX_CHUNKS_PER_SOURCE,
            include_surrounding_context: false,
        },
    };
    let retrieved = search
        .retrieve(owner_id, request)
        .await
        .map_err(map_search_error)?;
    build_pack(owner_id, message_id, scope, retrieved)
}

fn build_pack(
    owner_id: UserId,
    message_id: AiMessageId,
    scope: RecordSearchScope,
    retrieved: Vec<lumi_core::RetrievedChunk>,
) -> Result<AiContextPack, RecordContextError> {
    let mut seen_chunks = HashSet::new();
    let selected = retrieved
        .into_iter()
        .filter(|chunk| chunk.source_type.is_record())
        .filter(|chunk| chunk.score.total >= MIN_RETRIEVAL_SCORE)
        .filter(|chunk| chunk.citation.is_some())
        .filter(|chunk| seen_chunks.insert(chunk.chunk_id.clone()))
        .take(MAX_RECORD_CONTEXT_CHUNKS)
        .collect::<Vec<_>>();
    let first_citation = selected
        .first()
        .and_then(|chunk| chunk.citation.as_ref())
        .ok_or(RecordContextError::InsufficientContext)?;
    let pack_scope = source_scope(first_citation);
    let mut fragments = Vec::with_capacity(selected.len());
    let mut citations = Vec::with_capacity(selected.len());
    let mut record_context = Vec::with_capacity(selected.len());
    for chunk in selected {
        let Some(citation) = chunk.citation else {
            continue;
        };
        let citation_id = citation.citation_id.clone();
        let text_hash = content_hash(chunk.text.as_bytes());
        fragments.push(AiContextFragment {
            citation_id: citation_id.clone(),
            unit_id: citation.unit_id.clone(),
            block_id: chunk.chunk_id.clone(),
            label: Some(chunk.title.clone()),
            text: chunk.text,
            text_hash: text_hash.clone(),
            source_locator: citation.source_locator.clone(),
        });
        record_context.push(RecordContextSource {
            chunk_id: chunk.chunk_id,
            text_hash,
            citation_id,
            source_type: chunk.source_type,
            source_id: chunk.source_id,
            title: chunk.title,
            heading_path: chunk.heading_path,
            open_target: chunk.open_target,
        });
        citations.push(citation);
    }
    let hash_payload = serde_json::to_vec(&serde_json::json!({
        "owner_id": owner_id,
        "message_id": message_id,
        "record_scope": &scope,
        "fragments": &fragments,
        "record_context": &record_context,
        "prompt_version": RECORD_RAG_PROMPT_VERSION,
        "policy_version": POLICY_VERSION,
    }))
    .map_err(|_| RecordContextError::Unavailable)?;
    let pack = AiContextPack {
        schema_version: AI_CONTEXT_PACK_SCHEMA_VERSION.to_owned(),
        context_pack_id: Uuid::now_v7(),
        owner_id,
        task_id: None,
        message_id: Some(message_id),
        scope: pack_scope,
        record_scope: Some(scope),
        permission_snapshot: AiPermissionSnapshot {
            actor_id: owner_id,
            decision: AiPermissionDecision::Allowed,
            policy_version: POLICY_VERSION.to_owned(),
        },
        limits_version: EXPLICIT_CONTEXT_LIMITS_VERSION.to_owned(),
        fragments,
        citations,
        record_context,
        prompt_version: Some(RECORD_RAG_PROMPT_VERSION.to_owned()),
        pack_hash: content_hash(&hash_payload),
    };
    pack.validate()
        .map_err(|_| RecordContextError::InvalidScope)?;
    Ok(pack)
}

fn source_scope(citation: &SourceCitation) -> AiSourceScope {
    citation.anchor.as_ref().map_or_else(
        || AiSourceScope::Chapter {
            material_id: citation.material_id,
            revision_id: citation.revision_id,
            scope_ref: citation.unit_id.clone(),
        },
        |anchor| AiSourceScope::Selection {
            material_id: citation.material_id,
            revision_id: citation.revision_id,
            anchor: anchor.clone(),
        },
    )
}

fn map_search_error(error: SearchError) -> RecordContextError {
    match error {
        SearchError::InvalidRequest => RecordContextError::InvalidScope,
        SearchError::Unavailable | SearchError::Index(_) | SearchError::Storage => {
            RecordContextError::Unavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use lumi_core::{
        AiSourceLocator, RetrievedChunk, SearchOpenTarget, SearchScore, SearchSourceType,
        SourceCitation, SOURCE_CITATION_SCHEMA_VERSION,
    };

    use super::*;

    #[test]
    fn record_pack_keeps_cross_material_citations_and_exact_chunk_hashes(
    ) -> Result<(), RecordContextError> {
        let owner_id = Uuid::now_v7();
        let message_id = Uuid::now_v7();
        let first_material = Uuid::now_v7();
        let second_material = Uuid::now_v7();
        let chunks = vec![
            fixture_chunk(first_material, "chunk-a", "Первая заметка"),
            fixture_chunk(second_material, "chunk-b", "Вторая заметка"),
        ];

        let pack = build_pack(owner_id, message_id, fixture_scope(), chunks)?;

        assert_eq!(pack.record_context.len(), 2);
        assert_eq!(pack.citations[1].material_id, second_material);
        assert_eq!(
            pack.record_context[0].text_hash,
            content_hash("Первая заметка".as_bytes())
        );
        Ok(())
    }

    #[test]
    fn record_pack_rejects_context_below_grounding_threshold() {
        let owner_id = Uuid::now_v7();
        let message_id = Uuid::now_v7();
        let mut chunk = fixture_chunk(Uuid::now_v7(), "weak-chunk", "Слабое совпадение");
        chunk.score.total = MIN_RETRIEVAL_SCORE / 2.0;

        let result = build_pack(owner_id, message_id, fixture_scope(), vec![chunk]);

        assert!(matches!(
            result,
            Err(RecordContextError::InsufficientContext)
        ));
    }

    fn fixture_scope() -> RecordSearchScope {
        RecordSearchScope {
            query: Some("Что важно?".to_owned()),
            material_ids: Vec::new(),
            record_types: vec![SearchSourceType::Note],
            tags: Vec::new(),
            statuses: vec!["active".to_owned()],
            updated_range: None,
            retrieval_version: lumi_core::RECORD_RETRIEVAL_VERSION.to_owned(),
        }
    }

    fn fixture_chunk(material_id: Uuid, chunk_id: &str, text: &str) -> RetrievedChunk {
        let source_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let locator = AiSourceLocator::Web {
            canonical_url: "https://example.test/source".to_owned(),
            block_id: Some(chunk_id.to_owned()),
        };
        RetrievedChunk {
            chunk_id: chunk_id.to_owned(),
            text: text.to_owned(),
            source_type: SearchSourceType::Note,
            source_id,
            material_id: Some(material_id),
            title: "Заметка".to_owned(),
            heading_path: vec!["Глава".to_owned()],
            citation: Some(SourceCitation {
                schema_version: SOURCE_CITATION_SCHEMA_VERSION.to_owned(),
                citation_id: format!("search:{chunk_id}"),
                material_id,
                revision_id,
                unit_id: "chapter".to_owned(),
                block_id: chunk_id.to_owned(),
                source_locator: locator,
                anchor: None,
                quote_hash: content_hash(text.as_bytes()),
                fragment_byte_start: 0,
                fragment_byte_end: text.len(),
            }),
            score: SearchScore {
                bm25: 1.0,
                fasttext: 0.5,
                boost: 0.0,
                total: 0.8,
            },
            open_target: SearchOpenTarget::Annotation {
                material_id,
                annotation_id: source_id,
            },
        }
    }
}
