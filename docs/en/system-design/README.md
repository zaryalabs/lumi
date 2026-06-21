# System Design

This directory contains Lumi's technical design: how the product vision in
[`../vision.md`](../vision.md) becomes architecture, data formats,
integrations and implementable subsystems.

We are designing the full product target, not a throwaway MVP or a temporary
slice. Early implementation slices are defined separately in
[`../early-slices.md`](../early-slices.md).

Current document state: **Final v01**. The target architecture is accepted for
planning the first slice. Open questions are implementation, prototype or ADR
tasks; they do not block the target design itself.

## Goal

For `v01`, capture the technical position for every major Lumi direction:

- what the capability must do;
- which user scenarios it covers;
- which data and models are required;
- how it connects to the reader, sync, search, knowledge base, AI layer and
  plugins;
- which decisions are accepted, which alternatives were rejected and which
  questions remain open.

## Design Process

The design moved through several passes:

1. **Pass 1: initial vision.** Capture the first implementation hypothesis for
   each direction without forcing final architecture too early.
2. **Pass 2: subsystem alignment.** Return to documents after neighboring
   areas are designed and tighten contracts, dependencies and constraints.
3. **External review.** Compare the design with the external expert vision,
   especially where stronger architecture variants may exist.
4. **Final v01.** Freeze the target design for slice planning and
   implementation.

The design phase is complete. Work should now proceed through vertical slices,
ADRs for contested implementation choices and implementation.

## Final Composition

The final Lumi composition is built around several cross-cutting contracts:

- **Material -> DocumentRevision -> Normalized Content Package.** Every source
  first becomes an immutable revision and normalized package.
- **Reader-facing views.** Reflowable materials open through
  `ReadingDocument`; PDF and fixed-layout EPUB open through
  `PageFidelityDocument`.
- **Anchor + provenance.** Notes, highlights, search, learning, AI and social
  objects reference source-backed anchors, not DOM paths, pixels or format
  offsets as the only source of truth.
- **Draft-to-accepted flow.** AI output, generated learning items, KB drafts and
  social publication do not become strong knowledge/search/social objects until
  the user accepts them or an explicit policy allows it.
- **Cloud-backed web, full-copy native later.** The first web target stores
  state in a cloud account replica. Desktop/mobile are designed as future
  full-copy replicas. Private/decentralized mode remains a long-term accepted
  requirement.
- **Shared spaces do not distribute private files.** Social features sync
  comments, highlights, activity and material claims, but do not distribute
  source blobs to participants without their own copy or rights.
- **One Job engine.** Imports, indexing, AI, transcription, exports, deletion
  workflows and anchor repair use one durable job/lifecycle contract.
- **Plugin platform as target, first-party first.** Plugins are designed as a
  real extension platform, while early extension points should be validated
  through first-party plugins before a broad third-party runtime.

## Decision Statuses

Use these statuses consistently:

- `draft` - initial hypothesis that still needs discussion;
- `accepted` - target `v01` decision is accepted; open questions inside the
  document remain implementation/prototype questions;
- `revisit` - decision is temporarily accepted but must be revisited after
  neighboring subsystems are designed;
- `rejected` - option was considered and rejected with a reason;
- `open` - question is not decided yet.

## Structure

Each top-level direction has its own file. Nested directions use directories.

```text
docs/en/system-design/
  README.md
  feature-registry.md
  normalized-content.md
  reading-screen.md
  reader-architecture.md
  backend-api.md
  security-privacy.md
  quality.md
  formats/
    epub.md
    fb2.md
    pdf.md
    web-reader.md
    telegram.md
    x.md
    markdown.md
    lum.md
  web-account.md
  sync.md
  knowledge-base.md
  obsidian.md
  search.md
  learning.md
  social.md
  ai.md
  plugins.md
```

## Directions

