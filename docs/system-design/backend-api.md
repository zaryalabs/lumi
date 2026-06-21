# Backend и API boundaries

Status: draft

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
/api/v1/imports/*
/api/v1/jobs/*
/api/v1/search/*
/api/v1/sync/*
/api/v1/rooms/*
/api/v1/shares/*
/api/v1/providers/*
/api/v1/exports/*
/webhooks/telegram/*
/mcp/*
/ws/sync
/ws/rooms/:id
```

Responsibilities:

- `auth/account/devices` - seed-derived auth flow, sessions, profile, devices.
- `materials/revisions` - cloud-backed web material state and revision metadata.
- `blobs` - upload/download, resumable transfer, checksums, object storage.
- `imports/jobs` - durable work state for imports, AI, indexing, export/delete.
- `search` - serious server-side web search and retrieval API.
- `sync` - native full-copy sync, cursors, changes and snapshots.
- `rooms/shares` - social/shared reading spaces and public/share objects.
- `webhooks/telegram` - Telegram ingestion entrypoint.
- `mcp` - external agent integration with scoped tools.

### Contract rules

- JSON for control plane.
- Binary frames or streaming body for blob chunks and future sync frames.
- `application/problem+json` for errors.
- Request IDs in every request/response.
- Idempotency keys for uploads, imports, mutations and webhooks.
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
UpdateNote(note_id, markdown, expected_revision)
MoveReadingPosition(material_id, locator, intent)
CreateChallenge(scope, options)
SubmitAnswer(attempt_id, item_id, response)
EnqueueAiTask(kind, input_refs, policy)
ExportAccount(options)
```

Native clients can execute analogous commands locally, then sync changes. Web
command success means server durable commit. Native command success means local
durable commit and later sync.

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
