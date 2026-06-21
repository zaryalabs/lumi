# Единый поиск

Status: draft

## Контекст

Единый поиск в Lumi должен искать по всем meaningful artifacts системы:

- исходные материалы и их текстовые слои;
- главы, страницы, абзацы и chunks;
- заметки, хайлайты, comments и margin notes;
- база знаний;
- summaries, карточки, вопросы и other accepted AI/learning artifacts;
- shared folder comments и activity, к которым у пользователя есть access.

Поиск является не только UI-функцией. Он также дает retrieval layer для RAG-like
ИИ-сценариев: собрать релевантный контекст из книг, заметок и artifacts,
после чего другой слой может использовать его в чате, объяснении или генерации
упражнений.

Базовый retrieval approach для `v01`: **BM25 candidate generation + fastText
rerank**. BM25 дает большой хвост кандидатов по точному лексическому совпадению,
fastText rerank помогает поднять семантически близкие chunks и лучше переживать
словоформы/синонимы. Архитектура должна оставить место для embeddings,
cross-encoder rerankers и future hybrid search.

## Пользовательские сценарии

- Пользователь ищет слово или фразу по всей библиотеке, заметкам и базе
  знаний.
- Пользователь ограничивает поиск типом: книги, PDF, заметки, highlights,
  shared folder, generated artifacts.
- Пользователь ищет внутри текущего материала из reader panel.
- Пользователь открывает search result exactly at anchor: page, paragraph,
  note, highlight или KB heading.
- Пользователь задает вопрос ИИ. AI layer вызывает retrieval, получает top
  chunks и citations.
- Пользователь ищет по большой книге/PDF. Lumi ищет по chunked text layer и
  показывает page/chapter context.
- Пользователь работает offline; local index handles personal data.
- Пользователь работает в web-клиенте; если browser runtime не тянет полный
  local semantic index, web может использовать server-assisted index по своей
  облачной реплике аккаунта.

## Функциональные требования

### Scope

Индексируются:

- `Material` metadata: title, authors, source, tags, language.
- `ReadingDocument` text nodes for reflowable materials.
- PDF/OCR text layers with page anchors.
- Web/Telegram/X normalized text.
- Markdown and `lum` chapters, headings, concepts and glossary.
- Annotations, highlights, notes, voice note transcripts when available.
- KB notes, front matter, wikilinks, tags and attachments text where extracted.
- AI artifacts accepted or visible to the user.
- Learning artifacts: flashcards/questions/explanations where search policy
  allows.
- Shared comments/chat within accessible shared folders.

Не индексируются по умолчанию:

- raw credentials/secrets;
- plugin private data without search capability;
- rejected AI drafts;
- unsupported binary blobs without extracted text.

### Search surfaces

- Global search page.
- Library search/filter.
- Reader in-document search.
- KB search.
- Shared folder search.
- AI retrieval API.

All surfaces should use common indexed chunks and result anchors, but can apply
different filters, boosts and presentation.

### Chunking

Chunking должен быть source-aware:

- reflowable materials: heading/section hierarchy -> paragraphs -> windows;
- PDF: page text -> blocks/paragraphs where available -> page windows;
- notes/KB: heading sections and paragraphs;
- highlights/comments: one artifact as one small chunk, with source context;
- summaries/AI artifacts: section chunks;
- chat/shared comments: message or thread window.

Chunk rules:

- chunk has stable id, source object id, anchor and text hash;
- chunk size target should preserve context, not arbitrary token count only;
- overlaps allowed for long text, but result dedup required;
- chunks store citation metadata: title, chapter/page, anchor, quote preview;
- chunking version is part of index invalidation.

### Ranking

Initial ranking pipeline:

```text
Query
  -> normalization/tokenization
  -> filters and permissions
  -> BM25 top N candidates
  -> fastText query/document vector scoring
  -> score fusion and boosts
  -> dedup/grouping
  -> result snippets and anchors
```

BM25:

- primary candidate generator;
- field boosts for title, headings, tags, note title, exact phrase;
- language-aware tokenization where possible;
- typo/fuzzy search can be added later.

fastText:

- compute vector for chunk from tokens/subwords;
- compute vector for query;
- rerank BM25 candidate tail;
- optionally add semantic candidates from approximate nearest neighbor later,
  but not required for draft.

Score fusion:

- exact title/heading matches get boost;
- personal notes/highlights may get boost for user-facing search;
- current material gets boost for reader search;
- recent/current shared folder context can boost social search;
- AI retrieval should prioritize source diversity and citation quality, not
  just top repeated chunks.

### Retrieval for AI

Search exposes a retrieval API:

```text
retrieve(query, scope, filters, top_k, context_policy)
  -> RetrievedChunk[]
```

Each `RetrievedChunk` includes:

- text;
- source metadata;
- anchor/citation;
- score breakdown;
- surrounding context if allowed;
- content policy flags.

AI layer decides how to pack context into prompts. Search should not call LLM
itself.

### Permissions and privacy

- Search only returns objects user can access.
- Personal search and shared search must not leak private notes into shared
  folder results.
- Shared folder results for material-specific comments require material access
  check described in [`social.md`](social.md).
- External agent retrieval must receive only chunks explicitly included in task
  context policy.

## Нефункциональные требования

