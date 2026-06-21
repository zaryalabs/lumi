# ADR 0001: Development Environment Scaffold

Status: accepted

## Context

Lumi is moving from accepted system design into implementation. The repository
needs a local development environment before the first product slice starts.
The nearby `cortex` project provides a useful pattern: `AGENTS.md`,
`CONTRIBUTING.md`, `Makefile`, pre-commit hooks and Playwright verification.

Lumi cannot copy that scaffold directly because its first UI stack is Dioxus
Fullstack, not React/Vite, and its system boundary is an Axum API for a
cloud-backed reading product.

## Decision

Use a Rust-first workspace scaffold:

- `crates/lumi-core` for platform-independent contracts;
- `crates/lumi-server` for explicit Axum `/api/v1` routes;
- `apps/web` for the Dioxus Web shell;
- `tests/e2e` for Playwright tests and agent/operator browser inspection;
- `Makefile` as the gateway to local workflows;
- pre-commit hooks that run `make l` and `make t`.

The default local quality gate keeps Playwright separate from `make c`.
Browser-facing changes should run `make web-e2e` explicitly.

## Consequences

- Contributors get real Rust checks immediately instead of documentation-only
  no-ops.
- The web shell is visible to Playwright before the reader implementation
  exists.
- Dioxus-specific checks depend on local `dx` and `wasm32-unknown-unknown`.
  Missing tools are reported clearly rather than hidden.
- The scaffold establishes the shared-domain/server/UI-adapter split required
  by `docs/system-design/reader-architecture.md`.

## Alternatives

- Copy the React/Vite scaffold from `cortex`: rejected because it conflicts with
  Lumi's accepted Dioxus direction.
- Keep all commands as no-ops until implementation starts: rejected because it
  delays build and test feedback.
- Put Playwright into the default commit gate: rejected for now because Dioxus
  browser startup requires heavier local tooling than the core Rust checks.

## Compatibility

This ADR does not change a product data schema. Follow-up implementation that
changes normalized content, anchors, sync, plugin, AI, search or auth contracts
must add dedicated ADRs and fixtures according to
`docs/system-design/quality.md`.
