#!/usr/bin/env bash
# Archive the exact upstream iPXE source the shipped binary was built from, to attach to each release that ships the boot drive
# (GPL: the corresponding source accompanies the binary). Reads the pinned commit from scripts/build-ipxe.sh.
#   scripts/ipxe-source-archive.sh [out-dir]    -> ipxe-<12 hex>-source.tar.gz and its .sha256
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME="${CONTAINER_RUNTIME:-podman}"
COMMIT="$(sed -n 's/^IPXE_COMMIT=\([0-9a-f]\{40\}\).*/\1/p' "$ROOT/scripts/build-ipxe.sh")"
BASE="$(sed -n 's/^BASE=\([^ ]*\).*/\1/p' "$ROOT/scripts/build-ipxe.sh")"
[ -n "$COMMIT" ] && [ -n "$BASE" ] || { echo "could not read the pinned commit from scripts/build-ipxe.sh" >&2; exit 1; }
OUT="${1:-$ROOT/dist-ipxe-source}"; mkdir -p "$OUT"
NAME="ipxe-${COMMIT:0:12}-source"
"$RUNTIME" run --rm -v "$OUT":/o:Z "$BASE" bash -c "set -euo pipefail
  apt-get update -qq >/dev/null && apt-get install -y -qq git ca-certificates >/dev/null
  git init -q /s && cd /s && git fetch -q --depth 1 https://github.com/ipxe/ipxe $COMMIT && git checkout -q FETCH_HEAD
  [ \"\$(git rev-parse HEAD)\" = $COMMIT ] || { echo 'fetched the wrong commit' >&2; exit 1; }
  git archive --format=tar --prefix=$NAME/ HEAD | gzip -n -9 > /o/$NAME.tar.gz"
( cd "$OUT" && sha256sum "$NAME.tar.gz" | tee "$NAME.tar.gz.sha256" )
