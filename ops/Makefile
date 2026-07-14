SHELL := /bin/sh

DOCKER ?= docker
COMPOSE ?= $(DOCKER) compose
COMPOSE_FILE ?= compose.yaml
ENV_FILE ?= .env
IMAGE_ENV ?= .env.images
RELEASES_DIR ?= builds/releases
RELEASE ?=
S ?=
TAIL ?= 200
LUMI_EXTERNAL_SMOKE_URL ?=
LUMI_BACKUP_DESTINATION_ENCRYPTED ?= 0

COMPOSE_CMD = $(COMPOSE) --env-file $(ENV_FILE) --env-file $(IMAGE_ENV) -f $(COMPOSE_FILE)

.DEFAULT_GOAL := help

help: ## Show targets and installation notes
	@awk 'BEGIN {FS = ":.*## "}; /^[a-zA-Z0-9_.-]+:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)
	@printf '\n'
	@cat README.md

require-files:
	@test -f "$(ENV_FILE)" || { echo "Missing $(ENV_FILE)" >&2; exit 1; }
	@test -e "$(IMAGE_ENV)" || { echo "Missing active $(IMAGE_ENV) release pointer" >&2; exit 1; }

config: require-files ## Validate active production Compose configuration
	$(COMPOSE_CMD) config --quiet

ps: require-files ## Show Compose service status
	$(COMPOSE_CMD) ps $(S)

status: require-files ## Show active release and service status
	@printf 'active release: '
	@ls -l "$(IMAGE_ENV)"
	$(COMPOSE_CMD) ps

logs: require-files ## Follow product logs; optionally set S=<service>
	$(COMPOSE_CMD) logs --tail=$(TAIL) --follow $(S)

releases: ## List locally installed release manifests
	@mkdir -p "$(RELEASES_DIR)"
	@find "$(RELEASES_DIR)" -maxdepth 1 -type f -name '*.env.images' -exec basename {} \; | sort

require-release:
	@case "$(RELEASE)" in \
		''|[!A-Za-z0-9]*|*[!A-Za-z0-9._-]*) echo "Set a safe RELEASE=<release-id>" >&2; exit 1 ;; \
	esac

validate-release: require-release ## Validate a release manifest before activation
	@./scripts/validate-release-manifest.sh "$(RELEASES_DIR)/$(RELEASE).env.images" "$(RELEASE)"

activate: validate-release ## Activate RELEASE=<release-id>
	@ln -sfn "$(RELEASES_DIR)/$(RELEASE).env.images" "$(IMAGE_ENV)"
	@printf 'Activated %s\n' "$(RELEASE)"

pull: require-files ## Pull all immutable runtime images
	$(COMPOSE_CMD) pull postgres blob-init server web

migrate: require-files ## Apply forward-only database migrations
	$(COMPOSE_CMD) run --rm migrate

up: require-files ## Start or update runtime services without deleting state
	@mkdir -p volumes/postgres-data volumes/blobs backups
	$(COMPOSE_CMD) up -d --remove-orphans postgres blob-init server web

down: require-files ## Stop Lumi without deleting product data
	$(COMPOSE_CMD) down

smoke: require-files ## Check internal readiness and external Traefik routing
	@set -eu; \
		external_url="$(LUMI_EXTERNAL_SMOKE_URL)"; \
		if [ -z "$$external_url" ]; then external_url="$$(sed -n 's/^LUMI_WEB_ORIGIN=//p' "$(ENV_FILE)")"; fi; \
		case "$$external_url" in https://*) external_host="$${external_url#https://}" ;; *) echo "LUMI_WEB_ORIGIN must be HTTPS" >&2; exit 1 ;; esac; \
		case "$$external_host" in ''|*[!A-Za-z0-9.-]*) echo "LUMI_WEB_ORIGIN must not contain a path, port or shell syntax" >&2; exit 1 ;; esac; \
		attempt=1; \
		while [ "$$attempt" -le 20 ]; do \
			if $(COMPOSE_CMD) exec -T web wget --quiet --spider http://127.0.0.1:8080/api/v1/ready; then break; fi; \
			attempt=$$((attempt + 1)); \
			sleep 3; \
		done; \
		test "$$attempt" -le 20 || { $(COMPOSE_CMD) logs --tail=100 server web; exit 1; }; \
		status="$$(curl --silent --show-error --output /dev/null --write-out '%{http_code}' "$$external_url")"; \
		test "$$status" = 200 || { echo "External smoke returned HTTP $$status" >&2; exit 1; }; \
		printf 'Lumi smoke passed (external HTTP %s)\n' "$$status"

deploy: require-files ## Pull, migrate, start and smoke the active release
	$(MAKE) --no-print-directory pull
	$(MAKE) --no-print-directory migrate
	$(MAKE) --no-print-directory up
	$(MAKE) --no-print-directory ps
	$(MAKE) --no-print-directory smoke

rollback: require-release ## Activate RELEASE=<previous-release> and deploy it
	$(MAKE) --no-print-directory activate RELEASE="$(RELEASE)"
	$(MAKE) --no-print-directory deploy

backup: require-files ## Quiesce writes and create an encrypted-destination backup
	@set -eu; \
		test "$(LUMI_BACKUP_DESTINATION_ENCRYPTED)" = 1 || { echo "Set LUMI_BACKUP_DESTINATION_ENCRYPTED=1 only after verifying encrypted storage" >&2; exit 1; }; \
		$(COMPOSE_CMD) stop server; \
		restart_server() { $(COMPOSE_CMD) up -d server >/dev/null; }; \
		trap restart_server EXIT INT TERM; \
		LUMI_BACKUP_WRITES_QUIESCED=1 LUMI_BACKUP_DESTINATION_ENCRYPTED="$(LUMI_BACKUP_DESTINATION_ENCRYPTED)" \
			$(COMPOSE_CMD) run --rm backup; \
		restart_server; \
		trap - EXIT INT TERM

restore-verify: require-files ## Verify BACKUP=<backup-id> in a disposable database
	@test -n "$(BACKUP)" || { echo "Set BACKUP=<backup-id>" >&2; exit 1; }
	@case "$(BACKUP)" in *[!A-Za-z0-9T_-]*) echo "Unsafe backup id" >&2; exit 1 ;; esac
	LUMI_RESTORE_BACKUP_ID="$(BACKUP)" $(COMPOSE_CMD) run --rm restore-drill

.PHONY: help require-files config ps status logs releases require-release validate-release activate pull migrate up down smoke deploy rollback backup restore-verify
