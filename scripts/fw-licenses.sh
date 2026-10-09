#!/usr/bin/env bash
# Regenerate THIRD_PARTY_FIRMWARE_LICENSES.txt: build the firmware, then map its linker maps to licence texts inside the same
# ESP-IDF container (scripts/fw-licenses.py; pass --check to compare instead). Not a subcommand of fw.sh on purpose: fw.sh
# is a firmware source input (scripts/fw-source-hash.py), so changing it would mark the bundled firmware stale.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME="${CONTAINER_RUNTIME:-podman}"
IMAGE="${IDF_IMAGE:-$(sed -n 's/^IDF_IMAGE="${IDF_IMAGE:-\(.*\)}"$/\1/p' "$ROOT/scripts/fw.sh")}"
[[ -n "$IMAGE" ]] || { echo "error: could not read IDF_IMAGE from scripts/fw.sh" >&2; exit 1; }
"$ROOT/scripts/fw.sh" build >/dev/null
"$RUNTIME" run --rm -v "$ROOT:/project:z" -w /project -e IDF_PATH=/opt/esp/idf --entrypoint python3 "$IMAGE" scripts/fw-licenses.py "$@"
