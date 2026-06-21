SHELL := /bin/sh

CARGO ?= cargo
DX ?= dx
NPM ?= npm
PRE_COMMIT ?= pre-commit

RUST_MANIFEST := Cargo.toml
WEB_DIR := apps/web
WEB_PACKAGE := $(WEB_DIR)/Cargo.toml
E2E_DIR := tests/e2e
E2E_PACKAGE := $(E2E_DIR)/package.json
E2E_NODE_MODULES := $(E2E_DIR)/node_modules

LUMI_SERVER_BIND ?= 127.0.0.1:8080
LUMI_WEB_HOST ?= 127.0.0.1
LUMI_WEB_PORT ?= 5173

.DEFAULT_GOAL := help

help: ## Show available make targets
	@awk 'BEGIN {FS = ":.*## "}; /^[a-zA-Z0-9_.-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

init: ## Install hooks and local dependencies when tools are available
	@if command -v $(PRE_COMMIT) >/dev/null 2>&1; then \
		$(PRE_COMMIT) install; \
	else \
		echo "pre-commit is not installed; install it to enable commit hooks"; \
	fi
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		$(CARGO) fetch; \
	else \
		echo "No Cargo.toml found; skipping Rust dependency fetch"; \
	fi
	@if command -v rustup >/dev/null 2>&1; then \
		rustup target add wasm32-unknown-unknown; \
	else \
		echo "rustup is not available; install wasm32-unknown-unknown manually if needed"; \
	fi
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		$(NPM) --prefix $(E2E_DIR) install; \
	else \
		echo "No $(E2E_PACKAGE) found; skipping Playwright dependency install"; \
	fi
	@if command -v $(DX) >/dev/null 2>&1; then \
		$(DX) doctor; \
	else \
		echo "Dioxus CLI is not installed; install dx before running web targets"; \
	fi

fmt: docs-fmt rust-fmt e2e-fmt ## Format all supported project files

l: docs-l rust-l e2e-l ## Run light checks

dl: l rust-dl e2e-dl ## Run deeper optional checks

t: rust-t ## Run implemented test suites

c: fmt dl t ## Run full local quality gate

pc: ## Run pre-commit hooks on all files
	@if command -v $(PRE_COMMIT) >/dev/null 2>&1; then \
		$(PRE_COMMIT) run --all-files; \
	else \
		echo "pre-commit is not installed; cannot run hooks"; \
		exit 1; \
	fi

docs-fmt: ## Format/check docs when a formatter is available
	@echo "No docs formatter configured yet; skipping docs format"

docs-l: ## Run lightweight docs checks
	@find README.md AGENTS.md CONTRIBUTING.md docs -type f \( -name '*.md' -o -name '*.toml' -o -name '*.yaml' -o -name '*.yml' -o -name '*.json' \) -print >/dev/null
	@echo "Docs files are present"

rust-fmt: ## Format Rust code when Cargo workspace exists
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		$(CARGO) fmt --all; \
	else \
		echo "No Cargo.toml found; skipping Rust format"; \
	fi

rust-l: ## Run Rust format check and clippy for implemented crates
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		$(CARGO) fmt --all -- --check; \
		$(CARGO) clippy -p lumi-core -p lumi-server --all-targets -- -D warnings; \
		$(MAKE) rust-web-check; \
		$(MAKE) rust-web-l; \
	else \
		echo "No Cargo.toml found; skipping Rust lint"; \
	fi

rust-web-check: ## Check Dioxus RSX on the host without requiring a platform target
	@if [ -f "$(WEB_PACKAGE)" ]; then \
		$(CARGO) clippy -p lumi-web --no-default-features --features check -- -D warnings; \
	else \
		echo "No $(WEB_PACKAGE) found; skipping Dioxus host check"; \
	fi

rust-web-l: ## Run Dioxus web lint when wasm target is installed
	@if [ -f "$(WEB_PACKAGE)" ]; then \
		WASM_LIBDIR=$$(rustc --target wasm32-unknown-unknown --print target-libdir 2>/dev/null); \
		if [ -n "$$WASM_LIBDIR" ] && [ -d "$$WASM_LIBDIR" ]; then \
			$(CARGO) clippy -p lumi-web --target wasm32-unknown-unknown --no-default-features --features web -- -D warnings; \
		else \
			echo "wasm32-unknown-unknown is not installed; skipping Dioxus web lint"; \
		fi; \
	else \
		echo "No $(WEB_PACKAGE) found; skipping Dioxus web lint"; \
	fi

