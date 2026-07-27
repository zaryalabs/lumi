#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
git_sha=${GIT_SHA:-$(git -C "$root" rev-parse HEAD)}
docker_cmd=${DOCKER:-docker}
project="lumi-production-smoke-$$"
temporary=$(mktemp -d)
platform_created=0
export LUMI_SMOKE_CONTAINER_PREFIX="$project"

runtime_env="$temporary/runtime.env"
images_env="$temporary/images.env"
compose="$docker_cmd compose --project-directory $temporary -p $project --env-file $runtime_env --env-file $images_env -f $root/ops/compose.yaml -f $root/ops/compose.smoke.yaml"

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

mkdir -p \
  "$temporary/volumes/postgres-data" \
  "$temporary/volumes/blobs" \
  "$temporary/volumes/secrets" \
  "$temporary/volumes/search" \
  "$temporary/volumes/models" \
  "$temporary/backups" \
  "$temporary/scripts"
cp "$root/scripts/backup.sh" "$temporary/scripts/backup.sh"
cp "$root/scripts/restore-drill.sh" "$temporary/scripts/restore-drill.sh"
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
  'LUMI_FASTTEXT_MODEL=' \
  'LUMI_FASTTEXT_MODEL_SHA256=' \
  'LUMI_FASTTEXT_MODEL_VERSION=cc.ru.300.fasttext.v1' \
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
blob_init_container=$($compose ps -aq blob-init)
[ -n "$blob_init_container" ] || { echo "blob-init container was not created" >&2; exit 1; }
blob_init_status=$($docker_cmd wait "$blob_init_container")
[ "$blob_init_status" = 0 ] || { echo "blob-init failed with status $blob_init_status" >&2; exit 1; }
$compose up -d --wait postgres
$compose run --rm migrate
$compose up -d --wait server web
$compose exec -T web wget --quiet --spider http://127.0.0.1:8080/api/v1/ready
$compose stop server
backup_path=$(
  LUMI_BACKUP_WRITES_QUIESCED=1 \
    LUMI_BACKUP_DRILL_MODE=1 \
    LUMI_BACKUP_REQUIRE_SEEDED=0 \
    $compose run --rm backup |
    tail -n 1
)
backup_id=${backup_path##*/}
[ -n "$backup_id" ] || { echo "backup did not return an id" >&2; exit 1; }
LUMI_RESTORE_BACKUP_ID="$backup_id" $compose run --rm restore-drill
$compose ps
echo "production Compose smoke passed"
