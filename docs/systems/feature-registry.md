# Регистр функций

Status: accepted

Этот регистр - searchable inventory функций Lumi после финального прохода
проектирования. Он не задает первый срез и не является roadmap. Его задача -
дать stable feature ids, чтобы после реализации первого среза было проще
искать следующую работу, видеть зависимости и быстро переходить к design docs.

## Как пользоваться

- `ID` стабилен внутри проектирования `v01`. Если функция меняет scope,
  обновляем строку, а не переиспользуем ID для другой идеи.
- `Тип` помогает выбирать срезы: `foundation`, `product`, `integration`,
  `extension`, `quality`, `spike`.
- `Зависит от` перечисляет ключевые предшественники, а не полный graph.
- `Документы` указывают primary specs. Детали остаются в исходных документах.

## Foundation

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| CORE-001 | Library `Material` как стабильная запись материала | foundation | ACC-003, SYNC-001 | `normalized-content.md`, `sync.md` |
| CORE-002 | Immutable `DocumentRevision` для каждого успешного импорта | foundation | CORE-001 | `normalized-content.md` |
| CORE-003 | Normalized Content Package для reflowable и fixed-layout материалов | foundation | CORE-002 | `normalized-content.md`, `reader-architecture.md` |
| CORE-004 | `ReadingDocument` reader-facing view для reflowable content | foundation | CORE-003 | `normalized-content.md`, `reader-architecture.md` |
| CORE-005 | `PageFidelityDocument` для PDF и fixed-layout EPUB | foundation | CORE-003 | `reader-architecture.md`, `formats/pdf.md`, `formats/epub.md` |
| CORE-006 | Единая anchor model: node path, quote, context, source locator, page rects | foundation | CORE-003 | `normalized-content.md`, `reading-screen.md` |
| CORE-007 | Anchor recovery и unresolved anchor state после reimport | foundation | CORE-006 | `normalized-content.md`, `reading-screen.md`, `sync.md` |
| CORE-008 | Source provenance и structured import diagnostics | foundation | CORE-002 | `normalized-content.md`, `quality.md` |
| CORE-009 | Content-addressed `Blob`/`BlobManifest` abstraction | foundation | ACC-003, SYNC-001 | `web-account.md`, `sync.md` |
| CORE-010 | Durable `Job` engine для import, indexing, AI, transcription, fingerprinting, export/delete | foundation | API-001 | `backend-api.md`, `ai.md`, `web-account.md` |
| CORE-011 | Export/download личных материалов, notes, blobs and JSON/Markdown bundles | product | CORE-001, CORE-009 | `web-account.md`, `sync.md`, `knowledge-base.md` |
| CORE-012 | Derived data rebuild policy for indexes, caches, page maps and graph | foundation | CORE-003, SYNC-001 | `README.md`, `quality.md`, `search.md` |
| CORE-013 | Bounded explicit source context and citations for material/revision/anchor scopes without a search index | foundation | CORE-006 | `ai.md`, `normalized-content.md` |
| CORE-014 | Reusable audio attachment upload, authorization, retention and transcript references | foundation | CORE-009, CORE-010 | `learning.md`, `reading-screen.md`, `web-account.md` |
| CORE-015 | Stable internal `LinkTarget` for materials, annotations, anchors and future object types | foundation | CORE-006, SYNC-002 | `desk.md`, `knowledge-base.md` |
| API-001 | Versioned Axum API boundaries and route groups | foundation | - | `backend-api.md` |
| API-002 | Idempotent commands for web mutations, imports, jobs and webhooks | foundation | API-001, CORE-010 | `backend-api.md`, `sync.md` |
| SEC-001 | Data classification and privacy modes | foundation | - | `security-privacy.md` |
| SEC-002 | Import security: sanitizer allowlists, size limits, quarantine and SSRF rules | foundation | API-001 | `security-privacy.md`, `quality.md`, `formats/web-reader.md` |
| SEC-003 | Secret storage policy for API keys, OAuth tokens and provider credentials | foundation | ACC-001 | `security-privacy.md`, `ai.md`, `web-account.md` |