- **Local-first.** Personal search works offline from local index.
- **Incremental.** Index updates from change events, not full rebuild each time.
- **Rebuildable.** Index shards are derived data and can be recreated from
  synced state and blobs.
- **Fast enough.** Global search should respond interactively for typical
  libraries; long rebuilds run in background.
- **Explainable.** Debug/result metadata should show why a result matched:
  field, snippet, score components.
- **Portable.** Search index format can be internal; source data must remain
  exportable.
- **Extensible.** Embeddings/vector DB/cross-encoder rerank can be added later
  behind same retrieval contract.

## Модель данных

```text
Source objects
  -> SearchDocument
  -> SearchChunk[]
  -> LexicalIndex
  -> VectorIndex
  -> SearchResult
```

Основные сущности:

- `SearchDocument` - indexed source object: material, note, artifact, comment.
- `SearchChunk` - stable text unit with anchor.
- `SearchField` - title/body/heading/tag/comment/etc.
- `SearchIndexShard` - local index partition by user/space/type.
- `SearchVector` - fastText vector for chunk.
- `SearchQuery` - parsed query + filters.
- `SearchResult` - ranked result with snippet and anchor.
- `IndexJob` - background indexing task.
- `IndexVersion` - schema, chunker and model versions.

Предварительный chunk:

```text
SearchChunk {
  id
  space_id
  source_type
  source_id
  document_revision_id
  anchor
  field
  title
  heading_path
  text
  language
  tags
  permissions
  content_hash
  chunker_version
}
```

## Реализация

### Libraries

Primary candidates:

- `tantivy` for local/server BM25 lexical index in Rust.
- fastText-compatible model/runtime for subword vectors. Exact crate/binding
  needs prototype.
- `whatlang` or similar language detection only if useful for tokenization.
- SQLite tables for index metadata/jobs; index engine stores postings/vectors.

Need prototype for fastText in Rust/Web:

- native desktop/server path can use bindings or compiled library;
- web path may need WASM-compatible implementation/model;
- if fastText runtime is too heavy for web, web can use server-assisted index
  or a lighter local semantic rerank until desktop/mobile catch up.

### Index pipeline

1. Domain object changes or blob text layer becomes available.
2. `IndexJob` created with source id and index version.
3. Extractor builds text fields and source-aware chunks.
4. BM25 document fields are updated.
5. fastText vector computed per chunk.
6. Old chunks for same source/version are removed or superseded.
7. Search metadata stores last indexed revision/hash.

### Query pipeline

1. Parse query string: terms, phrases, filters, tags, type qualifiers.
2. Resolve scope: personal, material, KB, shared folder, AI context.
3. Run BM25 top N.
4. Compute query vector.
5. Rerank candidate chunks by fused lexical + vector score.
6. Group near-duplicate chunks by source/anchor.
7. Build snippets from text and anchor context.
8. Return result list with score breakdown and open target.

### In-document search

Reader search can use:

- direct material-local text index for current `DocumentRevision`;
- global index filtered by `material_id`;
- fallback linear search for small documents before index ready.

PDF search uses `PdfTextLayer` or OCR layer. Without text layer UI shows
"текст не извлечен" and offers OCR/index task where available.

### Index invalidation

Reindex when:

- source object revision changes;
- `DocumentRevision` changes;
- OCR/text extraction revision changes;
- chunker version changes;
- fastText model version changes;
- permissions change for shared content;
- accepted AI artifact changes state.

## Интеграции и зависимости

- **Reader.** Search results open reader at `Anchor`. Reader search uses same
  text layers.
- **Форматы.** Importers provide normalized text and source maps.
- **Синхронизация.** Index is derived local data. Sync delivers source objects;
  indexing rebuilds locally.
- **Веб-аккаунт.** Web может иметь server-assisted index over cloud-backed
  account replica, но desktop/mobile local indexes остаются primary для
  offline/full-copy модели.
- **База знаний.** KB notes and graph metadata are indexed.
- **Obsidian.** Imported/exported Markdown changes trigger KB/search indexing.
- **Learning.** Search can find learning items and supply retrieval context for
  generated questions.
- **ИИ.** AI uses search retrieval, but search does not call LLM.
- **Social.** Search respects shared folder permissions and material ownership
  checks.
- **Плагины.** Plugins may provide text extractors or index fields through
  controlled extension points; they cannot bypass permission filters.

## Альтернативы

- `accepted`: BM25 candidate generation + fastText rerank for `v01`.
- `rejected`: vector-only search. It loses exact matches, titles, tags and
  predictable user search behavior.
- `rejected`: LLM call for every search query. Too slow, expensive and not
  offline-first.
- `rejected`: one global server index only. This breaks local/offline personal
  search and privacy expectations.
- `revisit`: dense embeddings + ANN index. Likely useful later, but fastText
  and BM25 match the current simplicity/portability goal.
- `revisit`: cross-encoder rerank. Better quality for AI retrieval, but
  requires heavier model/runtime.

## Открытые вопросы

- Какой fastText model and runtime использовать для Russian/English mixed
  libraries and web compatibility?
- Каким должен быть default BM25 candidate tail size before rerank?
- Нужно ли хранить vector index locally on mobile or compute semantic rerank
  server-side when allowed?
- Какой query syntax дать пользователю: `tag:`, `type:`, `in:`, quotes?
- Нужно ли индексировать rejected/generated drafts or keep them invisible until
  accepted?
