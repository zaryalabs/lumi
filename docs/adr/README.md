# Architecture Decision Records

ADRs capture durable implementation decisions. Use ADRs for the decision classes
listed in [`../systems/quality.md`](../systems/quality.md),
including schema, anchor, sync,
plugin, AI, search and account/auth boundaries.

Текущие source/reader decisions:

- [`0009`](0009-source-backed-anchor-v2.md) — source-backed anchors;
- [`0010`](0010-web-telegram-source-baseline.md) — общий Web/Telegram baseline;
- [`0011`](0011-beta-operations-and-telegram-webhook.md) — beta operations и
  историческая webhook boundary;
- [`0012`](0012-embedded-telegram-bot-settings.md) — встроенный Telegram
  listener и настройка bot token через UI;
- [`0013`](0013-telegram-composite-import.md) — составной Telegram source и
  durable media groups;
- [`0014`](0014-pdf-fixed-layout-import-and-web-reader.md) — fixed-layout PDF
  import, page anchors и Web renderer;
- [`0015`](0015-lum-portable-package-import.md) — portable `.lum` package
  import и compatibility profile;
- [`0016`](0016-instance-admin-bootstrap.md) — instance-wide роль
  администратора и безопасный bootstrap через публичный auth lookup id;
- [`0017`](0017-telegram-admin-auto-link.md) — автопривязка Telegram-бота к
  администратору без одноразового pairing token;
- [`0018`](0018-markdown-import-compiler.md) — Markdown compiler, extension
  lowering и source locators.
- [`0019`](0019-ai-task-run-artifact-schema.md) — AI task/run/artifact schema,
  claims, idempotency и summary slots;
- [`0020`](0020-common-job-runtime.md) — общий Job Runtime с безопасным
  import-adapter переходом;
- [`0021`](0021-provider-secret-store.md) — account-scoped provider secrets,
  encryption и rotation;
- [`0022`](0022-explicit-source-context.md) — bounded explicit context packs и
  source citations;
- [`0023`](0023-mcp-streamable-http-auth-tools.md) — MCP Streamable HTTP,
  revocable auth, tool schemas и claim fencing;
- [`0024`](0024-derived-material-provenance.md) — provenance производных
  `.lum`-материалов.
- [`0025`](0025-openai-whisper-transcription.md) — встроенная транскрибация
  через OpenAI Audio Transcriptions API и модель `whisper-1`.
- [`0026`](0026-learning-completion-items-sessions.md) — revision-bound
  learning sources, durable completion, versioned items и immutable session
  snapshots.
- [`0027`](0027-fsrs-scheduling-challenges.md) — versioned FSRS schedules,
  ordered hint evidence и bounded Challenges projection.
- [`0028`](0028-learning-ai-evaluation-explain-back.md) — source-backed AI
  drafts, open-answer evaluation и explain-back.
- [`0029`](0029-learning-voice-attachments-transcripts.md) — owner-scoped audio
  attachments, transcript revisions и voice learning lifecycle.
- [`0030`](0030-annotation-v2-rich-reader.md) — совместимый Annotation v2,
  source-backed targets и paint-only rich overlays Reader.
- [`0031`](0031-voice-note-audio-lifecycle.md) — общий owner-scoped audio
  lifecycle для Voice Notes и learning.
- [`0032`](0032-stable-record-links-backlinks.md) — stable `LinkTarget`,
  wikilinks, ambiguous/unresolved states и backlinks.
- [`0033`](0033-search-chunks-tantivy-fasttext.md) — source-aware chunks,
  Tantivy BM25, fastText rerank, общий indexing job и retrieval API.
- [`0034`](0034-desk-projection-and-search-surfaces.md) — rebuildable Desk
  projection, типизированные routes и единые Web/MCP search surfaces.
- [`0035`](0035-record-scoped-rag-global-chat.md) — record-only retrieval,
  visible context и citations в существующем durable global chat.
- [`0036`](0036-community-space-sync-membership-links.md) — граница
  CommunitySpace/SyncSpace, membership, roles и безопасные link invitations.
- [`0037`](0037-material-fingerprints-and-community-claims.md) —
  privacy-protected fingerprints и conservative claims личных копий.
- [`0038`](0038-material-discussions-and-moderation.md) — material-level
  discussions, author CRUD и moderation tombstones.
- [`0039`](0039-community-chat-activity-and-mcp.md) — Space chat, append-only
  activity, bounded polling и social MCP parity.
- [`0040`](0040-shared-reading-social-index-images-and-fingerprint-lifecycle.md)
  — shared anchors/Reader layers, permission-aware social index, Community
  images и автоматический fingerprint lifecycle.

## Template

```markdown
# ADR NNNN: Title

Status: proposed | accepted | rejected | superseded

## Context

What problem or boundary forced the decision?

## Decision

What did we choose?

## Consequences

What gets easier, harder or constrained?

## Alternatives

What did we reject and why?

## Compatibility

What migrations, fixtures or tests are required?
```
