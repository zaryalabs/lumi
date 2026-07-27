//! Permission-aware BM25 plus fastText search, retrieval and rebuild lifecycle.

mod chunker;
mod fasttext;
mod index;
mod worker;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use lumi_core::{
    content_hash, AiSourceLocator, ImportedFixture, RetrievalRequest, RetrievedChunk, SearchChunk,
    SearchIndexState, SearchOpenTarget, SearchPage, SearchRankingProfile, SearchRebuildReceipt,
    SearchRequest, SearchResult, SearchScope, SearchScore, SearchSourceType, SearchStatus,
    SourceCitation, SourceLocator, UserId, SEARCH_CHUNKER_VERSION, SEARCH_INDEX_VERSION,
    SOURCE_CITATION_SCHEMA_VERSION,
};
use serde::Deserialize;
use sqlx_postgres::PgPool;
use uuid::Uuid;

use crate::account::AuthenticatedSession;
use crate::{AppError, AppState};
use fasttext::{cosine, FastTextLoadError, FinalFusionFastText, FixtureFastText, SemanticModel};
use index::{LexicalCandidate, SearchIndex, SearchIndexError};

const DEFAULT_PAGE_SIZE: usize = 20;
const MAX_CANDIDATES: usize = 500;

/// Search subsystem shared by HTTP and future MCP/AI adapters.
pub(crate) struct SearchRuntime {
    index: Option<Arc<SearchIndex>>,
    model: Option<Arc<dyn SemanticModel>>,
    pool: Option<PgPool>,
    model_version: String,
    failure_code: RwLock<Option<String>>,
}

impl SearchRuntime {
    /// Build an empty ready in-memory runtime for state-focused tests.
    pub(crate) fn empty_memory() -> Self {
        Self {
            index: SearchIndex::memory().ok().map(Arc::new),
            model: Some(Arc::new(FixtureFastText)),
            pool: None,
            model_version: "fixture.fasttext.v1".to_owned(),
            failure_code: RwLock::new(None),
        }
    }

    /// Build a ready in-memory search runtime over the seeded fixture.
    pub(crate) fn memory(imported: &ImportedFixture) -> Self {
        let index = SearchIndex::memory().ok().map(Arc::new);
        let model: Arc<dyn SemanticModel> = Arc::new(FixtureFastText);
        let runtime = Self {
            index,
            model: Some(model),
            pool: None,
            model_version: "fixture.fasttext.v1".to_owned(),
            failure_code: RwLock::new(None),
        };
        let chunks = chunker::reflowable_material(imported.material.id, &imported.package);
        if runtime
            .replace_in_index(
                imported.material.owner_id,
                "material",
                imported.material.id,
                chunks,
            )
            .is_err()
        {
            runtime.set_failure("search_fixture_index_failed");
        }
        runtime
    }

    /// Open a persistent derived index and optional verified fastText model.
    pub(crate) async fn persistent(
        pool: PgPool,
        search_root: &Path,
        model_path: Option<&Path>,
        model_sha256: Option<&str>,
        model_version: String,
    ) -> Self {
        let index_path = search_root.join(SEARCH_INDEX_VERSION);
        let (index, index_failure) = match SearchIndex::persistent(&index_path) {
            Ok(index) => (Some(Arc::new(index)), None),
            Err(error) => (None, Some(error.code().to_owned())),
        };
        let (model, model_failure): (Option<Arc<dyn SemanticModel>>, Option<String>) =
            match (model_path, model_sha256) {
                (Some(path), Some(checksum)) => match FinalFusionFastText::load(path, checksum) {
                    Ok(model) => (Some(Arc::new(model)), None),
                    Err(error) => (None, Some(error.code().to_owned())),
                },
                _ => (None, Some(FastTextLoadError::Unavailable.code().to_owned())),
            };
        let failure_code = index_failure.or(model_failure);
        let runtime = Self {
            index,
            model,
            pool: Some(pool),
            model_version,
            failure_code: RwLock::new(failure_code),
        };
        if runtime.ensure_initial_rebuilds().await.is_err() {
            runtime.set_failure("search_initial_rebuild_enqueue_failed");
        }
        runtime
    }

    /// Whether all query and retrieval prerequisites are ready.
    pub(crate) fn is_query_ready(&self) -> bool {
        self.index.is_some() && self.model.is_some() && self.failure().is_none()
    }

