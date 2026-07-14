#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
git_sha=${GIT_SHA:-$(git -C "$root" rev-parse HEAD)}
docker_cmd=${DOCKER:-docker}
project="lumi-production-smoke-$$"
temporary=$(mktemp -d)
platform_created=0

runtime_env="$temporary/runtime.env"
images_env="$temporary/images.env"
compose="$docker_cmd compose --project-directory $temporary -p $project --env-file $runtime_env --env-file $images_env -f $root/ops/compose.yaml"

cleanup() {
  status=$?
  if [ "$status" -ne 0 ]; then
    $compose ps -a || true
    $compose logs --no-color postgres blob-init server web || true
  fi
  $compose down --volumes --remove-orphans >/dev/null 2>&1 || true
  if [ "$platform_created" = 1 ]; then
    $docker_cmd network rm platform >/dev/null 2>&1 || true
  fi
  rm -rf "$temporary"
  exit "$status"
}
trap cleanup EXIT INT TERM

mkdir -p "$temporary/volumes/postgres-data" "$temporary/volumes/blobs" "$temporary/backups" "$temporary/scripts"
printf '%s\n' \
  'LUMI_POSTGRES_IMAGE=postgres:17-alpine' \
  'LUMI_BLOB_INIT_IMAGE=busybox:1.37' \
  'LUMI_POSTGRES_PASSWORD=production-smoke-password' \
  'LUMI_DATABASE_URL=postgres://lumi:production-smoke-password@postgres:5432/lumi' \
  'LUMI_DOMAIN=lumi.test' \
  'LUMI_WEB_ORIGIN=https://lumi.test' \
  'LUMI_TELEGRAM_BOT_SCOPE=lumi-production-smoke' \
  'LUMI_TELEGRAM_BOT_USERNAME=' \
  'LUMI_TELEGRAM_WEBHOOK_SECRET=' \
  'RUST_LOG=info,tower_http=info' > "$runtime_env"
printf '%s\n' \
  'LUMI_RELEASE_ID=production-smoke' \
  "LUMI_RELEASE_SHA=$git_sha" \
  'LUMI_RELEASE_AT=2026-01-01T00:00:00Z' \
  "LUMI_SERVER_IMAGE=ghcr.io/zaryalabs/lumi-server:sha-$git_sha" \
  "LUMI_WEB_IMAGE=ghcr.io/zaryalabs/lumi-web:sha-$git_sha" > "$images_env"

if ! $docker_cmd network inspect platform >/dev/null 2>&1; then
  $docker_cmd network create platform >/dev/null
  platform_created=1
fi

$compose up -d postgres blob-init
blob_init_status=$($docker_cmd wait lumi-blob-init)
[ "$blob_init_status" = 0 ] || { echo "blob-init failed with status $blob_init_status" >&2; exit 1; }
$compose up -d --wait postgres
$compose run --rm migrate
$compose up -d --wait server web
$compose exec -T web wget --quiet --spider http://127.0.0.1:8080/api/v1/ready
$compose ps
echo "production Compose smoke passed"
