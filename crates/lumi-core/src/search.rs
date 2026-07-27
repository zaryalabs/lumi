//! Shared contracts for permission-aware indexed search and AI retrieval.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{content_hash, Anchor, DocumentRevisionId, MaterialId, SourceCitation, TimestampMs};

/// Version of the indexed-search API and persisted chunk contract.
pub const SEARCH_CONTRACT_VERSION: &str = "search.contract.v2";
/// Version of source-aware chunking rules.
pub const SEARCH_CHUNKER_VERSION: &str = "search.chunker.v3";
/// Version of the initial Tantivy schema.
pub const SEARCH_INDEX_VERSION: &str = "tantivy.v3";
/// Version of the fastText score-fusion policy.
pub const SEARCH_RANKING_VERSION: &str = "bm25-fasttext.v1";
/// Maximum UTF-8 query size accepted by the public API.
pub const MAX_SEARCH_QUERY_BYTES: usize = 2_048;
/// Maximum number of results returned by one user query.
pub const MAX_SEARCH_RESULTS: usize = 100;
/// Maximum number of chunks returned for one AI retrieval request.
pub const MAX_RETRIEVED_CHUNKS: usize = 32;

/// Stable content-derived search chunk identifier.
pub type SearchChunkId = String;

/// Source families admitted to the personal search index.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchSourceType {
    /// Material metadata and normalized source text.
    Material,
    /// Source-backed highlight.
    Highlight,
    /// Selection note.
    Note,
    /// Structural or page margin note.
    MarginNote,
    /// Accepted transcript attached to a voice note.
    VoiceTranscript,
    /// Accepted typed AI artifact.
    AiArtifact,
    /// Active learning item.
    LearningItem,
    /// Member-visible comment in a shared material discussion.
    SharedComment,
    /// Member-visible Community chat message.
    SharedChatMessage,
    /// Separately published shared highlight quote.
    SharedHighlight,
}

impl SearchSourceType {
    /// Return the stable persistence and query token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Material => "material",
            Self::Highlight => "highlight",
            Self::Note => "note",
            Self::MarginNote => "margin_note",
            Self::VoiceTranscript => "voice_transcript",
            Self::AiArtifact => "ai_artifact",
            Self::LearningItem => "learning_item",
            Self::SharedComment => "shared_comment",
            Self::SharedChatMessage => "shared_chat_message",
            Self::SharedHighlight => "shared_highlight",
        }
    }

    /// Return whether this source is a personal record rather than source text.
    #[must_use]
    pub const fn is_record(self) -> bool {
        matches!(
            self,
            Self::Highlight
                | Self::Note
                | Self::MarginNote
                | Self::VoiceTranscript
                | Self::AiArtifact
                | Self::LearningItem
        )
    }

    /// Return whether this source is permission-bearing Community content.
    #[must_use]
    pub const fn is_social(self) -> bool {
        matches!(
            self,
            Self::SharedComment | Self::SharedChatMessage | Self::SharedHighlight
        )
    }
}

/// Field that supplied one indexed chunk.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    /// Material or source metadata.
    Metadata,
    /// User-visible title.
    Title,
    /// Structural heading.
    Heading,
    /// Main normalized body.
    Body,
    /// Highlighted source quote.
    Quote,
    /// Accepted voice transcript.
    Transcript,
    /// Learning prompt.
    Prompt,
    /// Learning explanation.
    Explanation,
    /// Shared comment or chat body.
    Message,
}

impl SearchField {
    /// Return the stable persistence token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Title => "title",
            Self::Heading => "heading",
            Self::Body => "body",
            Self::Quote => "quote",
            Self::Transcript => "transcript",
            Self::Prompt => "prompt",
            Self::Explanation => "explanation",
            Self::Message => "message",
        }
    }
}

/// Search scope resolved after account authorization.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchScope {
    /// All searchable personal sources owned by the account.
    #[default]
    Personal,
    /// Current immutable material only.
    Material {
        /// Material restriction.
        material_id: MaterialId,
    },
    /// Personal annotations, accepted artifacts and learning items only.
    Records,
    /// Content visible inside one active Community membership boundary.
    Community {
        /// Authorized Space restriction.
        space_id: Uuid,
        /// Optional shared material restriction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shared_material_id: Option<Uuid>,
    },
}

