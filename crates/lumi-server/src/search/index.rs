//! Tantivy BM25 index with explicit owner/scope filters and single-writer updates.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use lumi_core::{SearchChunk, SearchRequest, SearchScope, SearchSourceType};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TantivyDocument, Value, STORED, STRING, TEXT,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term};
use uuid::Uuid;

const WRITER_HEAP_BYTES: usize = 64 * 1024 * 1024;
const MAX_BM25_CANDIDATES: usize = 500;

/// Stored BM25 candidate and its corresponding fastText vector.
pub(crate) struct LexicalCandidate {
    pub(crate) chunk: SearchChunk,
    pub(crate) bm25: f32,
    pub(crate) vector: Vec<f32>,
}

/// One process-wide Tantivy shard with owner terms on every document.
pub(crate) struct SearchIndex {
    index: Index,
    reader: IndexReader,
    writer: Mutex<IndexWriter>,
    fields: SearchFields,
    generation: AtomicU64,
}

impl SearchIndex {
    /// Build an isolated in-memory index for deterministic tests.
    pub(crate) fn memory() -> Result<Self, SearchIndexError> {
        let (schema, fields) = search_schema();
        let index = Index::create_in_ram(schema);
        Self::from_index(index, fields)
    }

    /// Open or create the versioned persistent Tantivy directory.
    pub(crate) fn persistent(path: &Path) -> Result<Self, SearchIndexError> {
        std::fs::create_dir_all(path).map_err(|_| SearchIndexError::Unavailable)?;
        let (schema, fields) = search_schema();
        let index = if path.join("meta.json").exists() {
            let index = Index::open_in_dir(path).map_err(|_| SearchIndexError::InvalidIndex)?;
            if index.schema() != schema {
                return Err(SearchIndexError::SchemaMismatch);
            }
            index
        } else {
            Index::create_in_dir(path, schema).map_err(|_| SearchIndexError::Unavailable)?
        };
        Self::from_index(index, fields)
    }

