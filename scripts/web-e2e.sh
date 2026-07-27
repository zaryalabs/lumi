#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$root"
project=${LUMI_E2E_COMPOSE_PROJECT:-lumi-e2e-$$}
runtime=$(mktemp -d /tmp/lumi-e2e.XXXXXX)
poppler_image=${LUMI_E2E_POPPLER_IMAGE:-lumi-e2e-poppler:bookworm}
npm=${NPM:-npm}

cleanup() {
  status=$?
  docker compose --project-name "$project" down --volumes --remove-orphans >/dev/null 2>&1 || true
  rm -rf "$runtime"
  exit "$status"
}
trap cleanup EXIT INT TERM

free_port() {
  python3 -c 'import socket; sock = socket.socket(); sock.bind(("127.0.0.1", 0)); print(sock.getsockname()[1]); sock.close()'
}

: "${LUMI_E2E_POSTGRES_PORT:=$(free_port)}"
: "${LUMI_E2E_API_PORT:=$(free_port)}"
: "${LUMI_E2E_WEB_PORT:=$(free_port)}"
: "${LUMI_E2E_OPENROUTER_PORT:=$(free_port)}"

mkdir -p "$runtime/bin" "$runtime/blob-store" "$runtime/search-index" "$runtime/tmp"
rm -rf -- "$root/target/dx/lumi-web/debug/web"
for tool in pdfinfo pdftotext pdftoppm; do
  ln -s "$root/scripts/e2e-poppler-tool.sh" "$runtime/bin/$tool"
done

docker build \
  --file "$root/tests/e2e/Dockerfile.poppler" \
  --tag "$poppler_image" \
  "$root/tests/e2e"

export COMPOSE_PROJECT_NAME="$project"
export LUMI_BLOB_ROOT="$runtime/blob-store"
export LUMI_SEARCH_ROOT="$runtime/search-index"
export LUMI_SEARCH_FIXTURE_MODEL=1
export LUMI_FASTTEXT_MODEL_VERSION=fixture.fasttext.v1
export LUMI_E2E_POPPLER_IMAGE="$poppler_image"
export LUMI_PDFINFO_BIN="$runtime/bin/pdfinfo"
export LUMI_PDFTOTEXT_BIN="$runtime/bin/pdftotext"
export LUMI_PDFTOPPM_BIN="$runtime/bin/pdftoppm"
export TMPDIR="$runtime/tmp"
export LUMI_E2E_POSTGRES_PORT
export LUMI_E2E_API_PORT
export LUMI_E2E_WEB_PORT
export LUMI_E2E_OPENROUTER_PORT
export DATABASE_URL="postgres://lumi:lumi-local@127.0.0.1:$LUMI_E2E_POSTGRES_PORT/lumi"

"$npm" --prefix "$root/tests/e2e" test