## Account And Sync

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| ACC-001 | Seed phrase account creation and seed-derived auth without plaintext seed on server | foundation | API-001, SEC-001 | `web-account.md`, `security-privacy.md` |
| ACC-002 | `WebSession`, `SyncDevice`, revocation and device list | foundation | ACC-001 | `web-account.md`, `sync.md` |
| ACC-003 | Cloud-backed web personal `SyncSpace` as authoritative web state | foundation | ACC-001, CORE-009 | `web-account.md`, `sync.md` |
| ACC-004 | Account profile and 1:1 User Space identity separated from auth identity | product | ACC-001 | `web-account.md`, `social.md` |
| ACC-005 | Server-side `ImportInbox` for uploads, Telegram, web capture and providers | foundation | ACC-003, CORE-010 | `web-account.md`, `formats/telegram.md`, `formats/web-reader.md` |
| ACC-006 | Account export and deletion workflows with retention/grace policy | product | ACC-003, CORE-010 | `web-account.md`, `sync.md`, `security-privacy.md` |
| ACC-007 | Instance-wide `user/admin` access with deployment bootstrap through public auth lookup id | foundation | ACC-001, ACC-002 | `web-account.md`, `security-privacy.md` |
| SYNC-001 | `SyncSpace` namespaces: personal, community, system/provider and future private relay | foundation | ACC-002 | `sync.md`, `social.md` |
| SYNC-002 | Change log, snapshots, cursors and deterministic reducers | foundation | SYNC-001 | `sync.md` |
| SYNC-003 | Native local store, outbox/inbox and offline full-copy replicas | foundation | SYNC-002 | `sync.md`, `reader-architecture.md` |
| SYNC-004 | Blob storage policy: metadata-only, full library, on-open, manual pin | product | CORE-009, SYNC-003 | `sync.md`, `web-account.md` |
| SYNC-005 | Future private/decentralized mode without cloud replica plaintext content | extension | SYNC-003, SEC-003 | `sync.md`, `web-account.md`, `security-privacy.md` |
| SYNC-006 | Conflict objects, three-way merge path and tombstones | foundation | SYNC-002 | `sync.md`, `knowledge-base.md`, `obsidian.md` |
| SYNC-007 | Sync status UI states: synced, pending, conflicted, missing blobs, failed | product | SYNC-002, SYNC-004 | `sync.md`, `reading-screen.md` |

## Reader

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| RD-001 | Reflowable reader over platform layout engines | product | CORE-004 | `reader-architecture.md`, `reading-screen.md` |
| RD-002 | Fixed-layout/PDF fidelity surface with visual layer, text layer and overlays | product | CORE-005 | `reader-architecture.md`, `formats/pdf.md` |
| RD-003 | Pagination and `PageMap` based on platform measurement | foundation | RD-001 | `reader-architecture.md`, `quality.md` |
| RD-004 | Reader settings: theme, typography, width, spacing and page/scroll mode | product | RD-001 | `reading-screen.md` |
| RD-005 | Navigation: TOC, headings, links, footnotes, bookmarks and history | product | RD-001, CORE-006 | `reading-screen.md`, `reader-architecture.md` |
| RD-006 | Highlights with style/category/privacy | product | CORE-006, SYNC-002 | `reading-screen.md`, `sync.md` |
| RD-007 | Text notes, margin notes and stable links to materials, annotations and anchors | product | RD-006, CORE-015 | `reading-screen.md`, `desk.md` |
| RD-008 | Voice notes attached to anchors with optional later transcription | product | RD-007, CORE-014 | `reading-screen.md`, `ai.md`, `learning.md` |
| RD-009 | Contextual reader panels for current-material notes, search, AI, learning and social layers | product | RD-001 | `reading-screen.md` |
| RD-010 | Reading progress and timeline events for learning analytics | foundation | RD-001, SYNC-002 | `reading-screen.md`, `learning.md` |
| RD-011 | Reader task creation for AI/learning from selected context | foundation | RD-001, AI-002 | `reading-screen.md`, `ai.md`, `learning.md` |
| RD-012 | Plugin block placeholders and first-party reader block routing | extension | PLG-001, RD-001 | `reader-architecture.md`, `plugins.md` |

Реализация `0.4.0/E1–E2` закрывает текущий Web baseline `RD-006`–`RD-008`:
Annotation v2, yellow/bold styles, selection/block/page targets,
title/tags/status, durable CRUD/navigation/export, Voice Notes поверх общего
audio lifecycle, stable `LinkTarget`, wikilinks и backlinks.

