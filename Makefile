SHELL := /bin/sh

CARGO ?= cargo
DX ?= dx
NPM ?= npm
PRE_COMMIT ?= pre-commit
DOCKER ?= docker

RUST_MANIFEST := Cargo.toml
STAGE0_SPIKE_PACKAGE := lumi-stage0-spikes
WEB_DIR := apps/web
WEB_PACKAGE := $(WEB_DIR)/Cargo.toml
E2E_DIR := tests/e2e
E2E_PACKAGE := $(E2E_DIR)/package.json
E2E_NODE_MODULES := $(E2E_DIR)/node_modules
OPS_DIR := ops

GIT_SHA ?= $(shell git rev-parse HEAD)
SHORT_SHA ?= $(shell printf '%.7s' "$(GIT_SHA)")
IMAGE_REGISTRY ?= ghcr.io
IMAGE_OWNER ?= zaryalabs
IMAGE_NAMESPACE ?= $(IMAGE_REGISTRY)/$(IMAGE_OWNER)
IMAGE_TAG ?= sha-$(GIT_SHA)
LUMI_SERVER_IMAGE ?= $(IMAGE_NAMESPACE)/lumi-server:$(IMAGE_TAG)
LUMI_WEB_IMAGE ?= $(IMAGE_NAMESPACE)/lumi-web:$(IMAGE_TAG)
RELEASE_ID ?= $(shell date -u "+%Y%m%d-%H%M%S")-$(SHORT_SHA)
RELEASE_MANIFEST ?= builds/releases/$(RELEASE_ID).env.images
BUILD_TIMESTAMP ?= $(shell date -u "+%Y-%m-%dT%H:%M:%SZ")
INSTALL_DIR ?= /opt/apps/lumi
DEPLOY_WRAPPER ?= /usr/local/sbin/lumi-ci-root-deploy

LUMI_SERVER_BIND ?= 127.0.0.1:8080
LUMI_SERVER_PORT ?= 8080
LUMI_POSTGRES_HOST ?= 127.0.0.1
LUMI_POSTGRES_PORT ?= 5432
DATABASE_URL ?= postgres://lumi:lumi-local@$(LUMI_POSTGRES_HOST):$(LUMI_POSTGRES_PORT)/lumi
LUMI_WEB_HOST ?= 127.0.0.1
LUMI_WEB_PORT ?= 5173
LUMI_API_BASE ?= http://127.0.0.1:8080/api/v1
LUMI_BLOB_ROOT ?= .local/blob-store
LUMI_PROTOTYPE_PORT ?= 4173
RUSTUP_TOOLCHAIN_BIN ?= $(shell if command -v rustup >/dev/null 2>&1; then dirname "$$(rustup which rustc 2>/dev/null)"; fi)
RUSTUP_PATH_ENV := $(if $(RUSTUP_TOOLCHAIN_BIN),PATH=$(RUSTUP_TOOLCHAIN_BIN):$$PATH,)

.DEFAULT_GOAL := help

.PHONY: help prepare build push release-manifest deploy ci-clean-images ops-config cicd-contract-test production-compose-smoke init fmt l dl t c pc docs-fmt docs-l rust-fmt rust-l rust-web-check rust-web-l rust-dl rust-t up logs down reset server-r telegram-r db-up db-down db-migrate web-r prototype-r prototype-e2e pagination-spike-r pagination-spike-e2e stage0-spikes web-build e2e-fmt e2e-fmt-check e2e-l e2e-dl web-e2e pg-t compatibility security performance staging-config staging-smoke backup restore-drill restore-attestation-test restore-attestation beta-local beta agent-inspect

help: ## Show available make targets
	@awk 'BEGIN {FS = ":.*## "}; /^[a-zA-Z0-9_.-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

# CI/CD contract

prepare: docs-l rust-l rust-t e2e-fmt-check e2e-l e2e-dl ops-config restore-attestation-test cicd-contract-test ## Run non-mutating release checks

build: ## Build commit-tagged server and web images
	$(DOCKER) build --pull --file deployments/Dockerfile.server --tag "$(LUMI_SERVER_IMAGE)" .
	$(DOCKER) build --pull --file deployments/Dockerfile.web --tag "$(LUMI_WEB_IMAGE)" .

push: ## Push commit-tagged images and write their digest-pinned manifest
	$(DOCKER) push "$(LUMI_SERVER_IMAGE)"
	$(DOCKER) push "$(LUMI_WEB_IMAGE)"
	$(MAKE) --no-print-directory release-manifest

