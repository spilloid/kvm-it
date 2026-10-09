#!/usr/bin/env bash
# EXPERIMENTS ONLY: build an UNSIGNED iPXE UEFI binary from a pinned upstream commit, in a container (a custom build, or your own certificate or embedded script).
#   scripts/build-ipxe.sh                 -> firmware/ipxe/ipxe.efi
# This is NOT what the adapter ships: the boot drive carries the iPXE project's signed binaries (firmware/ipxe/signed, see pins.env), which scripts/build-ipxe-image.sh
# uses; an unsigned build is refused by Secure Boot targets unless you enrol your own key. Source is upstream iPXE, unmodified, at IPXE_COMMIT (default configuration,
# x86_64 UEFI target). Reproducible given the pinned container image and its apt toolchain.
# Needs podman (or CONTAINER_RUNTIME=docker) and network access.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME="${CONTAINER_RUNTIME:-podman}"
IPXE_REPO=https://github.com/ipxe/ipxe
IPXE_COMMIT=6262f1081fe185564e8ec8365a1d23597ec6e6f5   # upstream master of 2026-10-01 ("v2.0.0-375-g6262f1081")
BASE=docker.io/library/debian@sha256:704583dbf243593da87cf949fc0543ffeca24a28d36c2760dc9545410cb8ed02   # debian:bookworm, pinned by digest
OUT="${1:-$ROOT/firmware/ipxe/ipxe.efi}"
W="$(mktemp -d)"; trap 'rm -rf "$W"' EXIT
"$RUNTIME" run --rm -v "$W":/o:Z "$BASE" bash -c "set -euo pipefail
  apt-get update -qq >/dev/null && apt-get install -y -qq build-essential perl liblzma-dev git ca-certificates >/dev/null
  git init -q /o/ipxe && cd /o/ipxe && git fetch -q --depth 1 $IPXE_REPO $IPXE_COMMIT && git checkout -q FETCH_HEAD
  [ \"\$(git rev-parse HEAD)\" = $IPXE_COMMIT ] || { echo 'fetched the wrong commit' >&2; exit 1; }
  export SOURCE_DATE_EPOCH=\$(git log -1 --format=%ct)   # iPXE embeds the build time; pin it to the commit time so the build is reproducible
  cd src && make -j\$(nproc) bin-x86_64-efi/ipxe.efi >/dev/null && cp bin-x86_64-efi/ipxe.efi /o/ipxe.efi"
cp "$W/ipxe.efi" "$OUT"
sha256sum "$OUT"
strings -a "$OUT" | grep -m1 -E '^[0-9]+\.[0-9]+\.[0-9]+\+? \(' || true