/// Ranking behavior shared by the Web, Desk and AI adapters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchRankingProfile {
    /// Ordinary cross-library search.
    #[default]
    Global,
    /// Search inside the current material.
    Reader,
    /// Record-focused Desk search.
    Records,
    /// Retrieval favoring source diversity and citation quality.
    AiRetrieval,
    /// Community discussion and chat search.
    Community,
}

/// Canonical source-aware chunk persisted in derived search storage.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchChunk {
    /// Stable content-derived chunk id.
    pub id: SearchChunkId,
    /// Searchable source family.
    pub source_type: SearchSourceType,
    /// Stable domain object that produced the chunk.
    pub source_id: Uuid,
    /// Parent material when the source is material-backed.
    pub material_id: Option<MaterialId>,
    /// Immutable normalized revision when available.
    pub revision_id: Option<DocumentRevisionId>,
    /// Community Space for social sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_space_id: Option<Uuid>,
    /// Shared material for anchored social sources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_material_id: Option<Uuid>,
    /// Source object revision or immutable version marker.
    pub source_version: String,
    /// Indexed field.
    pub field: SearchField,
    /// Result title.
    pub title: String,
    /// Structural path used for context and grouping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub heading_path: Vec<String>,
    /// Plain untrusted text.
    pub text: String,
    /// Optional language hint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Normalized user tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Source lifecycle used by record-scoped retrieval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Last primary-object update used by bounded record scopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<TimestampMs>,
    /// Exact source-backed anchor when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// SHA-256 hash of the indexed text.
    pub text_hash: String,
    /// Rules that produced this chunk.
    pub chunker_version: String,
}

impl SearchChunk {
    /// Build a stable chunk id from source identity, revision, field and ordinal.
    #[must_use]
    pub fn stable_id(
        source_type: SearchSourceType,
        source_id: Uuid,
        source_version: &str,
        field: SearchField,
        ordinal: usize,
    ) -> SearchChunkId {
        content_hash(
            format!(
                "{}:{source_id}:{source_version}:{}:{ordinal}",
                source_type.as_str(),
                field.as_str()
            )
            .as_bytes(),
        )
    }
}

/// User-facing search request after query-parameter decoding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchRequest {
    /// Plain user query.
    pub query: String,
    /// Authorized source scope.
    #[serde(default)]
    pub scope: SearchScope,
    /// Optional source-family allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_types: Vec<SearchSourceType>,
    /// Optional normalized tag filters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Optional parent-material allowlist applied before retrieval disclosure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub material_ids: Vec<MaterialId>,
    /// Optional source lifecycle allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statuses: Vec<String>,
    /// Optional inclusive primary-object update range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_range: Option<SearchUpdatedRange>,
    /// Ranking profile.
    #[serde(default)]
    pub ranking: SearchRankingProfile,
    /// Opaque continuation cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Requested bounded page size.
    pub limit: usize,
}

/// Inclusive primary-object update range for record retrieval.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchUpdatedRange {
    /// Earliest accepted update timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<TimestampMs>,
    /// Latest accepted update timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<TimestampMs>,
}

/// Open target shared by global, Reader and future Desk search surfaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchOpenTarget {
    /// Open a material overview.
    Material {
        /// Target material.
        material_id: MaterialId,
    },
    /// Open an immutable reader revision at an optional exact anchor.
    Reader {
        /// Target material.
        material_id: MaterialId,
        /// Target immutable revision.
        revision_id: DocumentRevisionId,
        /// Exact target when the source has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        anchor: Option<Box<Anchor>>,
    },
    /// Open a record through its parent material.
    Annotation {
        /// Parent material.
        material_id: MaterialId,
        /// Stable annotation id.
        annotation_id: Uuid,
    },
    /// Open a learning item.
    LearningItem {
        /// Stable item id.
        item_id: Uuid,
    },
    /// Open a saved typed AI artifact.
    AiArtifact {
        /// Stable artifact id.
        artifact_id: Uuid,
    },
    /// Open a shared material discussion or published highlight.
    CommunityMaterial {
        /// Community Space.
        space_id: Uuid,
        /// Shared material identity.
        shared_material_id: Uuid,
        /// Optional exact social object.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<Uuid>,
    },
    /// Open Community chat at one message.
    CommunityChat {
        /// Community Space.
        space_id: Uuid,
        /// Stable message identity.
        message_id: Uuid,
    },
}

