# Quality, ADR и compatibility

Status: draft

## Контекст

Lumi имеет несколько risky boundaries: normalized package schema, anchors,
importers, sync, search, AI artifacts, plugins and cloud/native storage. Эти
границы нельзя стабилизировать только обсуждением; для них нужны ADR,
fixtures, compatibility tests and performance budgets.

## ADR policy

ADR обязателен для решений, меняющих:

- Normalized Content Package schema;
- `lum` format schema;
- anchor/source-map selector model;
- sync change/object schema;
- blob protocol and storage policy;
- plugin manifest/runtime/capability contract;
- AI task/artifact schema;
- search retrieval contract;
- account/auth/recovery model.

ADR должен содержать decision, context, consequences, rejected alternatives,
compatibility impact and required fixtures.

## Golden fixtures

Для каждого importer/source family нужен fixture corpus:

- source artifact or captured snapshot;
- expected normalized package manifest;
- expected units/blocks/navigation/resources;
- source map samples;
- expected diagnostics;
- regression annotations and anchor recovery cases.

URL/web fixtures записываются как snapshots. Tests do not depend on live sites.

Required fixture families:

- EPUB: navigation, footnotes, images, tables, CSS edge cases, malformed files.
- FB2: Russian fixtures, encodings, notes, embedded images, malformed XML.
- PDF: text layer, scanned/no-text, page labels, rotations, multi-column.
- Web: Medium/Substack/Habr, docs pages, code/tables/images, bad extraction.
- Telegram: text, forwarded posts, files, batches, media captions.
- X: single post, thread, long post, article, partial/deleted/protected cases.
- Markdown/`lum`: wikilinks, callouts, rich blocks, resources, broken links.

## Performance budgets

Initial budgets are design targets for spikes, not final SLA:

- open already normalized reflowable material: p95 under 300 ms for first
  readable view;
- create annotation/note on native local store: under 50 ms to durable local
  commit;
- web annotation/note server command: interactive perceived latency with
  optimistic UI and durable server ack;
- global search first page: interactive for 10k materials / 500k chunks;
- reader navigation remains smooth with 100k annotations in library and many
  annotations in current material;
- import jobs never block UI and expose stages/progress;
- restart recovers jobs/outbox without manual cleanup.

## Test strategy

- Domain/unit tests: invariants, anchor resolver, merge policies, scheduler,
  query parser, capability checks.
- Import compatibility tests: golden fixtures and snapshots.
- Sync simulation: multiple native replicas, concurrent edits, offline periods,
  retries, duplicates, delete/restore and compaction.
- Browser integration: web commands, reader, PDF.js, CodeMirror, extension
  capture handoff and cloud API flows.
- Native integration: SQLite migrations, filesystem, keychain, blob store,
  audio permissions, deep links.
- Security tests: sanitizer bypass corpus, SSRF, ZIP/XML/PDF fuzzing,
  malicious plugin/MCP schemas.

## Открытые вопросы

- Exact benchmark datasets and thresholds for serious search.
- Which fixtures can be committed under open licenses.
- CI cadence for expensive compatibility/performance suites.
