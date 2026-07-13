# Local Development Runbook

Status: draft

This runbook describes the local scaffold for the first implementation slice:
Rust workspace, Axum API skeleton, Dioxus Web shell and Playwright browser
verification.

## Prerequisites

- Rust stable with `cargo`, `rustfmt` and `clippy`.
- `wasm32-unknown-unknown` target for Dioxus Web.
- Dioxus CLI `dx`.
- Node.js and npm for Playwright.
- `pre-commit` for Git hooks.

Useful installation commands:

```sh
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli
python -m pip install pre-commit
```

Dioxus can also be installed with the official prebuilt installer or
`cargo-binstall`; see the Dioxus 0.7 setup docs.

## Bootstrap

```sh
make init
```

`make init` installs pre-commit hooks when available, fetches Cargo
dependencies, installs the wasm target through `rustup` when available, installs
Playwright dependencies and runs `dx doctor` when Dioxus CLI exists.

## Local Processes

Start the API:

```sh
make server-r
```

Start the web shell:

```sh
make web-r
```

Defaults:

- API: `http://127.0.0.1:8080/api/v1`
- Web: `http://127.0.0.1:5173`
- API bind override: `LUMI_SERVER_BIND`
- Web host override: `LUMI_WEB_HOST`
- Web port override: `LUMI_WEB_PORT`

## Quality Gates

Light local check:

```sh
make l
```

Rust tests:

```sh
make t
```

Full local handoff gate:

```sh
make c
```

Optional browser test:

```sh
make web-e2e
```

`make c` does not run Playwright by default. Run `make web-e2e` when a change
affects browser behavior, accessibility, routing or the reader surface.

## Browser Verification Modes

Automated Playwright:

```sh
make web-e2e
```

Real local profile:

```sh
make server-r
make web-r
LUMI_E2E_REAL_PROFILE=1 PLAYWRIGHT_BASE_URL=http://127.0.0.1:5173 npm --prefix tests/e2e test
```

Agent/operator inspection:

```sh
make agent-inspect
```

Record durable observations in
[docs/tmp-plans/playwright-agent-inspection.md](../tmp-plans/playwright-agent-inspection.md).

## Troubleshooting

- If Dioxus web lint is skipped, install `wasm32-unknown-unknown`.
- If `make web-r` fails with missing `dx`, install Dioxus CLI.
- If Playwright says browsers are missing, run
  `npm --prefix tests/e2e run install-browsers`.
- If `make init` cannot install tools inside a restricted sandbox, run the
  printed commands in a normal developer shell.