## Desk

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| DESK-001 | Material-centered Desk as a top-level Web surface and rebuildable projection | product | CORE-001, RD-006, SYNC-002 | `desk.md` |
| DESK-002 | Cross-material records views with grouping, filters, inline editing and source navigation | product | DESK-001, CORE-006, RD-007 | `desk.md`, `reading-screen.md` |
| DESK-003 | Material learning view with items, attempts, due/missed/skipped/completed state and mastery summary | product | DESK-001, LRN-001, LRN-005 | `desk.md`, `learning.md` |
| DESK-004 | Stable `LinkTarget` resolution behind readable taxonomy paths and record links | foundation | DESK-001, CORE-015 | `desk.md`, `knowledge-base.md` |
| DESK-005 | Saved typed artifacts in Desk while raw AI chat remains outside | product | DESK-001, AI-003, AI-005 | `desk.md`, `ai-chat.md` |

## Formats And Sources

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| FMT-EPUB-001 | DRM-free EPUB importer: OPF, spine, nav, resources and EPUB CFI compatibility | integration | CORE-003, RD-001 | `formats/epub.md` |
| FMT-EPUB-002 | Fixed-layout EPUB detection and fidelity mode with normalized fallback | integration | CORE-005, RD-002 | `formats/epub.md`, `reader-architecture.md` |
| FMT-EPUB-003 | Optional EPUB DRM access providers: LCP and Adobe as capability layer | spike | FMT-EPUB-001, SEC-003 | `formats/epub.md` |
| FMT-FB2-001 | FB2/FB2.zip importer with metadata, bodies, notes and embedded resources | integration | CORE-003, RD-001 | `formats/fb2.md` |
| FMT-PDF-001 | PDF import, page model, thumbnails, outline, links and fidelity reader | integration | CORE-005, RD-002 | `formats/pdf.md` |
| FMT-PDF-002 | PDF text layer for selection, search, export, AI and anchors | foundation | FMT-PDF-001, CORE-006 | `formats/pdf.md`, `search.md` |
| FMT-PDF-003 | OCR as background/plugin task for scanned PDFs | extension | FMT-PDF-002, CORE-010, AI-006 | `formats/pdf.md`, `ai.md`, `plugins.md` |
| FMT-PDF-004 | PDF annotation export sidecar and optional embedded PDF annotations | extension | RD-006, CORE-011 | `formats/pdf.md`, `obsidian.md` |
| FMT-WEB-001 | Cloud browser URL capture to `RenderedPageSnapshot` | integration | ACC-005, CORE-010, SEC-002 | `formats/web-reader.md`, `web-account.md` |
| FMT-WEB-002 | Browser extension capture for current tab or selected fragment | integration | ACC-005, SEC-002 | `formats/web-reader.md`, `plugins.md` |
| FMT-WEB-003 | Mobile WebView capture and explicit regenerate snapshot UX | integration | ACC-005, RD-001 | `formats/web-reader.md` |
| FMT-WEB-004 | Generic article extractor with optional fixture-backed site adapters | foundation | FMT-WEB-001 | `formats/web-reader.md`, `quality.md` |
| FMT-WEB-005 | Web article revisions, diff visibility and anchor migration after recapture | product | FMT-WEB-004, CORE-007 | `formats/web-reader.md`, `normalized-content.md` |
| FMT-TG-001 | Telegram bot auto-link to the configuring instance admin | integration | ACC-001, ACC-005 | `formats/telegram.md`, `web-account.md` |
| FMT-TG-002 | Telegram ingestion buffer for text, forwards, links, files and media captions | integration | FMT-TG-001, CORE-010 | `formats/telegram.md` |
| FMT-TG-003 | Explicit `/batch` mode for grouping several Telegram messages into one material | product | FMT-TG-002 | `formats/telegram.md` |
| FMT-X-001 | X public URL import through official Post Lookup API | integration | ACC-005, SEC-003 | `formats/x.md` |
| FMT-X-002 | X author thread reconstruction with partial-thread diagnostics | integration | FMT-X-001, CORE-008 | `formats/x.md` |
| FMT-X-003 | X long posts and Articles through API payloads | integration | FMT-X-001 | `formats/x.md` |
| FMT-X-004 | X compliance state, rehydration and deletion/tombstone policy | foundation | FMT-X-001, SEC-001 | `formats/x.md`, `security-privacy.md` |
| FMT-X-005 | X browser extension fallback with degraded compliance metadata | extension | FMT-X-004, FMT-WEB-002 | `formats/x.md` |
| FMT-MD-001 | Markdown importer: CommonMark + GFM + `lumi-markdown` extensions | integration | CORE-003, RD-001 | `formats/markdown.md` |
| FMT-MD-002 | Markdown raw HTML safe subset and placeholder policy | foundation | FMT-MD-001, SEC-002 | `formats/markdown.md` |
| FMT-LUM-001 | `lum` source project and `.lum` ZIP package with `lum.toml` manifest | integration | FMT-MD-001, CORE-003 | `formats/lum.md` |
| FMT-LUM-002 | `lum` spine, book graph, resources and stable source map | foundation | FMT-LUM-001, CORE-006 | `formats/lum.md` |
| FMT-LUM-003 | `lum:<block_type>` interactive blocks mapped to first-party plugins | extension | FMT-LUM-002, PLG-002, LRN-002 | `formats/lum.md`, `plugins.md`, `learning.md` |
| FMT-LUM-004 | `lum validate`, `lum pack` and `lum inspect` build/validation tools | quality | FMT-LUM-001, QUAL-001 | `formats/lum.md`, `quality.md` |

