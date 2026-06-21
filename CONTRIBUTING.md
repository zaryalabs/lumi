# Contributing

This repository is moving from accepted system design into implementation. The
rules below keep the project buildable, reviewable and aligned with the target
architecture while the first slices are still small.

## Canonical Sources

- `README.md` explains the current scaffold and local workflow.
- `docs/VISION.md` explains the product direction.
- `docs/system-design/` contains the accepted `v01` technical design.
- `AGENTS.md` contains short operational instructions for agents and
  contributors.
- `Makefile` is the gateway to local tooling.

When a technical or product decision changes, update the relevant document in
`docs/`. If the decision touches one of the boundaries listed in
`docs/system-design/quality.md`, add or update an ADR.

## Development Flow

1. Start from the current main branch.
2. Create a short-lived branch for the change.
3. Read the code and docs around the area you will touch.
4. Make a focused change.
5. Run the relevant local checks.
6. Run `make c` before commit, PR or handoff.

Use branch names that make the work obvious:

```text
feat/s0-domain-contracts
feat/web-reader-shell
fix/api-health-shutdown
docs/local-dev-runbook
chore/tooling-precommit
```

## Commit Style

Use focused commits with concise Conventional Commit-style subjects:

```text
feat: add material domain ids
fix: preserve anchor quote context
docs: clarify reader adapter boundary
chore: add pre-commit quality gate
```

Prefer a body when the reason is not obvious from the diff. Explain tradeoffs,
migration notes or follow-up work there.

## Architecture Principles

Lumi follows a shared-domain architecture. Frameworks and platform adapters
support the product model; they should not define it.

Core boundaries:

- Shared Rust domain contracts are platform-independent.
- Axum owns explicit system API routes.
- Dioxus components are UI/platform adapters.
- Web is cloud-backed for the first target.
- Future desktop and mobile clients must be able to use full-copy local
  replicas built from the same domain contracts.

Implementation guidance:

- Keep domain types independent from HTTP handlers, SQL rows and UI components.
- Keep transport contracts explicit and versionable.
- Keep persistence details behind repository/service boundaries.
- Keep reader core free of DOM, WebView and Dioxus renderer-specific types.
- Prefer small modules with clear ownership over generic utility layers.
- Add abstractions only when they protect a real boundary or remove real
  duplication.

## Planned Repository Shape

The implementation should grow toward this shape:

```text
crates/
  lumi-core/          shared ids, schemas, commands and reader contracts
  lumi-import/        importers and normalized package builders
  lumi-reader/        reader core, anchors, annotations and page-map logic
  lumi-server/        Axum API, jobs and cloud-backed web state
  lumi-worker/        background import/index/export jobs
apps/
  web/                Dioxus Web client and reader adapter
  desktop/            future Dioxus Desktop/WebView client
  mobile/             future Dioxus Mobile/WebView client
tests/
  e2e/                Playwright browser tests
docs/
```

Names can change when implementation proves a better boundary, but the shared
domain/server/UI adapter split should remain.

## Rust Standards

Expected baseline:

```text
cargo fmt --all -- --check
cargo clippy -p lumi-core -p lumi-server --all-targets -- -D warnings
cargo test -p lumi-core -p lumi-server
```

Web/Dioxus checks additionally need `wasm32-unknown-unknown` and `dx`:

```text
cargo clippy -p lumi-web --no-default-features --features check -- -D warnings
cargo clippy -p lumi-web --target wasm32-unknown-unknown --no-default-features --features web -- -D warnings
dx build --web
```

Preferred deeper tooling:

- `cargo-nextest` for Rust tests;
- `cargo audit` for vulnerability checks;
- `cargo deny` for dependency policy;
- `taplo-cli` for TOML formatting/linting.

Use `make` targets instead of calling these directly in routine workflows.

## Web Standards

The web app follows `docs/system-design/reader-architecture.md`:

- Dioxus Web is the first platform adapter.
- Browser layout/selection/accessibility engines should do low-level text work.
- Reader core stays platform-independent.
- UI must expose semantic landmarks, headings, labels and role-addressable
  controls so Playwright and agents can inspect flows reliably.

## Testing Expectations

Match test scope to risk:

- Domain logic needs focused unit tests.
- API routes need router tests without binding sockets when possible.
- Import compatibility needs golden fixtures and snapshots.
- Browser-critical user workflows need Playwright coverage.
- Native storage/sync paths need integration tests once those clients exist.

Tests should prove behavior at the boundary where the risk exists. Avoid tests
that only mirror implementation details.

## Documentation Expectations

Update docs when a change affects:

- architecture;
- product behavior;
- setup or local workflow;
- command names;
- API/protocol contracts;
- quality gates.

Keep temporary implementation notes in `docs/tmp-plans/`. Promote durable
decisions into `docs/system-design/`, `docs/adr/` or a runbook.

## Local Quality Gate

Run:

```text
make c
```

before commit, PR or handoff after code changes.

The gate should stay strict for implemented stacks and explicit about skipped
stacks. A target may skip only when the required local tool or platform target
is missing, and it must print a clear message.
