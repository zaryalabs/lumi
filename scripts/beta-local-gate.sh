#!/bin/sh
set -eu

project=${LUMI_BETA_COMPOSE_PROJECT:-lumi-beta-local-$$}
runtime=$(mktemp -d /tmp/lumi-beta-local.XXXXXX)
postgres_port=${LUMI_BETA_POSTGRES_PORT:-}

cleanup() {
  status=$?
  docker compose --project-name "$project" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$runtime"
  exit "$status"
}
trap cleanup EXIT INT TERM

if [ -z "$postgres_port" ]; then
  postgres_port=$(python3 -c 'import socket; sock = socket.socket(); sock.bind(("127.0.0.1", 0)); print(sock.getsockname()[1]); sock.close()')
fi

export COMPOSE_PROJECT_NAME="$project"
export LUMI_POSTGRES_PORT="$postgres_port"
export DATABASE_URL="postgres://lumi:lumi-local@127.0.0.1:$postgres_port/lumi"
export LUMI_BLOB_ROOT="$runtime/blob-store"

make staging-config
make staging-smoke
make pg-t
make compatibility
make security
make performance
make restore-attestation-test
make c
make web-e2e
