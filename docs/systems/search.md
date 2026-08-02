# Единый поиск

Status: accepted

## Контекст

Единый поиск в Lumi должен искать по всем meaningful artifacts системы:

- исходные материалы и их текстовые слои;
- главы, страницы, абзацы и chunks;
- заметки, хайлайты, comments и margin notes;
- база знаний;
- summaries, карточки, вопросы и other accepted AI/learning artifacts;
- Community Space comments и chat messages, к которым у пользователя есть
  access.

Поиск является не только UI-функцией. Он также дает retrieval layer для RAG-like
ИИ-сценариев: собрать релевантный контекст из книг, заметок и artifacts,
после чего другой слой может использовать его в чате, объяснении или генерации
упражнений.

До появления serious search ИИ и learning могут работать только с явно
выбранным bounded source scope: selection, anchor, chapter или material
revision. Такой контекст строит общий `SourceContextResolver` непосредственно
из нормализованного документа и source-backed anchors. Он не выполняет
ранжирование по библиотеке, не объявляет capability `ai-retrieval` и не
является скрытым вторым поисковым движком.

Базовый retrieval approach для `v01`: **BM25 candidate generation + fastText
rerank**. BM25 дает большой хвост кандидатов по точному лексическому совпадению,
fastText rerank помогает поднять семантически близкие chunks и лучше переживать
словоформы/синонимы. Архитектура должна оставить место для embeddings,
cross-encoder rerankers и future hybrid search.

## Пользовательские сценарии

- Пользователь ищет слово или фразу по всей библиотеке, заметкам и базе
  знаний.
- Пользователь ограничивает поиск типом: книги, PDF, заметки, highlights,
  Community Space, generated artifacts.
- Пользователь ищет внутри текущего материала из reader panel.
- Пользователь открывает search result exactly at anchor: page, paragraph,
  note, highlight или KB heading.
- Пользователь задает вопрос ИИ. AI layer вызывает retrieval, получает top
  chunks и citations.
- Пользователь ищет по большой книге/PDF. Lumi ищет по chunked text layer и
  показывает page/chapter context.
- Пользователь работает offline; local index handles personal data.
- Пользователь работает в web-клиенте; web использует server-side serious search
  по cloud account state.

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
- Saved/accepted typed AI artifacts visible to the user.
- Learning artifacts: flashcards/questions/explanations where search policy
  allows.
- Shared comments/chat within accessible Community Spaces.

Не индексируются по умолчанию:

- raw credentials/secrets;
- plugin private data without search capability;
- rejected AI drafts;
- unsupported binary blobs without extracted text.

### Search surfaces

Первый personal search slice:

- Global search page.
- Library search/filter.
- Reader in-document search.
- Desk search/filter over records, поддержанные текущим release scope.
- AI retrieval API.

Library search является локальным фильтром уже загруженной библиотеки и явно
отделён от Global Search. Global Search группирует personal и permission-aware
social source types. Открытие результата сериализует полный bounded `Anchor`;
social result дополнительно сохраняет shared material/source или message
identity. Raw ranking score в обычном UI не показывается.

KB search и Community Space search являются отдельными последующими
поверхностями над тем же index/query contract. Их отсутствие не делает
personal search slice частично реализованным.

Все подключенные поверхности используют общие indexed chunks и result anchors,
но могут применять разные filters, boosts и presentation.

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
- recent/current Community Space context can boost social search;
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

`retrieve` применяется для открытого запроса по material/library/record scope.
Selection, chapter и whole-material workflows с детерминированным обходом
могут использовать `SourceContextResolver` без search index. Оба пути
возвращают совместимые source refs и citations, но только indexed path
объявляет `SEARCH-005`/`ai-retrieval`.

### Permissions and privacy

- Search only returns objects user can access.
- Personal search and Community Space search must not leak private notes into
  social results.
- Community Space results for material-specific comments require material access
  check described in [`social.md`](social.md).
- External agent retrieval must receive only chunks explicitly included in task
  context policy.

## Нефункциональные требования

- **Native local-first.** Personal search works offline from local index on
  desktop/mobile once local state is available. Web search is server-side over
  cloud account state.
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

