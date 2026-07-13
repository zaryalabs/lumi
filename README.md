# Lumi

Lumi is an open-source app for deliberate reading and learning over materials
the user already chose: books, articles, threads, messages and notes.

The canonical development-facing product direction is described in
[docs/vision.md](docs/vision.md). The accepted technical design for `v01`
lives in [docs/systems](docs/systems).

## Current State

The repository is moving from system design into implementation. The initial
developer scaffold includes:

- a Rust workspace;
- shared domain contracts in `crates/lumi-core`;
- an Axum API skeleton in `crates/lumi-server`;
- a Dioxus web shell in `apps/web`;
- Playwright E2E scaffolding in `tests/e2e`;
- `make` targets and pre-commit hooks for local quality gates.

The current implementation target is the S1 Web EPUB Reader slice from
[docs/early-slices.md](docs/early-slices.md). Persistent accounts and durable
real EPUB import are implemented; the next step is the fully API-backed library.

## Local Setup

Prerequisites:

- Rust stable with `cargo`, `rustfmt` and `clippy`;
- `wasm32-unknown-unknown` Rust target for Dioxus Web;
- Dioxus CLI `dx`;
- Node.js and npm for Playwright tests;
- `pre-commit` for Git hooks.

Bootstrap the local environment:

```sh
make init
```

Run the core local checks:

```sh
make l
make t
make c
```

Run local services from separate terminals:

```sh
make server-r
make web-r
```

Default endpoints:

- API health: `http://127.0.0.1:8080/api/v1/health`
- Web shell: `http://127.0.0.1:5173`

Run the browser E2E scaffold after web dependencies and Dioxus CLI are
available:

```sh
make web-e2e
```

More detail is in
[docs/runbooks/local-dev.md](docs/runbooks/local-dev.md).

## Documentation Workflow

The repository temporarily keeps a single Russian documentation tree:

- [`docs`](docs) - canonical product, architecture, ADR and runbook
  documentation in Russian.
- [`docs/tmp-plans`](docs/tmp-plans) - temporary implementation plans for active
  intermediate slices.

Workflow:

1. Discuss, draft and stabilize product or architecture decisions in Russian
   under `docs/`.
2. Keep durable product and architecture decisions in the canonical sections,
   especially `docs/systems/`, `docs/adr/` and `docs/runbooks/`.
3. Temporary plans are not canonical. If a temporary plan creates a durable
   product, architecture or process decision, promote it into the appropriate
   canonical document.

## Repository Shape

```text
apps/web/             Dioxus web shell and platform adapter surface
crates/lumi-core/     shared domain contracts
crates/lumi-server/   Axum API boundary and server entrypoint
docs/                 canonical Russian product, systems, ADRs and runbooks
docs/visuals/         static dependency-free UI/UX prototype and visual notes
docs/tmp-plans/       temporary implementation plans
tests/e2e/            Playwright browser tests and agent inspection harness
```

Use `make help` to see the supported local commands.

For fast UI/UX iteration without the Rust web stack or backend, run
`make prototype-r` and open <http://127.0.0.1:4173>. The prototype workflow is
documented in [`docs/visuals`](docs/visuals).
