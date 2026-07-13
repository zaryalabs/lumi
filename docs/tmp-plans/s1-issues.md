# S1 Web EPUB Reader: implementation issues

Status: `active`

Backlog продолжает
[`s1-web-epub-reader.md`](s1-web-epub-reader.md) после завершения Этапа 0.
Registry IDs стабильны по
[`../systems/feature-registry.md`](../systems/feature-registry.md) и должны
оставаться в title или body GitHub issue/PR.

## [S1-01. Persistent account slice](https://github.com/zaryalabs/lumi/issues/1)

Title: `[ACC-001][ACC-002][ACC-003][SYNC-002] S1: persistent account slice`

Depends on: ADR 0003, ADR 0004.

Scope:

- SQLx migrations и account-scoped PostgreSQL repositories;
- Ed25519 registration/login, challenge replay protection, secure sessions,
  CSRF, device registration/revocation;
- personal sync space и transactional `sync_changes` append;
- минимальный registration/login/session-expired UI.

Acceptance:

- account/session переживают restart, а revoked session не проходит auth;
- tenant isolation tests не позволяют читать/менять чужие records;
- duplicate idempotency key и stale object revision детерминированы.

Registry: `ACC-001`, `ACC-002`, `ACC-003`, `ACC-004`, `API-001`, `API-002`,
`SEC-001`, `SYNC-001`, `SYNC-002`, `QUAL-001`.

## [S1-02. Real EPUB import slice](https://github.com/zaryalabs/lumi/issues/2)

Title: `[FMT-EPUB-001][CORE-008][SEC-002] S1: durable real EPUB import`

Depends on: S1-01, ADR 0005.

Scope:

- multipart upload → source blob → durable import job;
- OCF/OPF/spine/nav/NCX parsing и normalized package persistence;
- source map, resources, diagnostics, retry/cancellation/recovery;
- golden fixtures для supported, malformed и malicious EPUB.

Acceptance:

- supported DRM-free EPUB доступен после server restart;
- malformed/limit-violating EPUB имеет stable failed diagnostic;
- source, package, source map и diagnostics не теряются;
- worker не загружает remote resources и не исполняет active content.

Registry: `FMT-EPUB-001`, `CORE-001`, `CORE-002`, `CORE-003`, `CORE-008`,
`CORE-009`, `CORE-010`, `ACC-005`, `API-002`, `SEC-002`, `QUAL-002`,
`QUAL-006`.

## [S1-03. API-backed library](https://github.com/zaryalabs/lumi/issues/3)

Title: `[CORE-001][ACC-003][QUAL-005] S1: API-backed web library`

Depends on: S1-01, S1-02.

Scope:

- versioned Dioxus API client и direct material routes;
- upload, empty/loading/importing/ready/failed states;
- server-backed list/details/archive/restore/delete/source download;
- retry/session-expired handling и удаление production fixtures.

Acceptance:

- library восстанавливается из `/api/v1` после browser/server reload;
- controls отражают реальные capabilities и command states;
- direct URLs и desktop/mobile role locators покрыты Playwright.

Registry: `CORE-001`, `CORE-011`, `ACC-003`, `API-001`, `API-002`, `SYNC-007`,
`QUAL-005`.

## [S1-04. Working paginated reader](https://github.com/zaryalabs/lumi/issues/4)

Title: `[RD-001][RD-003][RD-004][RD-005] S1: paginated ReadingDocument reader`

Depends on: S1-02, S1-03, ADR 0006.

Scope:

- reader route, `ReadingDocument` loader и DOM platform adapter;
- TOC, internal links, footnotes, history и settings;
- browser-measured `PageMap`, lazy window и cache invalidation;
- restore position через source-backed locator.

Acceptance:

- real EPUB читается от начала до конца desktop/mobile;
- PageMap непрерывен после font/width/viewport/resource changes;
- page number остаётся derived, progress — anchor/locator based.

Registry: `CORE-004`, `CORE-006`, `RD-001`, `RD-003`, `RD-004`, `RD-005`,
`RD-010`, `SPIKE-001`, `QUAL-005`.

## [S1-05. Annotations and progress](https://github.com/zaryalabs/lumi/issues/5)

Title: `[CORE-006][RD-006][RD-007][RD-010] S1: durable annotations and progress`

Depends on: S1-01, S1-04.

Scope:

- browser Selection/Range → полный source-backed anchor;
- highlight/note CRUD, overlays, panel и revision conflicts;
- progress persistence/restore и portable annotation export;
- anchor recovery regression fixtures.

Acceptance:

- position, highlights и notes переживают browser/server restart;
- settings/re-pagination не ломают targets;
- stale edit не перезаписывает note молча;
- export содержит metadata, quote, body и полный anchor JSON.

Registry: `CORE-006`, `CORE-007`, `CORE-011`, `RD-006`, `RD-007`, `RD-009`,
`RD-010`, `SYNC-002`, `SYNC-006`, `QUAL-003`, `QUAL-005`.

## [S1-06. UI/UX convergence](https://github.com/zaryalabs/lumi/issues/6)

Title: `[RD-004][RD-009][SYNC-007] S1: converge Dioxus UI with reader-first prototype`

Depends on: S1-03, S1-04, S1-05.

Scope:

- visual tokens, typography, cards, dialogs и reader chrome;
- honest importing/saving/save-failed/session-expired states;
- mobile panels/bottom sheet, keyboard/focus/accessibility;
- скрытие controls отложенных subsystems.

Acceptance:

- desktop/mobile journeys соответствуют prototype decisions;
- landmarks, roles, labels, keyboard order и contrast проверены;
- UI не показывает сохранение без server acknowledgement.

Registry: `RD-004`, `RD-005`, `RD-009`, `SYNC-007`, `QUAL-005`.

## [S1-07. Hardening and closed beta](https://github.com/zaryalabs/lumi/issues/7)

Title: `[QUAL-002][QUAL-005][QUAL-006] S1: hardening and closed beta readiness`

Depends on: S1-01…S1-06.

Scope:

- Playwright journey register → upload → read → note → reload → export;
- golden EPUB compatibility/security corpus и performance budgets;
- auth isolation/session/CSRF/rate-limit и malicious import suites;
- staging migrations, readiness, metrics, backups/restore и privacy UX.

Acceptance:

- completion criteria основного плана выполнены;
- `make c`, обязательный web E2E и security suites проходят;
- backup restore сохраняет account, material, annotations и changes;
- beta runbook содержит deploy, migrate, rollback и incident diagnostics.

Registry: `QUAL-002`, `QUAL-003`, `QUAL-004`, `QUAL-005`, `QUAL-006`,
`SEC-001`, `SEC-002`, `API-002`, `CORE-012`.