    fn from_index(index: Index, fields: SearchFields) -> Result<Self, SearchIndexError> {
        let writer = index
            .writer_with_num_threads(1, WRITER_HEAP_BYTES)
            .map_err(|_| SearchIndexError::Unavailable)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|_| SearchIndexError::Unavailable)?;
        Ok(Self {
            index,
            reader,
            writer: Mutex::new(writer),
            fields,
            generation: AtomicU64::new(0),
        })
    }

    /// Atomically replace all chunks for one owner-scoped source object.
    pub(crate) fn replace(
        &self,
        owner_id: Uuid,
        document_key: &str,
        chunks: &[(SearchChunk, Vec<f32>)],
    ) -> Result<u64, SearchIndexError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| SearchIndexError::Unavailable)?;
        writer.delete_term(Term::from_field_text(
            self.fields.document_key,
            document_key,
        ));
        for (chunk, vector) in chunks {
            add_chunk_document(&writer, self.fields, owner_id, document_key, chunk, vector)?;
        }
        writer.commit().map_err(|_| SearchIndexError::Unavailable)?;
        self.reader
            .reload()
            .map_err(|_| SearchIndexError::Unavailable)?;
        Ok(self.generation.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// Replace a complete owner shard in one Tantivy commit.
    pub(crate) fn rebuild<I>(&self, owner_id: Uuid, documents: I) -> Result<u64, SearchIndexError>
    where
        I: IntoIterator<Item = (String, Vec<(SearchChunk, Vec<f32>)>)>,
    {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| SearchIndexError::Unavailable)?;
        writer.delete_term(Term::from_field_text(
            self.fields.owner_id,
            &owner_id.to_string(),
        ));
        for (document_key, chunks) in documents {
            for (chunk, vector) in &chunks {
                add_chunk_document(&writer, self.fields, owner_id, &document_key, chunk, vector)?;
            }
        }
        writer.commit().map_err(|_| SearchIndexError::Unavailable)?;
        self.reader
            .reload()
            .map_err(|_| SearchIndexError::Unavailable)?;
        Ok(self.generation.fetch_add(1, Ordering::AcqRel) + 1)
    }

    /// Return permission-filtered BM25 candidates without constructing snippets.
    pub(crate) fn candidates(
        &self,
        owner_id: Uuid,
        request: &SearchRequest,
        requested_candidates: usize,
    ) -> Result<Vec<LexicalCandidate>, SearchIndexError> {
        let searcher = self.reader.searcher();
        let mut parser = QueryParser::for_index(
            &self.index,
            vec![
                self.fields.title,
                self.fields.heading,
                self.fields.tags,
                self.fields.body,
            ],
        );
        parser.set_field_boost(self.fields.title, 3.5);
        parser.set_field_boost(self.fields.heading, 2.0);
        parser.set_field_boost(self.fields.tags, 2.5);
        let (lexical, _) = parser.parse_query_lenient(&request.query);
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![
            (Occur::Must, lexical),
            (
                Occur::Must,
                term_query(self.fields.owner_id, &owner_id.to_string()),
            ),
        ];
        match request.scope {
            SearchScope::Personal => {}
            SearchScope::Material { material_id } => clauses.push((
                Occur::Must,
                term_query(self.fields.material_id, &material_id.to_string()),
            )),
            SearchScope::Records => clauses.push((
                Occur::Must,
                source_types_query(
                    self.fields.source_type,
                    &[
                        SearchSourceType::Highlight,
                        SearchSourceType::Note,
                        SearchSourceType::MarginNote,
                        SearchSourceType::VoiceTranscript,
                        SearchSourceType::AiArtifact,
                        SearchSourceType::LearningItem,
                    ],
                ),
            )),
        }
        if !request.source_types.is_empty() {
            clauses.push((
                Occur::Must,
                source_types_query(self.fields.source_type, &request.source_types),
            ));
        }
        for tag in &request.tags {
            clauses.push((
                Occur::Must,
                term_query(self.fields.tag_exact, &tag.to_lowercase()),
            ));
        }
        let query = BooleanQuery::new(clauses);
        let top_docs = searcher
            .search(
                &query,
                &TopDocs::with_limit(requested_candidates.min(MAX_BM25_CANDIDATES))
                    .order_by_score(),
            )
            .map_err(|_| SearchIndexError::Query)?;
        let mut candidates = Vec::with_capacity(top_docs.len());
        for (bm25, address) in top_docs {
            let document = searcher
                .doc::<TantivyDocument>(address)
                .map_err(|_| SearchIndexError::InvalidDocument)?;
            let payload = stored_str(&document, self.fields.payload)?;
            let chunk =
                serde_json::from_str(payload).map_err(|_| SearchIndexError::InvalidDocument)?;
            let vector = stored_bytes(&document, self.fields.vector)
                .map(decode_vector)
                .transpose()?
                .unwrap_or_default();
            candidates.push(LexicalCandidate {
                chunk,
                bm25,
                vector,
            });
        }
        Ok(candidates)
    }

    /// Current process-local committed generation.
    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy)]
struct SearchFields {
    document_key: Field,
    chunk_id: Field,
    owner_id: Field,
    source_type: Field,
    material_id: Field,
    title: Field,
    heading: Field,
    tags: Field,
    tag_exact: Field,
    body: Field,
    payload: Field,
    vector: Field,
}

fn search_schema() -> (Schema, SearchFields) {
    let mut builder = Schema::builder();
    let fields = SearchFields {
        document_key: builder.add_text_field("document_key", STRING | STORED),
        chunk_id: builder.add_text_field("chunk_id", STRING | STORED),
        owner_id: builder.add_text_field("owner_id", STRING),
        source_type: builder.add_text_field("source_type", STRING),
        material_id: builder.add_text_field("material_id", STRING),
        title: builder.add_text_field("title", TEXT | STORED),
        heading: builder.add_text_field("heading", TEXT | STORED),
        tags: builder.add_text_field("tags", TEXT),
        tag_exact: builder.add_text_field("tag_exact", STRING),
        body: builder.add_text_field("body", TEXT),
        payload: builder.add_text_field("payload", STORED),
        vector: builder.add_bytes_field("vector", STORED),
    };
    (builder.build(), fields)
}