release-manifest: ## Resolve image digests into builds/releases/<release-id>.env.images
	DOCKER="$(DOCKER)" \
	RELEASE_MANIFEST="$(RELEASE_MANIFEST)" \
	RELEASE_ID="$(RELEASE_ID)" \
	GIT_SHA="$(GIT_SHA)" \
	BUILD_TIMESTAMP="$(BUILD_TIMESTAMP)" \
	LUMI_SERVER_IMAGE="$(LUMI_SERVER_IMAGE)" \
	LUMI_WEB_IMAGE="$(LUMI_WEB_IMAGE)" \
	./scripts/write-release-manifest.sh

deploy: ## Install and deploy a CI-produced release manifest through /opt/apps/lumi
	@test -d "$(INSTALL_DIR)/builds/releases" || { echo "Missing bootstrapped $(INSTALL_DIR)" >&2; exit 1; }
	@test -x "$(DEPLOY_WRAPPER)" || { echo "Missing installed deploy wrapper $(DEPLOY_WRAPPER)" >&2; exit 1; }
	@test -f "$(RELEASE_MANIFEST)" || { echo "Missing CI release manifest $(RELEASE_MANIFEST)" >&2; exit 1; }
	./ops/validate-release-manifest.sh "$(RELEASE_MANIFEST)" "$(RELEASE_ID)"
	sudo "$(DEPLOY_WRAPPER)" "$(abspath $(RELEASE_MANIFEST))" "$(RELEASE_ID)"

ci-clean-images: ## Remove only Lumi images produced by this CI run
	-$(DOCKER) image rm "$(LUMI_SERVER_IMAGE)" "$(LUMI_WEB_IMAGE)"

ops-config: ## Validate the production installation Compose model
	cd $(OPS_DIR) && $(DOCKER) compose --env-file .env.example --env-file images.env.example -f compose.yaml config --quiet

cicd-contract-test: ## Test workflow, Make and release-manifest invariants
	python3 -m unittest scripts/test_cicd_contract.py

production-compose-smoke: ## Run the production topology with isolated disposable state
	DOCKER="$(DOCKER)" GIT_SHA="$(GIT_SHA)" ./scripts/production-compose-smoke.sh

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
	@find README.md AGENTS.md CONTRIBUTING.md docs ops -type f \( -name '*.md' -o -name '*.toml' -o -name '*.yaml' -o -name '*.yml' -o -name '*.json' \) -print >/dev/null
	@echo "Docs files are present"

rust-fmt: ## Format Rust code when Cargo workspace exists
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		$(CARGO) fmt --all; \
	else \
		echo "No Cargo.toml found; skipping Rust format"; \
	fi

rust-l: ## Run Rust format check and clippy for implemented crates
	@set -e; if [ -f "$(RUST_MANIFEST)" ]; then \
		$(CARGO) fmt --all -- --check; \
		$(CARGO) clippy -p lumi-core -p lumi-server -p $(STAGE0_SPIKE_PACKAGE) --all-targets -- -D warnings; \
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
		WASM_LIBDIR=$$($(RUSTUP_PATH_ENV) rustc --target wasm32-unknown-unknown --print target-libdir 2>/dev/null); \
		if [ -n "$$WASM_LIBDIR" ] && [ -d "$$WASM_LIBDIR" ]; then \
			$(RUSTUP_PATH_ENV) $(CARGO) clippy -p lumi-web --target wasm32-unknown-unknown --no-default-features --features web -- -D warnings; \
		else \
			echo "wasm32-unknown-unknown is not installed; skipping Dioxus web lint"; \
		fi; \
	else \
		echo "No $(WEB_PACKAGE) found; skipping Dioxus web lint"; \
	fi

rust-dl: ## Run deeper Rust dependency/config checks when tools are available
	@set -e; if [ -f "$(RUST_MANIFEST)" ]; then \
		if command -v cargo-audit >/dev/null 2>&1; then cargo audit; else echo "cargo-audit not installed; skipping audit"; fi; \
		if command -v cargo-deny >/dev/null 2>&1; then cargo deny check; else echo "cargo-deny not installed; skipping deny"; fi; \
		if command -v taplo >/dev/null 2>&1; then \
			taplo fmt --check; \
			if taplo help lint >/dev/null 2>&1; then taplo lint; else echo "taplo lint is unavailable in this Taplo build; format/parser check passed"; fi; \
		else \
			echo "taplo not installed; skipping TOML checks"; \
		fi; \
	else \
		echo "No Cargo.toml found; skipping deep Rust checks"; \
	fi

