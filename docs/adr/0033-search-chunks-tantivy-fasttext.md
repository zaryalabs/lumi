# ADR 0033: Source-aware search, Tantivy и fastText rerank

Status: accepted

## Контекст

Web-поиск должен индексировать immutable normalized content и личные записи,
возвращать точные source anchors и обслуживать AI retrieval без отдельного
векторного хранилища или вызова LLM. Индекс является derived data, но изменение
primary object и постановка на индексирование не должны расходиться.

Начальный release scope требует BM25 candidate generation и fastText rerank для
русского корпуса с английской технической лексикой. Нельзя молча продолжать
только с BM25, если модель отсутствует или повреждена.

## Решение

1. Общий контракт `search.contract.v1` определяет `SearchChunk`,
   `SearchRequest`, `SearchResult`, `RetrievedChunk`, `SearchStatus`,
   `SearchScope`, ranking profiles и bounded retrieval context policy.
2. Stable chunk id — SHA-256 от source type/id, source version, field и
   ordinal. `search.chunker.v1` строит:
   - heading-aware chunks для reflowable packages;
   - page/block-aware chunks для PDF text layers;
   - отдельные chunks для highlights, notes, margin notes и принятых Voice Note
     transcripts;
   - только active saved AI artifacts и active learning item revisions.
3. Tantivy `0.26` хранит BM25 postings и stored chunk payload. Каждый документ
   содержит обязательный exact owner term. Owner, material, source type и tags
   входят в BooleanQuery до чтения stored text и построения snippet.
4. fastText-модель читает `finalfusion 0.18` без FFI из стандартного `.bin` или
   finalfusion `.fifu`. Для обычного production profile выбрана модельная семья
   `cc.ru.300`; английские technical tokens сохраняются как отдельные subword
   tokens mixed-language query. Конкретный model artifact/version/checksum
   фиксирует deployment manifest. Модель не коммитится из-за размера и
   лицензии upstream.
5. До загрузки модель проходит SHA-256 verification. Нет path/checksum,
   checksum mismatch и invalid model переводят search status в `failed`;
   `search-query`/`ai-retrieval` capability не публикуется и BM25-only fallback
   не выполняется.
6. Fused score `bm25-fasttext.v1` нормализует BM25, добавляет cosine fastText и
   bounded exact title/tag/current-material boost. AI profile увеличивает вес
   semantic score, после чего retrieval ограничивает chunks per source и общий
   UTF-8 byte budget.
7. PostgreSQL хранит `search_documents`, `search_chunks`,
   `search_index_requests` и `search_account_state`. Tantivy и vectors остаются
   удаляемой derived projection.
8. Domain triggers для material/package, Annotation v2, accepted AI artifact,
   active learning item и accepted Voice Note transcript создают
   `search_index_requests` и общий `Job(kind = search_index)` в той же
   транзакции.
9. Worker использует common fenced `JobRuntime`, finite lease, retry,
   restart recovery и redacted dead-letter diagnostics. Replace/delete
   идемпотентны. Full rebuild сначала отмечает account как `rebuilding`, удаляет
   только derived owner projection и воспроизводит её из PostgreSQL/packages.
10. HTTP boundary:
    - `GET /api/v1/search`;
    - `POST /api/v1/search/retrieve`;
    - `GET /api/v1/search/status`;
    - `POST /api/v1/search/rebuild`.
11. Search возвращает plain-text snippets. AI получает text как untrusted
    source data и совместимый `SourceCitation`; search engine не вызывает
    provider.

## Последствия

- Exact matches остаются объяснимыми через BM25, а morphology/subwords влияют
  только на ограниченный candidate tail.
- Primary CRUD не зависит от доступности index/model; lag и failure видны через
  status/jobs.
- Один Tantivy shard безопасно обслуживает несколько Web accounts благодаря
  обязательному owner term и single-writer discipline.
- Большая upstream model увеличивает cold start/RSS. Для production допустим
  заранее конвертированный/quantized `.fifu` с тем же model identity и
  проверенным quality corpus.
- Native replica сможет реализовать те же DTO/chunker/ranking versions поверх
  локального index path, не синхронизируя postings или vectors.

## Альтернативы

- `rejected`: PostgreSQL `tsvector` как второй lexical engine — расходится с
  offline/native direction и ranking contract.
- `rejected`: vector-only/ANN — хуже exact title/tag/quote behavior и требует
  более тяжёлой model/storage инфраструктуры.
- `rejected`: fastText C binding — добавляет native ABI и unsafe boundary там,
  где pure-Rust reader достаточен.
- `rejected`: BM25-only degraded query — capability выглядела бы готовой при
  фактически другом ranking contract.
- `rejected`: authoritative Tantivy index — source data и rebuild/backup
  semantics должны оставаться в PostgreSQL/packages.

## Совместимость

- Миграция `20260726280000_search_core.sql` additive; domain schemas и
  Annotation/AI/Learning payload не переписываются.
- Tantivy schema, chunker и ranking version участвуют в status/invalidation.
  Несовместимая index schema требует full rebuild, а не in-place mutation.
- Required tests: deterministic chunk ids/windows, reflowable/PDF anchors,
  mixed Cyrillic/English rerank, owner exclusion, transactionally paired
  request/job, accepted-state filtering, rebuild equivalence и executable
  10k/500k performance dataset.