fn add_chunk_document(
    writer: &IndexWriter,
    fields: SearchFields,
    owner_id: Uuid,
    document_key: &str,
    chunk: &SearchChunk,
    vector: &[f32],
) -> Result<(), SearchIndexError> {
    let payload = serde_json::to_string(chunk).map_err(|_| SearchIndexError::InvalidDocument)?;
    let vector = encode_vector(vector);
    let mut document = TantivyDocument::default();
    document.add_text(fields.document_key, document_key);
    document.add_text(fields.chunk_id, &chunk.id);
    document.add_text(fields.owner_id, owner_id.to_string());
    document.add_text(fields.source_type, chunk.source_type.as_str());
    if let Some(material_id) = chunk.material_id {
        document.add_text(fields.material_id, material_id.to_string());
    }
    document.add_text(fields.title, &chunk.title);
    document.add_text(fields.heading, chunk.heading_path.join(" / "));
    for tag in &chunk.tags {
        document.add_text(fields.tags, tag);
        document.add_text(fields.tag_exact, tag.to_lowercase());
    }
    document.add_text(fields.body, &chunk.text);
    document.add_text(fields.payload, payload.as_str());
    document.add_bytes(fields.vector, &vector);
    writer
        .add_document(document)
        .map_err(|_| SearchIndexError::InvalidDocument)?;
    Ok(())
}

fn term_query(field: Field, value: &str) -> Box<dyn Query> {
    Box::new(TermQuery::new(
        Term::from_field_text(field, value),
        IndexRecordOption::Basic,
    ))
}

fn source_types_query(field: Field, source_types: &[SearchSourceType]) -> Box<dyn Query> {
    Box::new(BooleanQuery::new(
        source_types
            .iter()
            .map(|source_type| (Occur::Should, term_query(field, source_type.as_str())))
            .collect(),
    ))
}

fn stored_str(document: &TantivyDocument, field: Field) -> Result<&str, SearchIndexError> {
    document
        .get_first(field)
        .and_then(|value| value.as_str())
        .ok_or(SearchIndexError::InvalidDocument)
}

fn stored_bytes(document: &TantivyDocument, field: Field) -> Option<&[u8]> {
    document.get_first(field).and_then(|value| value.as_bytes())
}

fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

fn decode_vector(bytes: &[u8]) -> Result<Vec<f32>, SearchIndexError> {
    let chunks = bytes.chunks_exact(4);
    if !chunks.remainder().is_empty() {
        return Err(SearchIndexError::InvalidDocument);
    }
    Ok(chunks
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

/// Tantivy index failure mapped to redacted application errors.
#[derive(Clone, Copy, Debug, Eq, thiserror::Error, PartialEq)]
pub(crate) enum SearchIndexError {
    /// Index path or writer is unavailable.
    #[error("search index is unavailable")]
    Unavailable,
    /// Existing index does not match the current schema.
    #[error("search index schema requires a rebuild")]
    SchemaMismatch,
    /// Existing Tantivy metadata is corrupt or unsupported.
    #[error("search index is invalid")]
    InvalidIndex,
    /// Derived document cannot be encoded or decoded.
    #[error("search index document is invalid")]
    InvalidDocument,
    /// User query could not be executed.
    #[error("search query failed")]
    Query,
}

impl SearchIndexError {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "search_index_unavailable",
            Self::SchemaMismatch => "search_index_schema_mismatch",
            Self::InvalidIndex => "search_index_invalid",
            Self::InvalidDocument => "search_document_invalid",
            Self::Query => "search_query_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use lumi_core::{rich_epub_fixture, SearchRankingProfile};

    use super::*;
    use crate::search::chunker;

    #[test]
    fn owner_filter_excludes_foreign_chunks_before_return() -> Result<(), Box<dyn std::error::Error>>
    {
        let first_owner = Uuid::now_v7();
        let second_owner = Uuid::now_v7();
        let imported = lumi_core::import_epub_fixture(first_owner, &rich_epub_fixture())?;
        let chunks = chunker::reflowable_material(imported.material.id, &imported.package)
            .into_iter()
            .map(|chunk| (chunk, vec![1.0, 0.0]))
            .collect::<Vec<_>>();
        let index = SearchIndex::memory()?;
        index.replace(first_owner, "first", &chunks)?;
        index.replace(second_owner, "second", &chunks)?;
        let request = SearchRequest {
            query: "reader".to_owned(),
            scope: SearchScope::Personal,
            source_types: Vec::new(),
            tags: Vec::new(),
            ranking: SearchRankingProfile::Global,
            cursor: None,
            limit: 20,
        };

        let first = index.candidates(first_owner, &request, 100)?;
        let foreign = index.candidates(Uuid::now_v7(), &request, 100)?;

        assert!(!first.is_empty());
        assert!(foreign.is_empty());
        Ok(())
    }
}