rust-t: ## Run Rust tests for implemented crates
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		if $(CARGO) nextest --version >/dev/null 2>&1; then \
			$(CARGO) nextest run -p lumi-core -p lumi-server -p $(STAGE0_SPIKE_PACKAGE); \
		else \
			$(CARGO) test -p lumi-core -p lumi-server -p $(STAGE0_SPIKE_PACKAGE); \
		fi; \
	else \
		echo "No Cargo.toml found; skipping Rust tests"; \
	fi

up: ## Build and start the complete local stack
	LUMI_SERVER_PORT=$(LUMI_SERVER_PORT) LUMI_WEB_PORT=$(LUMI_WEB_PORT) LUMI_POSTGRES_PORT=$(LUMI_POSTGRES_PORT) docker compose up -d --build --wait
	@echo "Lumi is ready: http://127.0.0.1:$(LUMI_WEB_PORT)"
	@echo "Logs: make logs | Stop: make down | Delete local data: make reset"

logs: ## Follow logs from the local stack
	docker compose logs --follow

down: ## Stop the local stack without deleting data
	docker compose down

reset: ## Stop the local stack and explicitly delete local data
	docker compose down --volumes --remove-orphans

server-r: ## Run the local Axum server
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		if ! nc -z "$(LUMI_POSTGRES_HOST)" "$(LUMI_POSTGRES_PORT)" >/dev/null 2>&1; then \
			echo "PostgreSQL is unavailable at $(LUMI_POSTGRES_HOST):$(LUMI_POSTGRES_PORT)"; \
			echo "Start it with: docker compose up -d --wait postgres"; \
			echo "Then apply migrations with: make db-migrate"; \
			exit 1; \
		fi; \
		DATABASE_URL=$(DATABASE_URL) LUMI_SERVER_BIND=$(LUMI_SERVER_BIND) $(CARGO) run -p lumi-server --bin lumi-server; \
	else \
		echo "No Cargo.toml found; cannot run server"; \
		exit 1; \
	fi

telegram-r: ## Run the local Telegram long-polling transport
	@if [ -f "$(RUST_MANIFEST)" ]; then \
		DATABASE_URL=$(DATABASE_URL) $(CARGO) run -p lumi-server --bin lumi-telegram-long-poll; \
	else \
		echo "No Cargo.toml found; cannot run Telegram transport"; \
		exit 1; \
	fi

db-up: ## Start the local PostgreSQL service
	LUMI_POSTGRES_PORT=$(LUMI_POSTGRES_PORT) docker compose up -d --wait postgres

db-down: ## Stop the local PostgreSQL service
	docker compose stop postgres

db-migrate: ## Apply forward-only SQLx migrations
	DATABASE_URL=$(DATABASE_URL) $(CARGO) run -p lumi-server --bin lumi-migrate

web-r: ## Run the Dioxus web development server
	@if [ -f "$(WEB_PACKAGE)" ]; then \
		if command -v $(DX) >/dev/null 2>&1; then \
			cd $(WEB_DIR) && $(RUSTUP_PATH_ENV) LUMI_API_BASE=$(LUMI_API_BASE) $(DX) serve --web --addr $(LUMI_WEB_HOST) --port $(LUMI_WEB_PORT); \
		else \
			echo "Dioxus CLI is not installed; install dx before running web"; \
			exit 1; \
		fi; \
	else \
		echo "No $(WEB_PACKAGE) found; cannot run web"; \
		exit 1; \
	fi

prototype-r: ## Run the static UI/UX prototype without backend
	@python3 -m http.server $(LUMI_PROTOTYPE_PORT) --bind 127.0.0.1 --directory docs/visuals/prototype

prototype-e2e: ## Run Playwright tests for the static UI/UX prototype
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) run test:prototype; else echo "E2E dependencies are not installed; run make init"; exit 1; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; cannot run prototype E2E tests"; \
		exit 1; \
	fi

pagination-spike-r: ## Run the Stage 0 pagination spike without a backend
	@python3 -m http.server $(LUMI_PROTOTYPE_PORT) --bind 127.0.0.1 --directory docs/visuals/pagination-spike

pagination-spike-e2e: ## Run Stage 0 pagination browser checks
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) run test:pagination-spike; else echo "E2E dependencies are not installed; run make init"; exit 1; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; cannot run pagination spike E2E tests"; \
		exit 1; \
	fi

stage0-spikes: ## Run executable auth, EPUB and pagination spikes
	$(CARGO) test -p $(STAGE0_SPIKE_PACKAGE)
	$(MAKE) pagination-spike-e2e