Web personal search foundation реализован в `0.4.0/E3`, а поверхности и MCP
parity — в `0.4.0/E4` по [`ADR 0033`](../adr/0033-search-chunks-tantivy-fasttext.md)
и [`ADR 0034`](../adr/0034-desk-projection-and-search-surfaces.md):

- `search.contract.v1` и `search.chunker.v1` находятся в `lumi-core`;
- Tantivy `0.26` выполняет BM25 с owner/material/type/tag filters до stored
  payload;
- `finalfusion 0.18` читает проверенный fastText `.bin`/`.fifu`;
- `search_index_requests` и common `Job(kind = search_index)` создаются
  транзакционно с domain changes;
- incremental replace/delete, restart recovery и full owner rebuild используют
  один worker/runtime;
- `/api/v1/search`, `/search/retrieve`, `/search/status` и `/search/rebuild`
  являются общим Web/MCP application boundary;
- status различает `ready`/`partial`/`rebuilding`/`failed` и отдельно сообщает
  количество source documents без searchable text;
- Global page, Library form, Reader material search и Desk filter/search
  используют один `SearchRuntime`, exact open targets и типизированные
  reload-safe routes;
- `mcp-tools.v3` добавляет global/material/records search и bounded context
  retrieval с теми же capability, permission и cursor rules.

Отсутствие model/checksum не включает BM25 fallback: status становится
`failed`, а capabilities не публикуются. Desk и primary CRUD при этом остаются
доступны; Search UI показывает failure вместо пустого успешного результата.

### Libraries

Primary candidates:

- `tantivy` for local/server BM25 lexical index in Rust.
- fastText-compatible model/runtime for subword vectors. Exact crate/binding
  needs prototype.
- `whatlang` or similar language detection only if useful for tokenization.
- SQLite tables for index metadata/jobs; index engine stores postings/vectors.

Need prototype for fastText in Rust/native/server:

- native desktop/server path can use bindings or compiled library;
- web path uses server-side search over cloud account state;
- native clients need local runtime/model strategy for offline/full-copy mode.

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
2. Resolve scope: personal, material, KB, Community Space, AI context.
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
- **Desk.** Search может открыть конкретный Desk item, Desk материала или
  сквозное представление с восстановимыми filters/query state.
- **Форматы.** Importers provide normalized text and source maps.
- **Синхронизация.** Index is derived local data. Sync delivers source objects;
  indexing rebuilds locally.
- **Веб-аккаунт.** Web uses server-side serious search over cloud account state.
  Desktop/mobile local indexes remain primary for offline/full-copy modes.
- **База знаний.** KB notes and graph metadata are indexed.
- **Obsidian.** Imported/exported Markdown changes trigger KB/search indexing.
- **Learning.** Search can find learning items and supply retrieval context for
  generated questions.
- **ИИ.** AI uses search retrieval, but search does not call LLM.
- **MCP.** Search and Desk query tools reuse the same permission filters,
  cursor contracts, status and bounded context APIs as Web.
- **Social.** Search respects Community Space permissions and material ownership
  checks.
- **Плагины.** Plugins may provide text extractors or index fields through
  controlled extension points; they cannot bypass permission filters.

## Альтернативы

- `accepted`: BM25 candidate generation + fastText rerank for `v01`.
- `rejected`: vector-only search. It loses exact matches, titles, tags and
  predictable user search behavior.
- `rejected`: LLM call for every search query. Too slow, expensive and not
  offline-first.
- `rejected`: one global server index only for all platforms. This breaks
  native local/offline personal search and future private mode.
- `revisit`: dense embeddings + ANN index. Likely useful later, but fastText
  and BM25 match the current simplicity/portability goal.
- `revisit`: cross-encoder rerank. Better quality for AI retrieval, but
  requires heavier model/runtime.

## Принятые параметры первого среза

- Runtime: pure-Rust `finalfusion`, модельная семья `cc.ru.300`, exact
  deployment checksum; candidate tail ограничен 500 chunks.
- Rejected/candidate/superseded AI artifacts и draft/rejected/archived learning
  items не индексируются.
- Public API принимает explicit `scope`, `type`, `tag`, `material_id`, cursor и
  limit. Расширенный user query syntax остаётся последующим совместимым
  дополнением.
- Web использует server-side index. Native vector storage/runtime выбирается
  вместе с первой full-copy replica, сохраняя текущие DTO и versions.
