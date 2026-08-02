#!/bin/sh
set -eu

css_root=${1:-apps/web/assets}
defined_file=$(mktemp)
used_file=$(mktemp)
missing_file=$(mktemp)
trap 'rm -f "$defined_file" "$used_file" "$missing_file"' EXIT

rg -o --no-filename --glob '*.css' -- '--[a-zA-Z0-9-]+[[:space:]]*:' "$css_root" \
  | sed 's/[[:space:]]*:$//' \
  | sort -u >"$defined_file"
rg -o --no-filename --glob '*.css' -- 'var\(--[a-zA-Z0-9-]+' "$css_root" \
  | sed 's/^var(//' \
  | sort -u >"$used_file"
comm -23 "$used_file" "$defined_file" >"$missing_file"

if [ -s "$missing_file" ]; then
  echo "Undefined CSS custom properties:" >&2
  sed 's/^/  /' "$missing_file" >&2
  exit 1
fi

echo "CSS custom properties are declared"