    /// Execute a bounded permission-scoped user query.
    pub(crate) async fn search(
        &self,
        owner_id: UserId,
        mut request: SearchRequest,
    ) -> Result<SearchPage, SearchError> {
        request
            .normalize_and_validate()
            .map_err(|_| SearchError::InvalidRequest)?;
        let ranked = self.rank(owner_id, &request)?;
        let generation = self.index.as_ref().map_or(0, |index| index.generation());
        let offset = decode_cursor(request.cursor.as_deref(), generation, &request)?;
        let end = offset.saturating_add(request.limit).min(ranked.len());
        let items = ranked[offset.min(ranked.len())..end]
            .iter()
            .map(|ranked| result_from_ranked(ranked, &request.query))
            .collect();
        let next_cursor = (end < ranked.len())
            .then(|| encode_cursor(generation, end, query_fingerprint(&request)));
        Ok(SearchPage {
            items,
            next_cursor,
            index_generation: generation,
        })
    }

    /// Retrieve bounded source text and citations without invoking an LLM.
    pub(crate) async fn retrieve(
        &self,
        owner_id: UserId,
        mut request: RetrievalRequest,
    ) -> Result<Vec<RetrievedChunk>, SearchError> {
        request
            .normalize_and_validate()
            .map_err(|_| SearchError::InvalidRequest)?;
        let ranked = self.rank(owner_id, &request.search)?;
        let mut per_source = HashMap::<Uuid, usize>::new();
        let mut used_bytes = 0_usize;
        let mut retrieved = Vec::new();
        for item in ranked {
            if retrieved.len() == request.top_k {
                break;
            }
            let count = per_source
                .entry(item.candidate.chunk.source_id)
                .or_default();
            if *count >= request.context_policy.max_chunks_per_source {
                continue;
            }
            let next_bytes = used_bytes.saturating_add(item.candidate.chunk.text.len());
            if next_bytes > request.context_policy.max_bytes {
                continue;
            }
            *count += 1;
            used_bytes = next_bytes;
            let chunk = &item.candidate.chunk;
            retrieved.push(RetrievedChunk {
                chunk_id: chunk.id.clone(),
                text: chunk.text.clone(),
                source_type: chunk.source_type,
                source_id: chunk.source_id,
                material_id: chunk.material_id,
                title: chunk.title.clone(),
                heading_path: chunk.heading_path.clone(),
                citation: citation_for_chunk(chunk),
                score: item.score,
                open_target: open_target(chunk),
            });
        }
        Ok(retrieved)
    }

    /// Return permission-scoped status and derived counts.
    pub(crate) async fn status(&self, owner_id: UserId) -> SearchStatus {
        let failure = self.failure();
        let generation = self.index.as_ref().map_or(0, |index| index.generation());
        let mut status = SearchStatus {
            state: if failure.is_some() {
                SearchIndexState::Failed
            } else {
                SearchIndexState::Ready
            },
            index_version: SEARCH_INDEX_VERSION.to_owned(),
            chunker_version: SEARCH_CHUNKER_VERSION.to_owned(),
            model_version: self.model_version.clone(),
            index_generation: generation,
            document_count: 0,
            chunk_count: 0,
            no_text_document_count: 0,
            pending_jobs: 0,
            failure_code: failure,
        };
        let Some(pool) = self.pool.as_ref() else {
            if let Some(index) = &self.index {
                status.index_generation = index.generation();
                status.document_count = 1;
            }
            return status;
        };
        let counts = sqlx_core::query::query(
            "SELECT
                (SELECT count(*) FROM search_documents WHERE user_id = $1) AS documents,
                (SELECT count(*) FROM search_chunks WHERE user_id = $1) AS chunks,
                (SELECT count(*)
                   FROM search_documents document
                  WHERE document.user_id = $1
                    AND NOT EXISTS (
                        SELECT 1 FROM search_chunks chunk
                         WHERE chunk.document_id = document.document_id
                    )) AS no_text_documents,
                (SELECT count(*) FROM search_index_requests
                  WHERE user_id = $1 AND status IN ('queued', 'running')) AS pending,
                (SELECT state FROM search_account_state WHERE user_id = $1) AS state,
                (SELECT index_generation FROM search_account_state WHERE user_id = $1) AS generation,
                (SELECT failure_code FROM search_account_state WHERE user_id = $1) AS failure_code",
        )
        .bind(owner_id)
        .fetch_one(pool)
        .await;
        if let Ok(row) = counts {
            use sqlx_core::row::Row;
            status.document_count =
                u64::try_from(row.try_get::<i64, _>("documents").unwrap_or_default())
                    .unwrap_or_default();
            status.chunk_count = u64::try_from(row.try_get::<i64, _>("chunks").unwrap_or_default())
                .unwrap_or_default();
            status.no_text_document_count = u64::try_from(
                row.try_get::<i64, _>("no_text_documents")
                    .unwrap_or_default(),
            )
            .unwrap_or_default();
            status.pending_jobs =
                u64::try_from(row.try_get::<i64, _>("pending").unwrap_or_default())
                    .unwrap_or_default();
            status.index_generation = u64::try_from(
                row.try_get::<Option<i64>, _>("generation")
                    .unwrap_or_default()
                    .unwrap_or_default(),
            )
            .unwrap_or(generation);
            if status.failure_code.is_none() {
                status.failure_code = row
                    .try_get::<Option<String>, _>("failure_code")
                    .unwrap_or_default();
            }
            if status.failure_code.is_none() {
                status.state = match row
                    .try_get::<Option<String>, _>("state")
                    .unwrap_or_default()
                    .as_deref()
                {
                    Some("rebuilding") => SearchIndexState::Rebuilding,
                    Some("partial") => SearchIndexState::Partial,
                    Some("failed") => SearchIndexState::Failed,
                    Some("ready") if status.pending_jobs == 0 => SearchIndexState::Ready,
                    _ if status.pending_jobs > 0 => SearchIndexState::Partial,
                    _ if status.document_count == 0 => SearchIndexState::Partial,
                    _ => SearchIndexState::Ready,
                };
            }
        }
        status
    }