/// Explainable components of the fused ranking score.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchScore {
    /// Raw Tantivy BM25 candidate score.
    pub bm25: f32,
    /// Cosine similarity from the configured fastText model.
    pub fasttext: f32,
    /// Deterministic exact/title/tag/current-material boost.
    pub boost: f32,
    /// Final sortable score.
    pub total: f32,
}

/// One permission-filtered search result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    /// Matched chunk id.
    pub chunk_id: SearchChunkId,
    /// Searchable source family.
    pub source_type: SearchSourceType,
    /// Stable domain object.
    pub source_id: Uuid,
    /// Optional parent material.
    pub material_id: Option<MaterialId>,
    /// Result title.
    pub title: String,
    /// Structural path.
    pub heading_path: Vec<String>,
    /// Plain-text snippet; clients must not treat it as trusted markup.
    pub snippet: String,
    /// Exact open target.
    pub open_target: SearchOpenTarget,
    /// Explainable score components.
    pub score: SearchScore,
}

/// Cursor-paginated search response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchPage {
    /// Results in deterministic fused-score order.
    pub items: Vec<SearchResult>,
    /// Opaque cursor for the next page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Index generation used for this page.
    pub index_generation: u64,
}

/// Retrieval context policy applied after ranking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievalContextPolicy {
    /// Maximum UTF-8 bytes across returned chunk text.
    pub max_bytes: usize,
    /// Maximum chunks from one source object.
    pub max_chunks_per_source: usize,
    /// Whether neighbouring source context may be included.
    #[serde(default)]
    pub include_surrounding_context: bool,
}

/// Permission-aware retrieval request consumed by AI orchestration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievalRequest {
    /// Search request and ranking filters.
    pub search: SearchRequest,
    /// Number of chunks requested.
    pub top_k: usize,
    /// Bounded context policy.
    pub context_policy: RetrievalContextPolicy,
}

/// One source-backed chunk returned to AI orchestration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RetrievedChunk {
    /// Matched chunk id.
    pub chunk_id: SearchChunkId,
    /// Plain untrusted source text.
    pub text: String,
    /// Search source family.
    pub source_type: SearchSourceType,
    /// Stable domain object.
    pub source_id: Uuid,
    /// Optional parent material.
    pub material_id: Option<MaterialId>,
    /// Human-visible title.
    pub title: String,
    /// Structural path.
    pub heading_path: Vec<String>,
    /// Exact source citation compatible with explicit-context AI flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citation: Option<SourceCitation>,
    /// Explainable score components.
    pub score: SearchScore,
    /// Exact open target.
    pub open_target: SearchOpenTarget,
}

/// Public lifecycle of account search derived state.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchIndexState {
    /// Index and fastText model are ready for queries.
    Ready,
    /// Some sources are searchable while updates remain queued or unavailable.
    Partial,
    /// A full derived-state rebuild is active.
    Rebuilding,
    /// Search cannot safely serve ranked results.
    Failed,
}

/// Permission-scoped operational status without private query or content text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchStatus {
    /// Current lifecycle.
    pub state: SearchIndexState,
    /// Tantivy schema version.
    pub index_version: String,
    /// Source-aware chunker version.
    pub chunker_version: String,
    /// Configured fastText model identity.
    pub model_version: String,
    /// Current committed index generation.
    pub index_generation: u64,
    /// Indexed source documents owned by the account.
    pub document_count: u64,
    /// Indexed chunks owned by the account.
    pub chunk_count: u64,
    /// Indexed source documents that currently contain no searchable text.
    pub no_text_document_count: u64,
    /// Queued or running indexing work.
    pub pending_jobs: u64,
    /// Stable redacted failure code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
}

/// Response returned after requesting an account-scoped full rebuild.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SearchRebuildReceipt {
    /// Common runtime job id.
    pub job_id: Uuid,
    /// Resulting public state.
    pub state: SearchIndexState,
}

