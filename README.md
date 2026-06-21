# Lumi

Lumi is an open-source app for deliberate reading and learning over materials
the user already chose: books, articles, threads, messages and notes.

The canonical development-facing product direction is described in
[docs/en/vision.md](docs/en/vision.md). The accepted technical design for `v01`
lives in [docs/en/system-design](docs/en/system-design).

## Current State

The repository is moving from system design into implementation. The initial
developer scaffold includes:

- a Rust workspace;
- shared domain contracts in `crates/lumi-core`;
- an Axum API skeleton in `crates/lumi-server`;
- a Dioxus web shell in `apps/web`;
- Playwright E2E scaffolding in `tests/e2e`;
- `make` targets and pre-commit hooks for local quality gates.

The current implementation target is a fixture-backed S1 Web EPUB Reader slice
from [docs/en/early-slices.md](docs/en/early-slices.md), built on the S0 core
architecture skeleton.

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
[docs/en/runbooks/local-dev.md](docs/en/runbooks/local-dev.md).

## Documentation Workflow

Documentation is split by language and should stay path-synchronized:

- [`docs/en`](docs/en) - canonical English documentation used for development.
- [`docs/ru`](docs/ru) - Russian drafts, source notes and working discussion.
- [`docs/tmp-plans`](docs/tmp-plans) - temporary implementation plans for active
  intermediate slices.

Workflow:

1. Discuss and draft product or architecture ideas in Russian under `docs/ru`
   when that is the clearest working language.
2. Translate and stabilize durable decisions in the matching `docs/en` path
   before using them as implementation guidance.
3. Keep the same relative Markdown document set in `docs/en` and `docs/ru`.
4. If one language has a document that the other language lacks, add the missing
   mirror instead of deleting the source document.
5. Temporary plans are not part of the language mirror. If a temporary plan
   creates a durable product, architecture or process decision, promote that
   decision into both language trees, with `docs/en` as the development canon.

## Repository Shape

```text
apps/web/             Dioxus web shell and platform adapter surface
crates/lumi-core/     shared domain contracts
crates/lumi-server/   Axum API boundary and server entrypoint
docs/en/              canonical English product, design, ADRs and runbooks
docs/ru/              Russian drafts, source notes and working docs
docs/tmp-plans/       temporary implementation plans
tests/e2e/            Playwright browser tests and agent inspection harness
```

Use `make help` to see the supported local commands.
