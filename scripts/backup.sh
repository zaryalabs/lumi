#!/bin/sh
set -eu
umask 077

: "${LUMI_BLOB_ROOT:?LUMI_BLOB_ROOT is required}"
: "${LUMI_BACKUP_WRITES_QUIESCED:?set LUMI_BACKUP_WRITES_QUIESCED=1 after stopping writes}"
[ "$LUMI_BACKUP_WRITES_QUIESCED" = "1" ] || { echo "writes must be quiesced" >&2; exit 2; }
destination_encrypted=${LUMI_BACKUP_DESTINATION_ENCRYPTED:-0}
drill_only=${LUMI_BACKUP_DRILL_MODE:-0}
if [ "$destination_encrypted" != "1" ] && [ "$drill_only" != "1" ]; then
  echo "encrypted destination attestation is required (or set LUMI_BACKUP_DRILL_MODE=1 for disposable local validation)" >&2
  exit 2
fi
[ -n "${PGSERVICE:-}${PGHOST:-}" ] || { echo "use PGSERVICE or libpq PGHOST/PGUSER/PGPASSFILE; password URLs are forbidden" >&2; exit 2; }

root=${1:-.local/backups}
stamp=$(date -u +%Y%m%dT%H%M%SZ)
target="$root/$stamp"
mkdir -p "$target"
pg_dump --format=custom --no-owner --no-privileges --file="$target/postgres.dump"
psql -v ON_ERROR_STOP=1 -At -F ' ' -c "SELECT relation, count FROM (SELECT 'accounts' relation, count(*) count FROM accounts UNION ALL SELECT 'materials', count(*) FROM materials WHERE deleted_at IS NULL UNION ALL SELECT 'document_revisions', count(*) FROM document_revisions UNION ALL SELECT 'annotations', count(*) FROM annotations WHERE deleted_at IS NULL UNION ALL SELECT 'reading_progress', count(*) FROM reading_progress WHERE deleted_at IS NULL UNION ALL SELECT 'sync_changes', count(*) FROM sync_changes) counts ORDER BY relation" > "$target/row-counts.txt"
psql -v ON_ERROR_STOP=1 -At -F ' ' -c "SELECT content_hash, storage_key, byte_length FROM blobs ORDER BY content_hash" > "$target/blob-records.txt"
if [ "${LUMI_BACKUP_REQUIRE_SEEDED:-0}" = "1" ]; then
  awk '$1 == "accounts" || $1 == "materials" || $1 == "document_revisions" { if ($2 < 1) exit 1 }' "$target/row-counts.txt"
fi
tar --exclude='./.health' -C "$LUMI_BLOB_ROOT" -czf "$target/blobs.tar.gz" .
(cd "$target" && if command -v sha256sum >/dev/null 2>&1; then sha256sum postgres.dump blobs.tar.gz row-counts.txt blob-records.txt; else shasum -a 256 postgres.dump blobs.tar.gz row-counts.txt blob-records.txt; fi > SHA256SUMS)
if [ "$destination_encrypted" = "1" ]; then encrypted_json=true; else encrypted_json=false; fi
if [ "$drill_only" = "1" ]; then drill_json=true; else drill_json=false; fi
printf '{"schema":"lumi.backup.v1","created_at":"%s","database":"postgres.dump","blobs":"blobs.tar.gz","row_counts":"row-counts.txt","blob_records":"blob-records.txt","checksums":"SHA256SUMS","writes_quiesced":true,"destination_encrypted":%s,"drill_only":%s}\n' "$stamp" "$encrypted_json" "$drill_json" > "$target/manifest.json"
chmod 600 "$target"/*
echo "$target"