/// Validation failures at the shared search contract boundary.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum SearchContractError {
    /// Query is blank.
    #[error("search query must not be empty")]
    EmptyQuery,
    /// Query exceeds the public byte limit.
    #[error("search query exceeds the byte limit")]
    QueryTooLarge,
    /// Requested page or retrieval bound is invalid.
    #[error("search result limit is outside policy")]
    InvalidLimit,
    /// Retrieval context policy is invalid.
    #[error("retrieval context policy is outside policy")]
    InvalidContextPolicy,
}

impl SearchRequest {
    /// Normalize user filters and validate public bounds.
    ///
    /// # Errors
    ///
    /// Returns [`SearchContractError`] when query or page bounds are invalid.
    pub fn normalize_and_validate(&mut self) -> Result<(), SearchContractError> {
        self.query = self.query.trim().to_owned();
        if self.query.is_empty() {
            return Err(SearchContractError::EmptyQuery);
        }
        if self.query.len() > MAX_SEARCH_QUERY_BYTES {
            return Err(SearchContractError::QueryTooLarge);
        }
        if !(1..=MAX_SEARCH_RESULTS).contains(&self.limit) {
            return Err(SearchContractError::InvalidLimit);
        }
        self.source_types.sort_unstable_by_key(|kind| kind.as_str());
        self.source_types.dedup();
        self.tags = self
            .tags
            .iter()
            .map(|tag| tag.trim().to_lowercase())
            .filter(|tag| !tag.is_empty())
            .collect();
        self.tags.sort();
        self.tags.dedup();
        self.material_ids.sort_unstable();
        self.material_ids.dedup();
        self.statuses = self
            .statuses
            .iter()
            .map(|status| status.trim().to_lowercase())
            .filter(|status| !status.is_empty())
            .collect();
        self.statuses.sort();
        self.statuses.dedup();
        if self.updated_range.is_some_and(|range| {
            range
                .from
                .is_some_and(|from| range.to.is_some_and(|to| from > to))
        }) {
            return Err(SearchContractError::InvalidContextPolicy);
        }
        Ok(())
    }
}

impl RetrievalRequest {
    /// Validate search and bounded context policy.
    ///
    /// # Errors
    ///
    /// Returns [`SearchContractError`] when any public bound is invalid.
    pub fn normalize_and_validate(&mut self) -> Result<(), SearchContractError> {
        self.search.ranking = SearchRankingProfile::AiRetrieval;
        self.search.normalize_and_validate()?;
        if !(1..=MAX_RETRIEVED_CHUNKS).contains(&self.top_k) {
            return Err(SearchContractError::InvalidLimit);
        }
        if !(1..=1_000_000).contains(&self.context_policy.max_bytes)
            || !(1..=self.top_k).contains(&self.context_policy.max_chunks_per_source)
        {
            return Err(SearchContractError::InvalidContextPolicy);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_id_changes_with_source_revision_and_ordinal() {
        let source_id = Uuid::nil();
        let first = SearchChunk::stable_id(
            SearchSourceType::Material,
            source_id,
            "revision-a",
            SearchField::Body,
            0,
        );
        let second = SearchChunk::stable_id(
            SearchSourceType::Material,
            source_id,
            "revision-b",
            SearchField::Body,
            0,
        );
        let third = SearchChunk::stable_id(
            SearchSourceType::Material,
            source_id,
            "revision-a",
            SearchField::Body,
            1,
        );

        assert_ne!(first, second);
        assert_ne!(first, third);
    }

    #[test]
    fn search_request_normalizes_filters() -> Result<(), SearchContractError> {
        let mut request = SearchRequest {
            query: "  память  ".to_owned(),
            scope: SearchScope::Personal,
            source_types: vec![SearchSourceType::Note, SearchSourceType::Note],
            tags: vec!["  Rust ".to_owned(), "rust".to_owned()],
            material_ids: Vec::new(),
            statuses: Vec::new(),
            updated_range: None,
            ranking: SearchRankingProfile::Global,
            cursor: None,
            limit: 20,
        };

        request.normalize_and_validate()?;

        assert_eq!(request.query, "память");
        assert_eq!(request.source_types, vec![SearchSourceType::Note]);
        assert_eq!(request.tags, vec!["rust"]);
        Ok(())
    }
}