| Direction | Document | Status |
| --- | --- | --- |
| Feature registry | `feature-registry.md` | `accepted` |
| Normalized content | `normalized-content.md` | `accepted` |
| Reading screen | `reading-screen.md` | `accepted` |
| Reader architecture | `reader-architecture.md` | `accepted` |
| Backend and API boundaries | `backend-api.md` | `accepted` |
| Security and privacy | `security-privacy.md` | `accepted` |
| Quality, ADR and compatibility | `quality.md` | `accepted` |
| EPUB | `formats/epub.md` | `accepted` |
| FB2 | `formats/fb2.md` | `accepted` |
| PDF | `formats/pdf.md` | `accepted` |
| Reader-mode web pages | `formats/web-reader.md` | `accepted` |
| Telegram through bot | `formats/telegram.md` | `accepted` |
| X long posts and threads | `formats/x.md` | `accepted` |
| Markdown | `formats/markdown.md` | `accepted` |
| Custom `lum` format | `formats/lum.md` | `accepted` |
| Web account and cloud replica | `web-account.md` | `accepted` |
| Sync | `sync.md` | `accepted` |
| Knowledge base | `knowledge-base.md` | `accepted` |
| Obsidian integration | `obsidian.md` | `accepted` |
| Search | `search.md` | `accepted` |
| Learning mechanics | `learning.md` | `accepted` |
| Social features | `social.md` | `accepted` |
| AI capabilities | `ai.md` | `accepted` |
| Plugins | `plugins.md` | `accepted` |

## Feature Registry

[`feature-registry.md`](feature-registry.md) is the searchable index of
features and subsystems. After the first slice exists, it helps find the next
user-visible feature, understand dependencies and jump to the source design
docs.

Registry maintenance rule: every new feature, major ADR or substantial scope
change should update the relevant registry row or add a new stable feature id.

[`../early-slices.md`](../early-slices.md) defines the first implementation
slices: core architecture skeleton, web EPUB reader, macOS desktop reader and
Android reader. These slices use registry IDs, but they stay outside
`system-design` because they define development order rather than the complete
functional inventory.

## Composition Model

Functional directions are grouped into four layers:

- **Foundation layer.** Account, sync, blobs, jobs, security, normalized
  content, anchors and API boundaries.
- **Reading layer.** Library/import, reader, annotations, navigation,
  page/fidelity surfaces and reader timeline.
- **Knowledge layer.** Search, KB, learning and AI artifacts, all tied back to
  source refs.
- **Coordination/extension layer.** Social shared spaces, Obsidian projection,
  plugins, external agents and future private/decentralized mode.

When choosing the first or next slice, prefer a vertical user workflow across
several layers over implementing one entire layer. Example:
`web account -> import -> normalized package -> reader -> annotation -> search
index`, then extend the same path into KB, learning, AI or social.

## Direction Document Template

Keep design documents in a consistent shape so decisions are easy to compare
and revisit.

```markdown
# Direction name

Status: draft

## Context

What this direction gives the product and why it matters.

## User Scenarios

- ...

## Functional Requirements

- ...

## Non-Functional Requirements

- ...

## Data Model

Entities, identifiers, relationships and metadata.

## Implementation

Main approach, libraries, services, background jobs, client/server boundaries.

## Integrations and Dependencies

Connections to reader, formats, sync, search, KB, AI and plugins.

## Alternatives

Which options were considered and why they are better or worse.

## Open Questions

- ...
```

## General Principles

- The reader must have one internal display model so notes, highlights, search,
  learning and AI features work consistently across source formats.
- Every importer must create an immutable `DocumentRevision` and internal
  Normalized Content Package. `ReadingDocument` and `PageFidelityDocument` are
  reader-facing view models over that package, not the storage format itself.
- Source materials and user data must remain portable. Lumi should not lock the
  user into closed storage without export.
- The web version is a cloud-backed web application: materials, normalized
  packages, blobs, jobs and search indexes for web live on the server. Browser
  storage may only be a non-authoritative cache.
- Desktop and mobile must receive real local replicas: local storage, local
  blobs/packages, outbox/sync and offline search.
- In the long-term architecture, native clients must support private /
  decentralized mode: the user can disable the web/cloud replica and keep a
  private vault only on their devices, while the server keeps account, device
  registration, encrypted relay/key envelopes, social coordination and
  explicitly shared objects.
- Derived data is not source of truth. Search indexes, thumbnails, page maps,
  backlinks, caches and calculated projections must be rebuildable.
- The AI layer must be replaceable: the user can connect their own key, use a
  built-in subscription or disable AI scenarios.
- Architecture should target a real VS Code/Obsidian-level plugin platform:
  manifest, activation events, commands, UI contributions, capabilities, plugin
  data, trust levels and marketplace path. The roadmap may defer runtime and
  marketplace, but the target design should not be cut down.