## Knowledge, Search And Deferred Obsidian

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| KB-001 | Personal Knowledge Base as Markdown notes with stable internal ids | product | SYNC-002, FMT-MD-001 | `knowledge-base.md` |
| KB-002 | KB-note wikilinks, material links, anchor links, tags and unresolved links | product | KB-001, CORE-015 | `knowledge-base.md`, `formats/markdown.md` |
| KB-003 | Backlinks and graph index across notes, materials, annotations and accepted artifacts | product | KB-001, SEARCH-001 | `knowledge-base.md`, `search.md` |
| KB-004 | Reader action to create KB note or insert block from highlight/note | product | RD-007, KB-001 | `knowledge-base.md`, `reading-screen.md` |
| KB-005 | Generated artifacts as drafts until accepted into KB/search graph | foundation | AI-003, KB-001 | `knowledge-base.md`, `ai.md` |
| OBS-001 | Deferred Desktop: one-way Obsidian Markdown export with Lumi namespace/front matter | integration | KB-001, CORE-011, SYNC-003 | `obsidian.md`, `knowledge-base.md` |
| OBS-002 | Deferred portable fallback: manual Obsidian import/export bundle after the Desktop integration | integration | OBS-001, FMT-MD-001 | `obsidian.md` |
| OBS-003 | Deferred Desktop: explicit two-way folder sync with conflict objects | extension | OBS-001, SYNC-006 | `obsidian.md`, `sync.md` |
| SEARCH-001 | Tantivy-style BM25 index, versioned extractor contract and rebuild pipeline | foundation | CORE-003, SYNC-002 | `search.md` |
| SEARCH-002 | fastText rerank for BM25 candidate tail | foundation | SEARCH-001 | `search.md` |
| SEARCH-003 | Indexed source-aware chunking with anchors, snippets and citation metadata | foundation | CORE-013, SEARCH-001 | `search.md`, `normalized-content.md` |
| SEARCH-004 | Personal search surfaces: global, library, reader and Desk | product | SEARCH-001, RD-009, DESK-001 | `search.md`, `reading-screen.md`, `desk.md` |
| SEARCH-005 | Retrieval API for AI with context policy and citations | foundation | SEARCH-003, AI-001 | `search.md`, `ai.md` |
| SEARCH-006 | Permission-aware search across personal data and Community Spaces | foundation | SEARCH-001, SOC-001 | `search.md`, `social.md` |
| SEARCH-007 | Knowledge Base search surface and graph-aware filters | product | SEARCH-004, KB-001 | `search.md`, `knowledge-base.md` |
| SEARCH-008 | Community Space search surface over shared content | product | SEARCH-006, SOC-001 | `search.md`, `social.md` |

Реализация `0.4.0/E3` закрывает Web foundation `SEARCH-001`–`SEARCH-003` и
`SEARCH-005`: versioned Tantivy index, verified fastText rerank, source-aware
chunks, common indexing jobs, owner-filtered query и bounded retrieval API.
Пользовательские search surfaces `SEARCH-004`, MCP adapters и social
permissions `SEARCH-006` остаются следующими эпиками.

Эпики `0.4.0/E4–E5` закрывают `SEARCH-004` и record-scoped применение
`SEARCH-005`: Global/Library/Reader/Desk используют один query service, а
`record-rag` передаёт bounded owner-filtered records в существующий global
chat с visible disclosure и exact open targets. Community scope
`SEARCH-006` остаётся `0.5.0`.

