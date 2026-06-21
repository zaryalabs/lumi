# AGENTS.md

Quick guide for agents working in this repository.

> [!IMPORTANT]
> If `./.local/context/` exists, read `./.local/context/README.md` before
> starting work. `.local/` is private local context and is not committed. Do not
> read extra files from that directory unless its README explicitly asks you to.

## Start Here

- Read `README.md` first.
- Treat `docs/VISION.md` and `docs/system-design/` as the canonical product and
  architecture sources.
- Use `docs/runbooks/` for local workflow details.
- Keep architecture and process decisions in `docs/`; keep this file short and
  operational.

## Commands

- Use `make` for routine project operations.
- Run `make help` to see available commands.
- Run `make l` while iterating.
- Run `make c` before commits or handoff after code changes.
- Run `make web-e2e` when the change affects the web surface or browser flows.

## Project Shape

- `docs/` - product vision, accepted system design, ADRs and runbooks.
- `crates/lumi-core/` - shared domain contracts and platform-independent reader
  foundations.
- `crates/lumi-server/` - Axum API boundary and server process.
- `apps/web/` - Dioxus Web shell and future reader platform adapter.
- `tests/e2e/` - Playwright tests and agent/operator browser inspection harness.
- `Makefile` - gateway to local tooling.
- `.pre-commit-config.yaml` - commit-time quality gates.

## Technical Direction

Follow the stack documented in `docs/VISION.md` and `docs/system-design/`:

- Rust workspace for shared domain, import, reader, sync and service code.
- Dioxus 0.7 for Web first, then Desktop/WebView and Mobile/WebView candidates.
- Axum routes for explicit `/api/v1` system boundaries.
- SQLx for persistence once storage work starts.
- PostgreSQL/S3-compatible storage as the production direction, with local/dev
  backends allowed behind the same abstractions.
- Playwright for browser integration tests and agent/operator inspection.

## Architecture Rules

- Do not make reader core depend on Dioxus, DOM, WebView or platform handles.
- Keep web, desktop and mobile annotation/progress models shared.
- Keep system APIs explicit and versioned under Axum routes.
- Use Dioxus server functions only for narrow UI-specific calls, not as the
  durable system contract.
- Keep imported material handling source-backed: `Material -> DocumentRevision
  -> Normalized Content Package -> ReadingDocument`.
- Do not store highlights or notes as DOM paths only; use the target anchor
  model from `docs/system-design/normalized-content.md`.
- Add ADRs for schema, sync, anchor, plugin, AI, search and auth decisions listed
  in `docs/system-design/quality.md`.

## Agent-Readable UI

- Prefer semantic landmarks, headings, labels and roles over visual-only hooks.
- Browser flows should be inspectable through Playwright role locators.
- When adding a critical web workflow, add automated Playwright coverage and, if
  useful, an operator inspection note under `docs/tmp-plans/`.

## Quality Gate

Before commit or handoff:

1. Run `make c`.
2. Run `make web-e2e` for web-facing changes when the local browser stack is
   available.
3. Fix failures instead of weakening checks.
4. If a check cannot run because a local tool is missing, report the exact tool
   and command that could not run.
