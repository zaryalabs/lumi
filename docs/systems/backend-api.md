# Backend и API boundaries

Status: accepted

## Контекст

Первый target Lumi - cloud-backed web application. Поэтому backend не является
тонким sync relay для web: он хранит account state, normalized packages, blobs,
jobs, server-side search indexes and social state. Для desktop/mobile backend
также служит sync endpoint, blob storage, account/device bootstrap and shared
coordination layer.

Dioxus Fullstack/server functions можно использовать для UI-specific calls, но
системные контракты должны быть явными Axum routes with versioning.

## Функциональные требования

### Route groups

```text
/api/v1/auth/*
/api/v1/account/*
/api/v1/devices/*
/api/v1/materials/*
/api/v1/revisions/*
/api/v1/blobs/*
/api/v1/audio/attachments/*
/api/v1/links/*
/api/v1/imports/*
/api/v1/jobs/*
/api/v1/search/*
/api/v1/desk/*
/api/v1/providers/*
/api/v1/ai/conversations/*
/api/v1/ai/tasks/*
/api/v1/ai/artifacts/*
/api/v1/ai/summaries/*
/api/v1/mcp/connections/*
/api/v1/learning/items/*
/api/v1/learning/sessions/*
/api/v1/learning/challenges/*
/api/v1/learning/schedules
/api/v1/learning/settings
/api/v1/sync/*
/api/v1/spaces/*
/api/v1/shares/*
/api/v1/settings/telegram
/api/v1/exports/*
/mcp/*
/ws/sync
/ws/spaces/:id
```

Responsibilities:

- `auth/account/devices` - seed-derived auth flow, sessions, profile, devices.
- `materials/revisions` - cloud-backed web material state and revision metadata.
- `materials/*/annotations` - owner-scoped Annotation v2 CRUD, навигация и
  portable export с optimistic revision и идемпотентными мутациями.
- `blobs` - generic upload/download, resumable transfer, checksums, object
  storage and reusable attachment references.
- `audio/attachments` - generic owner-scoped attachment complete/play/delete
  для learning и Voice Notes; playback поддерживает safe byte ranges.
- `links` - suggestions, explicit stable-target resolution, owner-scoped
  backlinks и rebuild derived link projection для Annotation v2.
- `imports/jobs` - one durable execution runtime for imports, AI, indexing,
  transcription, fingerprints, export/delete and other typed job kinds.
- `search` - serious server-side web search and retrieval API.
- `desk` - account/material-centered read projections, filters and stable
  navigation targets over records, learning state and saved artifacts.
- `providers` - account-scoped AI provider state и write-only credential
  lifecycle; secret value не возвращается.
- `ai/conversations` - chat history, messages, generations, streaming,
  cancellation и reconnect.
- `ai/tasks/artifacts/summaries` - durable background operations, typed
  results, summary slots и derived-material workflows.
- `mcp/connections` - authenticated Web management revocable MCP connections;
  сам agent transport остается на `/mcp`.
- `learning/items/sessions/challenges/schedules/settings` - account-owned
  versioned exercises, immutable session snapshots, deterministic attempts,
  ordered assistance evidence, versioned FSRS state и bounded due projection.
- `materials/*/reading-completions` и `materials/*/learning-offer` - durable
  completion before optional one-shot Reader offer; learning failure не меняет
  reading progress.
- `sync` - native full-copy sync, cursors, changes and snapshots.
- `spaces/shares` - User Space social projection, Community Spaces, membership,
  link access, material sharing and collaborative reading objects.
- `settings/telegram` - admin-only instance-wide bot configuration and
  listener status.
- `settings/telegram` одновременно определяет администратора-владельца
  embedded transport; отдельного provider pairing API нет.
- `mcp` - account-scoped external agent integration с покрытием product user
  application commands и AI task queue по контракту [`mcp.md`](mcp.md).

### Contract rules

- JSON for control plane.
- Binary frames or streaming body for blob chunks and future sync frames.
- `application/problem+json` for errors.
- Request IDs in every request/response.
- Idempotency keys for uploads, imports and mutations.
- Cursor pagination for list/change APIs.
- Explicit body size limits per route.
- Stable API version in path.
- Server capabilities endpoint so clients can hide unavailable features.

