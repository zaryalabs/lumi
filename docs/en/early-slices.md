# Early Slices

Status: accepted

This document captures the first development slices after the final target
design. It does not replace
[`system-design/feature-registry.md`](system-design/feature-registry.md): the
registry keeps the full searchable feature backlog, while this document defines
the build order for the minimal core product.

## Decision

The first slices should not be an MVP with temporary architecture. They should
be the smallest vertical skeleton of the full system:

1. **S0 Core Architecture Skeleton.** Shared domain contracts, storage,
   import/job pipeline, reader core and web shell without trying to cover the
   full UX.
2. **S1 Web EPUB Reader.** Web version: account, library, DRM-free EPUB import,
   reading, progress, basic highlights and notes.
3. **S2 macOS Desktop Reader.** Desktop client for macOS on the same
   domain/core contracts: local replica, offline reading, sync with the web
   account.
4. **S3 Android Reader.** Android client on the same model: local replica, EPUB
   reading, sync, basic highlights/notes.

After S1-S3, the product grows functionally through the registry: new formats,
search, KB, AI, learning, social, Obsidian, plugins and additional platforms.

Main constraint: early slices may be narrow in user-facing scope, but they must
not create alternative architecture that has to be thrown away later.

## Product Scope

### In Early Core

- One source format: DRM-free reflowable EPUB.
- Web-first account and cloud-backed personal space.
- Library: upload EPUB, see material list, open material, delete/archive
  material.
- Reader: paginated or page-like reading, TOC/navigation, progress, basic
  reader settings.
- Basic annotations:
  - highlight text selection;
  - note attached to highlight or block/location;
  - list of notes/highlights for the current material;
  - persistence across reopen and sync.
- Source-backed anchors using the full target anchor model.
- Import diagnostics visible enough for debugging bad EPUBs.
- Export/download source EPUB and user annotations in a simple portable form.
- Golden fixtures and regression tests for the supported EPUB path.

### Out Of Early Core

- FB2, PDF, web pages, Telegram, X, Markdown import and `lum`.
- AI, agents, MCP bridge, voice transcription and generated artifacts.
- Learning mechanics, challenges, FSRS and explain-back.
- Social shared folders, shared comments and shared highlights.
- Obsidian integration and KB graph.
- Global search / RAG retrieval. Reader-local search can be added only if it is
  cheap and uses the same future search/index contracts.
- Third-party plugin runtime and marketplace.
- EPUB DRM, fixed-layout EPUB fidelity mode and media overlays.
- iOS, Windows, Linux and browser extensions.

## Architecture Guardrails

These are non-negotiable even in S0/S1:

- **No format-specific reader.** EPUB import must produce `Material`,
  `DocumentRevision`, Normalized Content Package and `ReadingDocument`. The
  reader must not render EPUB XHTML/CSS as the product model.
- **No throwaway anchors.** Highlights and notes must not be stored as DOM
  paths, CSS pixels or simple text offsets only. Store the full anchor shape:
  revision, node path, quote, prefix/suffix, content hash and EPUB source
  locator where available.
- **No localStorage-only web product.** Web state must live in account/server
  storage. Browser storage is cache/UI state only.
- **No server-only future lock-in.** Native clients must be able to build local
  full-copy replicas from the same domain objects, blobs and change model.
- **No hidden source loss.** Keep the source EPUB blob, normalized package,
  source map and diagnostics. Derived caches must be rebuildable.
- **No reader core dependency on Dioxus/DOM/WebView.** Dioxus components are
  platform adapters; domain models, anchors and annotation commands stay shared.
- **No special annotation model for web.** Web, macOS and Android use the same
  `Annotation`, `Highlight`, `Note`, `ReadingProgress` and source refs.
- **No ad hoc background work.** EPUB import, package build, indexing stubs and
  export jobs use the common `Job` lifecycle even if only import is active.
- **No fake plugin implementation.** Third-party runtime can wait, but
  `ReadingNode` must already allow typed plugin block placeholders and
  first-party capability routing.
- **No AI/social/learning tables as user-facing features.** It is acceptable to
  reserve module boundaries and enum variants, but inactive subsystems must not
  leak into UX.

## Slice S0: Core Architecture Skeleton

S0 is an implementation foundation, not a public product release.

Goal: create the smallest running vertical path that proves the target
architecture can carry EPUB reader functionality without later rewrite.

### Required Capabilities

- Workspace/module layout for shared Rust domain code, server code and UI
  platform adapters.
