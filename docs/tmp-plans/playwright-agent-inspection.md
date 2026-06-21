# Playwright Agent Inspection

Status: draft

Purpose: record manual or agent-driven browser inspection against the local
Lumi web profile. Automated checks live in `tests/e2e`; this note is for
observations that are useful during implementation but should not become
generated browser artifacts in git.

## Local Profile

- API: `http://127.0.0.1:8080/api/v1`
- Web: `http://127.0.0.1:5173`
- Web stack: Dioxus Web through `dx serve`
- Browser driver: `playwright-cli`

## Commands

```sh
make server-r
make web-r
playwright-cli -s=lumi-local open about:blank
playwright-cli -s=lumi-local goto http://127.0.0.1:5173
```

## Checklist

- App exposes a named `main` landmark.
- Primary navigation is reachable by role and accessible name.
- Reader surface exposes a named region.
- Status/context panel is reachable as complementary content.
- Console has no unexpected runtime errors.
- Visual layout works at desktop and narrow mobile widths.

## Observations

- Pending first run.
