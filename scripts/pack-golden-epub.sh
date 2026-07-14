#!/bin/sh
set -eu
source_dir=${1:-tests/fixtures/epub/supported}
output=${2:-/tmp/lumi-supported-golden.epub}
rm -f "$output"
(cd "$source_dir" && zip -X -0 "$output" mimetype >/dev/null && zip -X -r -9 "$output" META-INF EPUB >/dev/null)
echo "$output"
