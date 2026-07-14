#!/bin/sh
set -eu
umask 077

: "${RESTORE_DATABASE_NAME:?RESTORE_DATABASE_NAME is required and must end in _restore_drill}"
[ -n "${PGSERVICE:-}${PGHOST:-}" ] || { echo "use PGSERVICE or libpq PGHOST/PGUSER/PGPASSFILE; password URLs are forbidden" >&2; exit 2; }
case "$RESTORE_DATABASE_NAME" in
  *_restore_drill) ;;
  *) echo "refusing non-drill database name" >&2; exit 2 ;;
esac
backup=${1:?backup directory is required}
(cd "$backup" && if command -v sha256sum >/dev/null 2>&1; then sha256sum -c SHA256SUMS; else shasum -a 256 -c SHA256SUMS; fi)
dropdb --if-exists "$RESTORE_DATABASE_NAME"
createdb "$RESTORE_DATABASE_NAME"
tmp=$(mktemp -d)
cleanup() { rm -rf "$tmp"; dropdb --if-exists "$RESTORE_DATABASE_NAME"; }
trap cleanup EXIT INT TERM
pg_restore --exit-on-error --no-owner --no-privileges --dbname="$RESTORE_DATABASE_NAME" "$backup/postgres.dump"
tar -C "$tmp" -xzf "$backup/blobs.tar.gz"
PGDATABASE="$RESTORE_DATABASE_NAME" psql -v ON_ERROR_STOP=1 -At -F ' ' -c "SELECT relation, count FROM (SELECT 'accounts' relation, count(*) count FROM accounts UNION ALL SELECT 'materials', count(*) FROM materials WHERE deleted_at IS NULL UNION ALL SELECT 'document_revisions', count(*) FROM document_revisions UNION ALL SELECT 'annotations', count(*) FROM annotations WHERE deleted_at IS NULL UNION ALL SELECT 'reading_progress', count(*) FROM reading_progress WHERE deleted_at IS NULL UNION ALL SELECT 'sync_changes', count(*) FROM sync_changes) counts ORDER BY relation" > "$tmp/row-counts.txt"
diff -u "$backup/row-counts.txt" "$tmp/row-counts.txt"
while read -r hash storage_key byte_length; do
  file="$tmp/$storage_key"
  [ -f "$file" ] || { echo "missing restored blob $storage_key" >&2; exit 1; }
  [ "$(wc -c < "$file" | tr -d ' ')" = "$byte_length" ] || { echo "blob size mismatch $storage_key" >&2; exit 1; }
  if command -v sha256sum >/dev/null 2>&1; then actual_hash=$(sha256sum "$file" | awk '{print $1}'); else actual_hash=$(shasum -a 256 "$file" | awk '{print $1}'); fi
  [ "$actual_hash" = "$hash" ] || { echo "blob hash mismatch $storage_key" >&2; exit 1; }
done < "$backup/blob-records.txt"
echo "restore drill passed"