- Domain ids, versioned schemas and basic migrations for:
  - account/user;
  - material;
  - document revision;
  - normalized package metadata;
  - blob manifest;
  - annotation/highlight/note;
  - reading progress;
  - job/import status.
- Axum route boundary for auth/account/materials/imports/blobs/reader commands.
- Minimal seed phrase or seed-derived auth prototype, behind the final auth
  boundary. If exact OPAQUE/PAKE choice is not ready, record an ADR and keep the
  verifier/challenge boundary replaceable.
- Content-addressed blob abstraction with local/dev backend and migration path
  to S3-compatible storage.
- Common job lifecycle for import jobs.
- EPUB importer spike that writes a real `DocumentRevision` and minimal
  Normalized Content Package.
- Reader core model with `ReadingDocument`, `ReadingNode`, `Anchor`,
  `Annotation`, `ReadingProgress` commands.
- Web UI shell that can open one imported fixture through the reader adapter.
- Fixture test for one simple EPUB and one EPUB with headings/images/footnotes.

### Explicitly Deferred

- Polished library UI.
- Full account lifecycle and deletion flow.
- Full sync protocol for native clients.
- Full pagination algorithm. S0 can use a simple page-like reader if it keeps
  the `PageMap`/measurement boundary.
- Production auth hardening beyond the selected boundary.

### Exit Criteria

- A DRM-free EPUB fixture imports into `Material -> DocumentRevision ->
  Normalized Content Package -> ReadingDocument`.
- A web reader opens the fixture through the shared reader core.
- A highlight/note can be created against an anchor and survives reload.
- `git diff --check`, unit tests for importer/domain and at least one
  integration test pass.

### Registry Coverage

CORE-001, CORE-002, CORE-003, CORE-004, CORE-006, CORE-008, CORE-009,
CORE-010, API-001, API-002, ACC-001, ACC-003, FMT-EPUB-001, RD-001, RD-006,
RD-007, RD-010, QUAL-001, QUAL-002, QUAL-003, SPIKE-001.

## Slice S1: Web EPUB Reader

S1 is the first user-facing product slice.

Goal: a usable web EPUB reader with basic personal annotations, built on the S0
architecture.

### Required Capabilities

- Create/login web account through the accepted auth path or the S0-auth path
  if an ADR explicitly marks it replaceable.
- Upload DRM-free EPUB through web UI.
- Server-side import job with progress/error states.
- Library view:
  - list materials;
  - open material;
  - show import status;
  - remove/archive material;
  - download source EPUB.
- EPUB reader:
  - open last position;
  - TOC/navigation;
  - basic typography/theme settings;
  - basic page-like navigation;
  - internal links and footnotes as reader-native actions.
- Personal annotations:
  - create/delete highlight;
  - create/edit/delete note attached to selection/location;
  - notes/highlights panel for current material;
  - anchor recovery path ready for future reimport, even if UI repair is later.
- Reading progress persisted in server account state.
- Simple export of annotations with quote, note body, source metadata and
  anchor JSON.
- Basic diagnostics page/panel for failed imports.
- Test coverage for EPUB import, reader commands, annotations and persistence.

### Explicitly Deferred

- Search beyond simple in-document fallback.
- Multi-device native sync UX.
- Offline web reading.
- Account deletion/export bundle beyond simple EPUB/download and annotation
  export.
- Fixed-layout EPUB and DRM.
- AI, learning, social, KB, Obsidian and plugins UI.

### Exit Criteria

- A new user can create an account, upload an EPUB, read it, close the browser,
  return and keep progress/notes/highlights.
- Bad EPUB import returns a clear failed/diagnostic state, not a silent broken
  material.
- Data is stored through the same domain entities planned for native sync, not
  through web-only tables.
- The reader uses `ReadingDocument` and shared anchor/annotation commands.

### Registry Coverage

ACC-001, ACC-002, ACC-003, CORE-011, RD-004, RD-005, RD-009, SYNC-001,
SYNC-002, FMT-EPUB-001, SEARCH-003 as chunking/index stubs only, QUAL-002,
QUAL-005, QUAL-006.

## Slice S2: macOS Desktop Reader

S2 adds the first native full-copy client.

Goal: macOS client that uses the same core and proves local replica/offline
architecture before Android.

### Required Capabilities

- macOS app shell using the chosen desktop path, likely Dioxus Desktop/WebView
  unless the reader prototype rejects it.