rust-dl: ## Run deeper Rust dependency/config checks when tools are available
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		if command -v cargo-audit >/dev/null 2>&1; then cargo audit; else echo "cargo-audit not installed; skipping audit"; fi; \
		if command -v cargo-deny >/dev/null 2>&1; then cargo deny check; else echo "cargo-deny not installed; skipping deny"; fi; \
		if command -v taplo >/dev/null 2>&1; then taplo fmt --check && taplo lint; else echo "taplo not installed; skipping TOML checks"; fi; \
	else \
		echo "No Cargo.toml found; skipping deep Rust checks"; \
	fi

rust-t: ## Run Rust tests for implemented crates
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		if $(CARGO) nextest --version >/dev/null 2>&1; then \
			$(CARGO) nextest run -p lumi-core -p lumi-server; \
		else \
			$(CARGO) test -p lumi-core -p lumi-server; \
		fi; \
	else \
		echo "No Cargo.toml found; skipping Rust tests"; \
	fi

server-r: ## Run the local Axum server
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		LUMI_SERVER_BIND=$(LUMI_SERVER_BIND) $(CARGO) run -p lumi-server; \
	else \
		echo "No Cargo.toml found; cannot run server"; \
		exit 1; \
	fi

web-r: ## Run the Dioxus web development server
	@if [ -f "$(WEB_PACKAGE)" ]; then \
		if command -v $(DX) >/dev/null 2>&1; then \
			cd $(WEB_DIR) && LUMI_API_BASE=http://127.0.0.1:8080/api/v1 $(DX) serve --web --addr $(LUMI_WEB_HOST) --port $(LUMI_WEB_PORT); \
		else \
			echo "Dioxus CLI is not installed; install dx before running web"; \
			exit 1; \
		fi; \
	else \
		echo "No $(WEB_PACKAGE) found; cannot run web"; \
		exit 1; \
	fi

web-build: ## Build the Dioxus web app when dx is available
	@if [ -f "$(WEB_PACKAGE)" ]; then \
		if command -v $(DX) >/dev/null 2>&1; then \
			cd $(WEB_DIR) && $(DX) build --web; \
		else \
			echo "Dioxus CLI is not installed; skipping web build"; \
		fi; \
	else \
		echo "No $(WEB_PACKAGE) found; skipping web build"; \
	fi

e2e-fmt: ## Format/check Playwright files when Node dependencies exist
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) run format; else echo "E2E dependencies are not installed; skipping E2E format"; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; skipping E2E format"; \
	fi

e2e-l: ## Typecheck Playwright tests when Node dependencies exist
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) run typecheck; else echo "E2E dependencies are not installed; skipping E2E typecheck"; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; skipping E2E typecheck"; \
	fi

e2e-dl: ## Run optional Playwright static checks when dependencies exist
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) run lint; else echo "E2E dependencies are not installed; skipping E2E lint"; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; skipping E2E lint"; \
	fi

web-e2e: ## Run Playwright browser E2E tests
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) test; else echo "E2E dependencies are not installed; run make init"; exit 1; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; cannot run E2E tests"; \
		exit 1; \
	fi

agent-inspect: ## Print the local agent/operator browser inspection flow
	@echo "1. Start services: make server-r and make web-r"
	@echo "2. Open browser session: playwright-cli -s=lumi-local open about:blank"
	@echo "3. Navigate: playwright-cli -s=lumi-local goto http://127.0.0.1:5173"
	@echo "4. Record observations in docs/tmp-plans/playwright-agent-inspection.md"

clean: ## Remove common local build and cache artifacts
	rm -rf target
	rm -rf $(WEB_DIR)/dist $(WEB_DIR)/target
	rm -rf $(E2E_DIR)/test-results $(E2E_DIR)/playwright-report

.PHONY: help init fmt l dl t c pc docs-fmt docs-l rust-fmt rust-l rust-web-check rust-web-l rust-dl rust-t server-r web-r web-build e2e-fmt e2e-l e2e-dl web-e2e agent-inspect clean