### Command semantics

Web writes through server-side application commands:

```text
ImportMaterial(source)
CreateHighlight(material_id, target, style)
CreateMarginNote(material_id, target, body)
UpdateAnnotation(annotation_id, target, payload, metadata, expected_revision)
MoveReadingPosition(material_id, locator, intent)
CreateChallenge(scope, options)
SubmitAnswer(attempt_id, item_id, response)
EnqueueAiTask(kind, input_refs, policy)
ExportAccount(options)
```

Native clients can execute analogous commands locally, then sync changes. Web
command success means server durable commit. Native command success means local
durable commit and later sync.

Annotation v2 использует единый payload для highlight, note и будущего voice
note. `target` принимает `text_range`, `block`, `section`, `document` или
`page_area`; metadata включает title, ordered tags, status и optional relation.
Старые payload без v2-полей декодируются как active text-range annotation.
Полный контракт и migration policy закреплены в
[`ADR 0030`](../adr/0030-annotation-v2-rich-reader.md).

Voice Note создаётся только после completed generic upload и
`AudioAttachment`; audio bytes не входят в annotation payload. Внутренние
`[[wikilinks]]` сохраняют raw Markdown, а после разрешения — stable
`LinkTarget`. Durable contracts закреплены в
[`ADR 0031`](../adr/0031-voice-note-audio-lifecycle.md) и
[`ADR 0032`](../adr/0032-stable-record-links-backlinks.md).

Search foundation публикует:

```text
GET  /api/v1/search?q=&scope=&type=&material_id=&tag=&cursor=&limit=
POST /api/v1/search/retrieve
GET  /api/v1/search/status
POST /api/v1/search/rebuild
```

Query/retrieve начинают с authenticated account scope. Stored snippets читаются
только после owner/material/type/tag filtering. Retrieval возвращает bounded
`RetrievedChunk[]` и compatible source citations, но не вызывает AI provider.
Schema, jobs и model failure behavior закреплены в
[`ADR 0033`](../adr/0033-search-chunks-tantivy-fasttext.md).

Record-scoped RAG не добавляет отдельный answer route. Существующий
`POST /api/v1/ai/conversations/{conversation_id}/messages` принимает
`record_search` attachment, server-side разрешает его через
`/search/retrieve`, сохраняет exact disclosure в message/context pack и
использует прежние generation/SSE/cancel/retry routes. Контракт закреплён в
[`ADR 0035`](../adr/0035-record-scoped-rag-global-chat.md).

Domain requests such as `AiTask`, `IndexRequest`, `TranscriptionRequest` and
`FingerprintRequest` may have their own payload/status tables, but leases,
claim fencing, retry, cancellation, progress and recovery use one common
`Job` engine. A feature must not introduce a second execution lifecycle under
the name `index_jobs`, `fingerprint_jobs` or `transcription_jobs`.

Foundation `0.2.0/A1` реализует этот contract как общий `JobRuntime` с
PostgreSQL adapter для `jobs` и совместимым adapter существующего
`import_jobs`. Import table остаётся authoritative: shadow lifecycle и
двойная запись запрещены. Операционные детали и migration перечислены в
[`../runbooks/ai-persistence.md`](../runbooks/ai-persistence.md).

## Нефункциональные требования

- **Explicit boundary.** Public/system APIs are reviewed contracts, not incidental
  Dioxus function calls.
- **Idempotency.** Retried requests and webhook deliveries must not duplicate
  materials, jobs or blobs.
- **Observability.** Every request/job/import has trace id, stage, status and
  redacted diagnostics.
- **Security.** Route layer enforces auth, access checks, CSRF where applicable,
  size limits and content-type validation.
- **Self-hosting.** Reference deployment needs web app/server, worker,
  PostgreSQL and S3-compatible object storage.

## Альтернативы

- `rejected`: hide sync/blob/jobs/webhooks behind ad hoc UI calls. Это усложнит
  native clients, agents, self-hosting and testing.
- `rejected`: split sync/import/AI/social into microservices from the start.
  One Axum server and one worker binary are enough until measured bottlenecks
  justify separation.
- `accepted`: Dioxus Fullstack can remain for narrow UI-specific calls where it
  does not become the system boundary.