- Local SQLite/domain store and local content-addressed blob directory.
- Login/connect to existing web account.
- Bootstrap from cloud account state into local replica.
- Import local DRM-free EPUB file through the same importer.
- Read synced or locally imported EPUB offline after blobs/packages are local.
- Create/edit/delete highlights and notes offline.
- Sync progress/annotations/material metadata back to web account.
- Conflict-safe behavior for concurrent note edits: keep both revisions or
  create explicit conflict object; never lose content silently.
- Local diagnostics for missing blobs, pending sync and failed imports.

### Explicitly Deferred

- Obsidian folder projection.
- Desktop plugins and external command plugins.
- Local AI providers/agents.
- Full private/decentralized mode.
- macOS-specific advanced integrations such as global hotkeys, system share
  extension and Spotlight.

### Exit Criteria

- A material imported on web appears on macOS after sync/bootstrap.
- A material imported on macOS appears on web after sync.
- Highlights, notes and progress sync both ways.
- macOS can open a previously synced EPUB without network.
- The same EPUB fixtures and annotation anchor fixtures pass against desktop
  local store.

### Registry Coverage

SYNC-001, SYNC-002, SYNC-003, SYNC-004, SYNC-006, SYNC-007, RD-001, RD-003,
RD-006, RD-007, RD-010, FMT-EPUB-001, QUAL-004, SPIKE-001.

## Slice S3: Android Reader

S3 brings the same core reader to mobile.

Goal: Android client with local replica, EPUB reading and basic annotations.

### Required Capabilities

- Android app shell using the selected mobile path. Dioxus Mobile/WebView is the
  first candidate, but `SPIKE-002` must validate selection and geometry.
- Login/connect to existing web account.
- Local store and blob/package cache with mobile storage policy.
- Bootstrap library from cloud account.
- Download/open EPUB on demand.
- Optional local EPUB import through file picker/share sheet if platform effort
  is reasonable inside S3.
- Mobile reader UX:
  - page-like reading;
  - TOC/navigation;
  - resume position;
  - basic reader settings;
  - touch text selection where platform supports it.
- Highlights and notes on selected text or fallback block/location anchors if
  text selection is temporarily weaker than desktop/web.
- Offline reading and annotation for downloaded materials.
- Sync progress/annotations/material state with web/macOS.

### Explicitly Deferred

- Android web capture/import browser.
- Voice notes and transcription.
- Push notifications and background sync hardening.
- Advanced gestures, TTS and accessibility polish beyond baseline usable
  reader.
- Full plugin runtime and local AI.

### Exit Criteria

- Android can read an EPUB imported on web/macOS.
- Android-created highlight/note appears on web/macOS after sync.
- Android can read and annotate an already downloaded EPUB offline.
- If text selection is not reliable enough, the limitation is documented and
  block/location notes still use the same anchor model.

### Registry Coverage

SYNC-003, SYNC-004, SYNC-007, RD-001, RD-003, RD-004, RD-005, RD-006, RD-007,
FMT-EPUB-001, QUAL-004, SPIKE-002.

## After S1-S3

After the core reader exists on web, macOS and Android, new work should come
from the registry. Recommended expansion order:

1. **Search and export hardening.** In-document/global search, better
   annotation export, more EPUB fixtures.
2. **Second import format.** PDF if academic/technical reading is the priority,
   or web-reader if article capture is the stronger acquisition path.
3. **Knowledge loop.** KB notes, backlinks and Obsidian export.
4. **AI as optional layer.** Provider abstraction, task queue and reader
   selection actions.
5. **Learning.** Flashcards, chapter questions and explain-back after AI and
   search are stable.
6. **Social.** Shared folders after anchors, sync and permissions are proven.
7. **Plugin/runtime path.** Start with first-party plugin blocks, then move to
   third-party runtime only after contracts are stable.
8. **Remaining platforms.** iOS, Windows and Linux after Android/macOS validate
   the native replica model.

## Slice Rules

When choosing any early task:

- Prefer a thin vertical slice over a broad subsystem rewrite.
- Use registry IDs in issue titles or PR descriptions where possible.
- If a shortcut changes a public/domain contract, write an ADR before merging.
- If a feature needs AI/social/learning/plugin behavior, add a placeholder,
  capability flag or inactive module boundary, not a half-feature in the UI.
- Do not add a second model for materials, anchors, annotations or progress.
- Do not add a platform-specific storage format unless it maps cleanly to the
  shared domain model.