web-build: ## Build the Dioxus web app when dx is available
	@if [ -f "$(WEB_PACKAGE)" ]; then \
		if command -v $(DX) >/dev/null 2>&1; then \
			cd $(WEB_DIR) && $(RUSTUP_PATH_ENV) $(DX) build --web; \
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

e2e-fmt-check: ## Check Playwright formatting without modifying files
	@if [ -f "$(E2E_PACKAGE)" ]; then \
		if [ -d "$(E2E_NODE_MODULES)" ]; then $(NPM) --prefix $(E2E_DIR) run format:check; else echo "E2E dependencies are not installed; run make init"; exit 1; fi; \
	else \
		echo "No $(E2E_PACKAGE) found; cannot check E2E formatting"; \
		exit 1; \
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

pg-t: db-up db-migrate ## Run mandatory PostgreSQL-backed integration suites
	LUMI_TEST_DATABASE_URL=$(DATABASE_URL) $(CARGO) test -p lumi-server

compatibility: ## Run committed EPUB, Web and Telegram compatibility suites
	$(CARGO) test -p lumi-core epub::tests::import_should_build_typed_document_for_supported_epub
	$(CARGO) test -p lumi-core epub::tests::import_should_reject_path_traversal
	$(CARGO) test -p lumi-core epub::tests::import_should_reject_package_doctype
	$(CARGO) test -p lumi-core epub::tests::import_should_reject_locked_publication
	$(CARGO) test -p lumi-core epub::tests::import_should_reject_excessive_compression_ratio
	$(CARGO) test -p lumi-core fixtures
	$(CARGO) test -p lumi-core sources
	LUMI_TEST_DATABASE_URL=$(DATABASE_URL) $(CARGO) test -p lumi-server telegram

security: ## Run import, session, ownership and transport security suites
	$(CARGO) test -p lumi-server web::tests
	LUMI_TEST_DATABASE_URL=$(DATABASE_URL) $(CARGO) test -p lumi-server postgres_
	LUMI_TEST_DATABASE_URL=$(DATABASE_URL) $(CARGO) test -p lumi-server telegram_

performance: db-up db-migrate ## Run release-mode beta performance budgets
	LUMI_TEST_DATABASE_URL=$(DATABASE_URL) LUMI_PERFORMANCE=1 $(CARGO) test --release -p lumi-core -p lumi-server performance_

staging-config: ## Validate the executable staging Compose model
	docker compose --env-file deployments/staging.env.example -f deployments/compose.staging.yaml config --quiet

staging-smoke: ## Build, start and probe the isolated staging API image topology
	./scripts/staging-smoke.sh

backup: ## Create an operator-attested PostgreSQL plus blob backup manifest
	./scripts/backup.sh

restore-drill: ## Restore a backup into an explicitly disposable database
	./scripts/restore-drill.sh $(BACKUP_DIR)

restore-attestation-test: ## Test strict restore evidence validation
	python3 -m unittest scripts/test_restore_attestation.py

restore-attestation: ## Validate operator-provided encrypted restore drill evidence
	@test -n "$(RESTORE_ATTESTATION)" -a -f "$(RESTORE_ATTESTATION)" || { echo "RESTORE_ATTESTATION must point to structured external drill evidence"; exit 1; }
	python3 scripts/validate_restore_attestation.py "$(RESTORE_ATTESTATION)"

beta-local: ## Run repository-local beta mechanics without external acceptance
	./scripts/beta-local-gate.sh

beta: ## Run the aggregate closed-beta handoff gate
	./scripts/beta-gate.sh

agent-inspect: ## Print the local agent/operator browser inspection flow
	@echo "1. Start services: make up"
	@echo "2. Open browser session: playwright-cli -s=lumi-local open about:blank"
	@echo "3. Navigate: playwright-cli -s=lumi-local goto http://127.0.0.1:5173"
	@echo "4. Record observations in docs/tmp-plans/playwright-agent-inspection.md"

clean: ## Remove common local build and cache artifacts
	rm -rf target
	rm -rf $(WEB_DIR)/dist $(WEB_DIR)/target
	rm -rf $(E2E_DIR)/test-results $(E2E_DIR)/playwright-report

.PHONY: help init fmt l dl t c pc docs-fmt docs-l rust-fmt rust-l rust-web-check rust-web-l rust-dl rust-t up logs down reset db-up db-down db-migrate server-r web-r telegram-r prototype-r prototype-e2e pagination-spike-r pagination-spike-e2e stage0-spikes web-build e2e-fmt e2e-l e2e-dl web-e2e agent-inspect clean