    fn rank(
        &self,
        owner_id: UserId,
        request: &SearchRequest,
    ) -> Result<Vec<RankedChunk>, SearchError> {
        let index = self.index.as_ref().ok_or(SearchError::Unavailable)?;
        let model = self.model.as_ref().ok_or(SearchError::Unavailable)?;
        if self.failure().is_some() {
            return Err(SearchError::Unavailable);
        }
        let query_vector = model
            .vector(&request.query)
            .ok_or(SearchError::Unavailable)?;
        let candidates = index
            .candidates(owner_id, request, MAX_CANDIDATES)
            .map_err(SearchError::Index)?;
        let query_lower = request.query.to_lowercase();
        let mut ranked = candidates
            .into_iter()
            .map(|candidate| {
                let semantic = cosine(&query_vector, &candidate.vector);
                let lexical = candidate.bm25 / (candidate.bm25 + 1.0);
                let boost = score_boost(&candidate.chunk, &query_lower, request);
                let semantic_weight = match request.ranking {
                    SearchRankingProfile::AiRetrieval => 0.38,
                    SearchRankingProfile::Global
                    | SearchRankingProfile::Reader
                    | SearchRankingProfile::Records => 0.28,
                };
                let total = lexical * (1.0 - semantic_weight)
                    + ((semantic + 1.0) / 2.0) * semantic_weight
                    + boost;
                RankedChunk {
                    score: SearchScore {
                        bm25: candidate.bm25,
                        fasttext: semantic,
                        boost,
                        total,
                    },
                    candidate,
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .score
                .total
                .total_cmp(&left.score.total)
                .then_with(|| left.candidate.chunk.id.cmp(&right.candidate.chunk.id))
        });
        let mut seen = HashSet::new();
        ranked.retain(|item| {
            seen.insert((
                item.candidate.chunk.source_id,
                item.candidate.chunk.text_hash.clone(),
            ))
        });
        Ok(ranked)
    }

    fn replace_in_index(
        &self,
        owner_id: UserId,
        source_kind: &str,
        source_id: Uuid,
        chunks: Vec<SearchChunk>,
    ) -> Result<u64, SearchError> {
        let chunks = self.vectorize(chunks)?;
        self.replace_vectorized(owner_id, source_kind, source_id, &chunks)
    }

    fn vectorize(
        &self,
        chunks: Vec<SearchChunk>,
    ) -> Result<Vec<(SearchChunk, Vec<f32>)>, SearchError> {
        let model = self.model.as_ref().ok_or(SearchError::Unavailable)?;
        let vectorized = chunks
            .into_iter()
            .filter_map(|chunk| model.vector(&chunk.text).map(|vector| (chunk, vector)))
            .collect::<Vec<_>>();
        Ok(vectorized)
    }

    fn replace_vectorized(
        &self,
        owner_id: UserId,
        source_kind: &str,
        source_id: Uuid,
        chunks: &[(SearchChunk, Vec<f32>)],
    ) -> Result<u64, SearchError> {
        let index = self.index.as_ref().ok_or(SearchError::Unavailable)?;
        index
            .replace(
                owner_id,
                &document_key(owner_id, source_kind, source_id),
                chunks,
            )
            .map_err(SearchError::Index)
    }

    fn failure(&self) -> Option<String> {
        self.failure_code
            .read()
            .ok()
            .and_then(|failure| failure.clone())
    }

    fn set_failure(&self, code: &str) {
        if let Ok(mut failure) = self.failure_code.write() {
            *failure = Some(code.to_owned());
        }
    }
}

struct RankedChunk {
    candidate: LexicalCandidate,
    score: SearchScore,
}

/// Search application failure with no private payload details.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SearchError {
    /// Public request or cursor is invalid.
    #[error("search request is invalid")]
    InvalidRequest,
    /// Search is disabled, rebuilding without a usable shard, or failed.
    #[error("search is unavailable")]
    Unavailable,
    /// Derived lexical state failed.
    #[error(transparent)]
    Index(#[from] SearchIndexError),
    /// PostgreSQL job or projection state failed.
    #[error("search storage is unavailable")]
    Storage,
}

/// Protected search, retrieval, status and rebuild routes.
pub(crate) fn protected_routes() -> Router<AppState> {
    Router::new()
        .route("/search", get(search))
        .route("/search/retrieve", post(retrieve))
        .route("/search/status", get(status))
        .route("/search/rebuild", post(rebuild))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    scope: Option<String>,
    r#type: Option<String>,
    material_id: Option<Uuid>,
    tag: Option<String>,
    cursor: Option<String>,
    limit: Option<usize>,
}

async fn search(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<SearchPage>, AppError> {
    let request = request_from_query(query)?;
    state
        .search
        .search(session.user_id, request)
        .await
        .map(Json)
        .map_err(map_search_error)
}

async fn retrieve(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
    Json(request): Json<RetrievalRequest>,
) -> Result<Json<Vec<RetrievedChunk>>, AppError> {
    state
        .search
        .retrieve(session.user_id, request)
        .await
        .map(Json)
        .map_err(map_search_error)
}

async fn status(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Json<SearchStatus> {
    Json(state.search.status(session.user_id).await)
}

async fn rebuild(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<Json<SearchRebuildReceipt>, AppError> {
    state
        .search
        .enqueue_rebuild(session.user_id)
        .await
        .map(Json)
        .map_err(map_search_error)
}

fn request_from_query(query: SearchQuery) -> Result<SearchRequest, AppError> {
    let source_types = query
        .r#type
        .as_deref()
        .map(|types| {
            types
                .split(',')
                .map(parse_source_type)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let scope = match query.scope.as_deref() {
        None | Some("personal") => SearchScope::Personal,
        Some("records") => SearchScope::Records,
        Some("material") => SearchScope::Material {
            material_id: query
                .material_id
                .ok_or_else(|| AppError::BadRequest("material_id is required".to_owned()))?,
        },
        Some(_) => return Err(AppError::BadRequest("unknown search scope".to_owned())),
    };
    let ranking = match scope {
        SearchScope::Material { .. } => SearchRankingProfile::Reader,
        SearchScope::Records => SearchRankingProfile::Records,
        SearchScope::Personal => SearchRankingProfile::Global,
    };
    Ok(SearchRequest {
        query: query.q,
        scope,
        source_types,
        tags: query
            .tag
            .map(|tags| tags.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        material_ids: Vec::new(),
        statuses: Vec::new(),
        updated_range: None,
        ranking,
        cursor: query.cursor,
        limit: query.limit.unwrap_or(DEFAULT_PAGE_SIZE),
    })
}

fn parse_source_type(value: &str) -> Result<SearchSourceType, AppError> {
    match value.trim() {
        "material" => Ok(SearchSourceType::Material),
        "highlight" => Ok(SearchSourceType::Highlight),
        "note" => Ok(SearchSourceType::Note),
        "margin_note" => Ok(SearchSourceType::MarginNote),
        "voice_transcript" => Ok(SearchSourceType::VoiceTranscript),
        "ai_artifact" => Ok(SearchSourceType::AiArtifact),
        "learning_item" => Ok(SearchSourceType::LearningItem),
        _ => Err(AppError::BadRequest(
            "unknown search source type".to_owned(),
        )),
    }
}

fn map_search_error(error: SearchError) -> AppError {
    match error {
        SearchError::InvalidRequest => AppError::BadRequest("invalid search request".to_owned()),
        SearchError::Unavailable => AppError::Unavailable("search index or fastText model"),
        SearchError::Index(_) | SearchError::Storage => AppError::Internal("search operation"),
    }
}

fn score_boost(chunk: &SearchChunk, query: &str, request: &SearchRequest) -> f32 {
    let mut boost = 0.0;
    if chunk.title.to_lowercase() == query {
        boost += 0.25;
    } else if chunk.title.to_lowercase().contains(query) {
        boost += 0.12;
    }
    if chunk.tags.iter().any(|tag| tag.to_lowercase() == query) {
        boost += 0.12;
    }
    if matches!(request.ranking, SearchRankingProfile::Reader)
        && matches!(request.scope, SearchScope::Material { .. })
    {
        boost += 0.05;
    }
    if matches!(request.ranking, SearchRankingProfile::Records) && chunk.source_type.is_record() {
        boost += 0.05;
    }
    boost
}

fn result_from_ranked(ranked: &RankedChunk, query: &str) -> SearchResult {
    let chunk = &ranked.candidate.chunk;
    SearchResult {
        chunk_id: chunk.id.clone(),
        source_type: chunk.source_type,
        source_id: chunk.source_id,
        material_id: chunk.material_id,
        title: chunk.title.clone(),
        heading_path: chunk.heading_path.clone(),
        snippet: plain_snippet(&chunk.text, query),
        open_target: open_target(chunk),
        score: ranked.score,
    }
}

fn open_target(chunk: &SearchChunk) -> SearchOpenTarget {
    match chunk.source_type {
        SearchSourceType::Material => match (chunk.material_id, chunk.revision_id) {
            (Some(material_id), Some(revision_id)) if chunk.anchor.is_some() => {
                SearchOpenTarget::Reader {
                    material_id,
                    revision_id,
                    anchor: chunk.anchor.clone().map(Box::new),
                }
            }
            (Some(material_id), _) => SearchOpenTarget::Material { material_id },
            _ => SearchOpenTarget::Material {
                material_id: chunk.source_id,
            },
        },
        SearchSourceType::Highlight
        | SearchSourceType::Note
        | SearchSourceType::MarginNote
        | SearchSourceType::VoiceTranscript => SearchOpenTarget::Annotation {
            material_id: chunk.material_id.unwrap_or(chunk.source_id),
            annotation_id: chunk.source_id,
        },
        SearchSourceType::AiArtifact => SearchOpenTarget::AiArtifact {
            artifact_id: chunk.source_id,
        },
        SearchSourceType::LearningItem => SearchOpenTarget::LearningItem {
            item_id: chunk.source_id,
        },
    }
}

fn plain_snippet(text: &str, query: &str) -> String {
    const MAX_SNIPPET_CHARS: usize = 280;
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= MAX_SNIPPET_CHARS {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let byte_start = lower.find(&query.to_lowercase()).unwrap_or_default();
    let char_start = lower[..byte_start].chars().count();
    let start = char_start.saturating_sub(MAX_SNIPPET_CHARS / 3);
    let end = (start + MAX_SNIPPET_CHARS).min(chars.len());
    chars[start..end].iter().collect()
}

fn citation_for_chunk(chunk: &SearchChunk) -> Option<SourceCitation> {
    let material_id = chunk.material_id?;
    let revision_id = chunk.revision_id?;
    let source_locator = chunk
        .anchor
        .as_ref()
        .and_then(|anchor| anchor.source_locator.as_ref())
        .and_then(ai_locator)
        .unwrap_or_else(|| AiSourceLocator::Web {
            canonical_url: format!(
                "lumi://record/{}/{}",
                chunk.source_type.as_str(),
                chunk.source_id
            ),
            block_id: Some(chunk.id.clone()),
        });
    let citation_suffix = chunk.id.chars().take(16).collect::<String>();
    Some(SourceCitation {
        schema_version: SOURCE_CITATION_SCHEMA_VERSION.to_owned(),
        citation_id: format!("search:{citation_suffix}"),
        material_id,
        revision_id,
        unit_id: chunk
            .heading_path
            .first()
            .cloned()
            .unwrap_or_else(|| "document".to_owned()),
        block_id: chunk
            .anchor
            .as_ref()
            .and_then(|anchor| anchor.node_path.last().cloned())
            .unwrap_or_else(|| chunk.id.clone()),
        source_locator,
        anchor: chunk.anchor.clone().map(Box::new),
        quote_hash: chunk.text_hash.clone(),
        fragment_byte_start: 0,
        fragment_byte_end: chunk.text.len(),
    })
}

fn ai_locator(locator: &SourceLocator) -> Option<AiSourceLocator> {
    match locator {
        SourceLocator::Epub(locator) => Some(AiSourceLocator::Epub {
            href: locator.content_href.clone(),
            byte_start: locator.text_offset_start,
            byte_end: locator.text_offset_end,
        }),
        SourceLocator::Markdown(locator) => Some(AiSourceLocator::Markdown {
            file_path: locator.file_path.clone(),
            byte_start: locator.byte_start,
            byte_end: locator.byte_end,
        }),
        SourceLocator::Lum(locator) => Some(AiSourceLocator::Lum {
            file_path: locator.source_path.clone(),
            byte_start: locator.byte_start,
            byte_end: locator.byte_end,
        }),
        SourceLocator::Pdf(locator) => Some(AiSourceLocator::Pdf {
            page_index: locator.page_index,
            page_label: locator.page_label.clone(),
            text_block_start: locator.text_block_start,
            text_block_end: locator.text_block_end,
        }),
        SourceLocator::Web(locator) => Some(AiSourceLocator::Web {
            canonical_url: locator
                .canonical_url
                .clone()
                .unwrap_or_else(|| locator.original_url.clone()),
            block_id: None,
        }),
        SourceLocator::Telegram(locator) => Some(AiSourceLocator::Telegram {
            chat_id: locator.chat_id.to_string(),
            message_id: locator.message_id,
        }),
        SourceLocator::Normalized { .. } => None,
    }
}

fn document_key(owner_id: Uuid, source_kind: &str, source_id: Uuid) -> String {
    format!("{owner_id}:{source_kind}:{source_id}")
}

fn query_fingerprint(request: &SearchRequest) -> String {
    let mut request = request.clone();
    request.cursor = None;
    content_hash(serde_json::to_vec(&request).unwrap_or_default().as_slice())
}

fn encode_cursor(generation: u64, offset: usize, fingerprint: String) -> String {
    format!("v1:{generation}:{offset}:{}", &fingerprint[..16])
}

fn decode_cursor(
    cursor: Option<&str>,
    generation: u64,
    request: &SearchRequest,
) -> Result<usize, SearchError> {
    let Some(cursor) = cursor else {
        return Ok(0);
    };
    let mut parts = cursor.split(':');
    let version = parts.next();
    let cursor_generation = parts.next().and_then(|value| value.parse::<u64>().ok());
    let offset = parts.next().and_then(|value| value.parse::<usize>().ok());
    let fingerprint = parts.next();
    if parts.next().is_some()
        || version != Some("v1")
        || cursor_generation != Some(generation)
        || fingerprint != Some(&query_fingerprint(request)[..16])
    {
        return Err(SearchError::InvalidRequest);
    }
    offset.ok_or(SearchError::InvalidRequest)
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use lumi_core::{rich_epub_fixture, SearchField, SearchRankingProfile, SEARCH_CHUNKER_VERSION};
    use serde::Deserialize;

    use super::fasttext::FixtureFastText;
    use super::*;

    #[tokio::test]
    async fn memory_search_returns_source_backed_result() -> Result<(), Box<dyn std::error::Error>>
    {
        let imported = lumi_core::import_epub_fixture(UserId::nil(), &rich_epub_fixture())?;
        let runtime = SearchRuntime::memory(&imported);

        let page = runtime
            .search(
                UserId::nil(),
                SearchRequest {
                    query: "reader".to_owned(),
                    scope: SearchScope::Personal,
                    source_types: Vec::new(),
                    tags: Vec::new(),
                    material_ids: Vec::new(),
                    statuses: Vec::new(),
                    updated_range: None,
                    ranking: SearchRankingProfile::Global,
                    cursor: None,
                    limit: 10,
                },
            )
            .await?;

        assert!(!page.items.is_empty());
        assert!(page
            .items
            .iter()
            .all(|item| item.material_id == Some(imported.material.id)));
        Ok(())
    }

    #[derive(Deserialize)]
    struct SearchQualityCorpus {
        queries: Vec<SearchQualityCase>,
    }

    #[derive(Deserialize)]
    struct SearchQualityCase {
        query: String,
        relevant: String,
        irrelevant: String,
    }

    #[tokio::test]
    async fn mixed_language_search_quality_corpus_places_relevant_in_top_five(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let corpus: SearchQualityCorpus = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/search/mixed-language.json"
        ))?;
        let owner_id = Uuid::from_u128(10);
        let index = Arc::new(SearchIndex::memory()?);
        let model: Arc<dyn SemanticModel> = Arc::new(FixtureFastText);
        let mut documents = Vec::new();
        for (case_ordinal, case) in corpus.queries.iter().enumerate() {
            for (variant, text) in [
                ("relevant", &case.relevant),
                ("irrelevant", &case.irrelevant),
            ] {
                let ordinal = u128::try_from(case_ordinal)?;
                let source_id = Uuid::from_u128(
                    100 + ordinal.saturating_mul(2) + u128::from(variant == "irrelevant"),
                );
                let chunk = quality_chunk(source_id, variant, text);
                let vector = model.vector(text).ok_or("quality fixture vector")?;
                documents.push((
                    document_key(owner_id, "annotation", source_id),
                    vec![(chunk, vector)],
                ));
            }
        }
        index.rebuild(owner_id, documents)?;
        let runtime = SearchRuntime {
            index: Some(index),
            model: Some(model),
            pool: None,
            model_version: "fixture.fasttext.quality.v1".to_owned(),
            failure_code: RwLock::new(None),
        };

        for case in corpus.queries {
            let page = runtime
                .search(
                    owner_id,
                    SearchRequest {
                        query: case.query.clone(),
                        scope: SearchScope::Records,
                        source_types: vec![SearchSourceType::Note],
                        tags: Vec::new(),
                        material_ids: Vec::new(),
                        statuses: Vec::new(),
                        updated_range: None,
                        ranking: SearchRankingProfile::Records,
                        cursor: None,
                        limit: 10,
                    },
                )
                .await?;
            let relevant_rank = page
                .items
                .iter()
                .position(|item| item.snippet == case.relevant)
                .ok_or("relevant quality result is absent")?;
            let irrelevant_rank = page
                .items
                .iter()
                .position(|item| item.snippet == case.irrelevant)
                .unwrap_or(usize::MAX);
            assert!(
                relevant_rank < 5 && relevant_rank < irrelevant_rank,
                "query {:?} violates top-5 quality threshold",
                case.query
            );
        }
        Ok(())
    }

    #[test]
    fn cursor_rejects_changed_query() {
        let first = SearchRequest {
            query: "first".to_owned(),
            scope: SearchScope::Personal,
            source_types: Vec::new(),
            tags: Vec::new(),
            material_ids: Vec::new(),
            statuses: Vec::new(),
            updated_range: None,
            ranking: SearchRankingProfile::Global,
            cursor: None,
            limit: 10,
        };
        let mut second = first.clone();
        second.query = "second".to_owned();
        let cursor = encode_cursor(4, 10, query_fingerprint(&first));

        assert!(decode_cursor(Some(&cursor), 4, &second).is_err());
    }

    #[test]
    fn cursor_accepts_the_same_query_on_the_next_page() -> Result<(), SearchError> {
        let mut request = SearchRequest {
            query: "same".to_owned(),
            scope: SearchScope::Personal,
            source_types: Vec::new(),
            tags: Vec::new(),
            material_ids: Vec::new(),
            statuses: Vec::new(),
            updated_range: None,
            ranking: SearchRankingProfile::Global,
            cursor: None,
            limit: 10,
        };
        let cursor = encode_cursor(4, 10, query_fingerprint(&request));
        request.cursor = Some(cursor.clone());

        assert_eq!(decode_cursor(Some(&cursor), 4, &request)?, 10);
        Ok(())
    }

    #[tokio::test]
    async fn performance_search_500k_chunks_first_page_stays_within_budget(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let enabled = std::env::var("LUMI_SEARCH_PERFORMANCE").ok().as_deref() == Some("1")
            || std::env::var("LUMI_PERFORMANCE").ok().as_deref() == Some("1");
        if !enabled {
            return Ok(());
        }

        let owner_id = Uuid::from_u128(1);
        let index = Arc::new(SearchIndex::memory()?);
        index.rebuild(owner_id, performance_documents())?;
        let runtime = SearchRuntime {
            index: Some(Arc::clone(&index)),
            model: Some(Arc::new(FixtureFastText)),
            pool: None,
            model_version: "fixture.fasttext.performance.v1".to_owned(),
            failure_code: RwLock::new(None),
        };
        let request = SearchRequest {
            query: "память retrieval".to_owned(),
            scope: SearchScope::Personal,
            source_types: Vec::new(),
            tags: Vec::new(),
            material_ids: Vec::new(),
            statuses: Vec::new(),
            updated_range: None,
            ranking: SearchRankingProfile::Global,
            cursor: None,
            limit: 20,
        };
        let mut samples = Vec::with_capacity(20);
        for _ in 0..20 {
            let started = Instant::now();
            let page = runtime.search(owner_id, request.clone()).await?;
            samples.push(started.elapsed());
            assert_eq!(page.items.len(), 20);
        }
        samples.sort_unstable();
        let p95 = samples[18];
        assert!(
            p95 < Duration::from_millis(750),
            "search p95 {p95:?} exceeds 750ms"
        );

        let note_id = Uuid::from_u128(600_001);
        let note = performance_chunk(note_id, 0, SearchSourceType::Note);
        let started = Instant::now();
        index.replace(
            owner_id,
            &document_key(owner_id, "annotation", note_id),
            &[(note, vec![1.0; 16])],
        )?;
        let incremental = started.elapsed();
        assert!(
            incremental < Duration::from_millis(250),
            "incremental replace {incremental:?} exceeds 250ms"
        );
        Ok(())
    }

    fn performance_documents() -> impl Iterator<Item = (String, Vec<(SearchChunk, Vec<f32>)>)> {
        let owner_id = Uuid::from_u128(1);
        (1_u128..=10_000).map(move |ordinal| {
            let material_id = Uuid::from_u128(ordinal + 1);
            let chunks = (0..50)
                .map(|chunk_ordinal| {
                    (
                        performance_chunk(material_id, chunk_ordinal, SearchSourceType::Material),
                        vec![1.0; 16],
                    )
                })
                .collect();
            (document_key(owner_id, "material", material_id), chunks)
        })
    }

    fn performance_chunk(
        source_id: Uuid,
        ordinal: usize,
        source_type: SearchSourceType,
    ) -> SearchChunk {
        let text = if ordinal == 0 {
            format!("память retrieval relevant source {source_id}")
        } else {
            format!("bounded technical filler {source_id} {ordinal}")
        };
        SearchChunk {
            id: SearchChunk::stable_id(
                source_type,
                source_id,
                "performance.v1",
                SearchField::Body,
                ordinal,
            ),
            source_type,
            source_id,
            material_id: (source_type == SearchSourceType::Material).then_some(source_id),
            revision_id: None,
            source_version: "performance.v1".to_owned(),
            field: SearchField::Body,
            title: format!("Benchmark {source_id}"),
            heading_path: vec!["Performance".to_owned()],
            text_hash: content_hash(text.as_bytes()),
            text,
            language: Some("ru".to_owned()),
            tags: Vec::new(),
            status: None,
            updated_at: None,
            anchor: None,
            chunker_version: SEARCH_CHUNKER_VERSION.to_owned(),
        }
    }

    fn quality_chunk(source_id: Uuid, title: &str, text: &str) -> SearchChunk {
        SearchChunk {
            id: SearchChunk::stable_id(
                SearchSourceType::Note,
                source_id,
                "quality.v1",
                SearchField::Body,
                0,
            ),
            source_type: SearchSourceType::Note,
            source_id,
            material_id: Some(source_id),
            revision_id: None,
            source_version: "quality.v1".to_owned(),
            field: SearchField::Body,
            title: title.to_owned(),
            heading_path: Vec::new(),
            text: text.to_owned(),
            language: None,
            tags: Vec::new(),
            status: Some("active".to_owned()),
            updated_at: None,
            anchor: None,
            text_hash: content_hash(text.as_bytes()),
            chunker_version: SEARCH_CHUNKER_VERSION.to_owned(),
        }
    }
}
