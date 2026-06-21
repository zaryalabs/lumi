# Lumi

Lumi is an open-source app for deliberate reading and learning over materials
the user already chose: books, articles, threads, messages and notes.

The product direction is described in [docs/VISION.md](docs/VISION.md). The
accepted technical design for `v01` lives in
[docs/system-design](docs/system-design).

## Current State

The repository is moving from system design into implementation. The initial
developer scaffold includes:

- a Rust workspace;
- shared domain contracts in `crates/lumi-core`;
- an Axum API skeleton in `crates/lumi-server`;
- a Dioxus web shell in `apps/web`;
- Playwright E2E scaffolding in `tests/e2e`;
- `make` targets and pre-commit hooks for local quality gates.

The first implementation slice is S0 Core Architecture Skeleton from
[docs/system-design/early-slices.md](docs/system-design/early-slices.md).

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

More detail is in [docs/runbooks/local-dev.md](docs/runbooks/local-dev.md).

## Repository Shape

```text
apps/web/             Dioxus web shell and platform adapter surface
crates/lumi-core/     shared domain contracts
crates/lumi-server/   Axum API boundary and server entrypoint
docs/                 vision, system design, ADRs and runbooks
tests/e2e/            Playwright browser tests and agent inspection harness
```

Use `make help` to see the supported local commands.
