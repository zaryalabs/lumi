#!/bin/sh
set -eu

tool=$(basename "$0")
case "$tool" in
  pdfinfo|pdftotext|pdftoppm) ;;
  *)
    echo "unsupported Poppler tool wrapper: $tool" >&2
    exit 2
    ;;
esac

mount_directory=
for argument in "$@"; do
  case "$argument" in
    /*)
      mount_directory=$(dirname "$argument")
      break
      ;;
  esac
done

if [ -z "$mount_directory" ]; then
  echo "$tool requires an absolute input or output path" >&2
  exit 2
fi

image=${LUMI_E2E_POPPLER_IMAGE:?set LUMI_E2E_POPPLER_IMAGE}
exec docker run \
  --rm \
  --network none \
  --read-only \
  --tmpfs /tmp:rw,noexec,nosuid,size=16m \
  --user "$(id -u):$(id -g)" \
  --volume "$mount_directory:$mount_directory" \
  "$image" \
  "$tool" \
  "$@"