## Learning And AI

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| LRN-001 | Learning items: quiz, open question, flashcard, cloze, hinted question and reflection | product | CORE-006, SYNC-002 | `learning.md` |
| LRN-002 | Embedded `lum` exercises compiled into learning item templates | product | FMT-LUM-003, LRN-001 | `learning.md`, `formats/lum.md` |
| LRN-003 | Challenges screen for due reviews, chapter tests, missed exercises and drafts | product | LRN-001, LRN-004 | `learning.md`, `../vision.md` |
| LRN-004 | FSRS scheduler behind replaceable `Scheduler` port | foundation | LRN-001 | `learning.md` |
| LRN-005 | Attempts, mastery state and source-backed feedback | product | LRN-001, LRN-004 | `learning.md` |
| LRN-006 | Explain-back learning mechanic with iterative AI feedback over an explicit source scope | product | AI-005, CORE-013 | `learning.md`, `ai.md` |
| AI-001 | AI provider abstraction with OpenRouter/OpenAI-compatible first provider target | foundation | SEC-003 | `ai.md` |
| AI-002 | Durable `AiTask` queue, immediate internal execution, web bulk actions and MCP claims using common `Job` engine | foundation | CORE-010, AI-001 | `ai-task-queue.md`, `ai.md`, `backend-api.md` |
| AI-003 | Versioned typed AI artifact registry and schema validation | foundation | AI-002 | `ai.md`, `knowledge-base.md`, `learning.md` |
| AI-004 | Reader selection actions for ask, explain and summarize over explicit context | product | RD-011, CORE-013 | `ai.md`, `reading-screen.md` |
| AI-005 | Глобальный сворачиваемый ИИ-чат: управление чатами, многошаговый streaming-диалог и передача контекста из Lumi | product | AI-001 | `ai.md`, `ai-chat.md` |
| AI-006 | Voice transcription through OpenAI Audio Transcriptions API (`whisper-1`) as AI/background task over reusable audio attachments | extension | CORE-014, AI-002 | `ai.md`, `learning.md`, `../adr/0025-openai-whisper-transcription.md` |
| AI-007 | Account-scoped MCP interface with target parity for product user operations and AI queue worker tools | extension | AI-002, API-001 | `mcp.md`, `ai.md`, `backend-api.md` |
| AI-008 | Context policy, cost controls, user-visible context and source citations | foundation | AI-001, CORE-013, SEC-001 | `ai.md`, `security-privacy.md` |
| AI-009 | Один active summary на главу/материал и отдельный сокращенный `.lum`-материал со связью с источником | product | AI-003, CORE-002, FMT-LUM-001 | `ai-summaries.md`, `ai.md`, `formats/lum.md` |
| AI-010 | Record-scoped RAG в global chat с explicit filters, visible included context и проверяемыми citations | product | AI-005, AI-008, SEARCH-005, DESK-002 | `ai-chat.md`, `search.md`, `../adr/0035-record-scoped-rag-global-chat.md` |

Реализация `0.3.0/E1–E4` закрывает deterministic baseline `LRN-001`, `LRN-003`,
`LRN-004` и части `LRN-005`: revision-bound ручные items, durable completion,
immutable session snapshots, attempts, ordered hints/source evidence,
versioned FSRS, bounded Challenges, pause/resume/snooze/manual-only, source
jump и reload/resume, а также source-backed AI drafts, open-answer evaluation
и iterative explain-back с self-check fallback. Platform slice `0.3.0/E5`
добавляет `learning-mcp`: list/create-flashcard-task/submit используют те же
owner-scoped learning services и AI queue, capability filtering и idempotency,
что Web. Voice vertical добавляет явную browser recording/preview ветку,
owner-scoped generic audio lifecycle, отдельный encrypted OpenAI BYOK,
server-side `whisper-1`, editable transcript acceptance, retry и retention.
Сервер публикует `learning-core`, `learning-scheduling`, `learning-ai`,
`learning-explain-back`, `learning-audio-attachments`, `learning-voice` и
`learning-mcp` только при готовых prerequisites. Общий mastery projection
остаётся отдельным последующим расширением.

## Social And Plugins

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| SOC-001 | User Space and Community Space product model, community links, membership, roles and unlisted link access | product | ACC-004, SYNC-001 | `social.md`, `sync.md` |
| SOC-002 | Shared material identity and user material claims without distributing source blobs | foundation | SOC-001, CORE-008 | `social.md`, `normalized-content.md` |
| SOC-003 | Versioned content fingerprints for matching copies across supported normalized-text and PDF formats | foundation | CORE-008, SOC-002 | `social.md`, `normalized-content.md` |
| SOC-004 | Shared anchor mapping across users' local copies | product | SOC-002, CORE-007 | `social.md`, `reading-screen.md` |
| SOC-005 | Community Space material comments, shared highlights and Space-level chat separated from personal notes | product | SOC-001, RD-006 | `social.md`, `reading-screen.md` |
| SOC-006 | Privacy, moderation, deletion and quote/copyright limits for Community Spaces | foundation | SOC-001, SEC-001 | `social.md`, `security-privacy.md` |
| SOC-007 | `Share material to Space` from personal material surfaces with deduplication and `UserMaterialClaim` | product | SOC-001, SOC-002, CORE-001 | `social.md`, `reading-screen.md` |
| PLG-001 | Plugin manifest, activation events, commands, settings and capabilities | extension | SEC-001 | `plugins.md` |
| PLG-002 | First-party reader block plugins: math, Mermaid, code, SVG, quiz and flashcard | extension | PLG-001, RD-012 | `plugins.md`, `formats/lum.md`, `reading-screen.md` |
| PLG-003 | WASM processing plugin runtime for importers/extractors/post-processing | extension | PLG-001, SEC-002 | `plugins.md` |
| PLG-004 | Sandboxed UI plugin/runtime model for reader blocks and UI contributions | extension | PLG-001, RD-012 | `plugins.md`, `reader-architecture.md` |
| PLG-005 | Plugin-owned data, sync objects, uninstall safety and migrations | extension | PLG-001, SYNC-002 | `plugins.md`, `sync.md` |
| PLG-006 | Marketplace/trust path: first-party, verified, community and dev packages | extension | PLG-001 | `plugins.md` |

## Quality And Spikes

| ID | Функция | Тип | Зависит от | Документы |
| --- | --- | --- | --- | --- |
| QUAL-001 | ADR policy for schemas, anchors, sync, plugins, AI, search and account/auth | quality | CORE-003, SYNC-002 | `quality.md`, `README.md` |
| QUAL-002 | Golden fixtures for EPUB, FB2, PDF, Web, Telegram, X, Markdown and `lum` | quality | Format importers | `quality.md`, `formats/` |
| QUAL-003 | Anchor recovery regression fixtures | quality | CORE-006, CORE-007 | `quality.md`, `normalized-content.md` |
| QUAL-004 | Sync simulation: replicas, concurrent edits, retries, deletes and compaction | quality | SYNC-002, SYNC-006 | `quality.md`, `sync.md` |
| QUAL-005 | Browser integration tests for web commands, reader, PDF.js and extension handoff | quality | RD-001, API-001 | `quality.md`, `reader-architecture.md` |
| QUAL-006 | Security tests for sanitizer, SSRF, ZIP/XML/PDF fuzzing and plugin/MCP schemas | quality | SEC-002, PLG-001, AI-007 | `quality.md`, `security-privacy.md` |
| SPIKE-001 | Pagination algorithm prototype on books, articles, tables, images and plugin blocks | spike | RD-003 | `reader-architecture.md`, `quality.md` |
| SPIKE-002 | Dioxus Mobile/WebView selection and geometry prototype | spike | RD-001, RD-006 | `reader-architecture.md` |
| SPIKE-003 | PDF engine choice for first web target: PDF.js, PDFium server or PDFium WASM | spike | FMT-PDF-001 | `formats/pdf.md` |
| SPIKE-004 | fastText model/runtime for Russian/English mixed libraries | spike | SEARCH-002 | `search.md` |
| SPIKE-005 | Seed phrase auth protocol: OPAQUE/PAKE vs seed-derived challenge signing | spike | ACC-001 | `web-account.md`, `security-privacy.md` |
| SPIKE-006 | Web cloud browser runtime and sandboxing model | spike | FMT-WEB-001, SEC-002 | `formats/web-reader.md` |
| SPIKE-007 | X Article body availability and compliance recheck interval | spike | FMT-X-003, FMT-X-004 | `formats/x.md` |
| SPIKE-008 | Third-party plugin runtime choice: WASM-only, TypeScript sandbox or hybrid | spike | PLG-003, PLG-004 | `plugins.md` |
