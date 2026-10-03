#!/usr/bin/env bash
# Run cargo for the desktop workspace in a pinned Rust container (host has no Rust toolchain).
# Usage: scripts/rs.sh {test|clippy|fmt|shell|cargo <args>}
set -euo pipefail
RUST_IMAGE="${RUST_IMAGE:-docker.io/library/rust:1.90}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME="${CONTAINER_RUNTIME:-podman}"
run() {
    "$RUNTIME" run --rm $([ -t 0 ] && echo -it) \
        -v "$ROOT:/project:z" -v kvmit-cargo:/usr/local/cargo/registry \
        -w /project/desktop -e CARGO_TARGET_DIR=/project/desktop/target "$RUST_IMAGE" "$@"
}
case "${1:-}" in
test)   run cargo test --workspace ;;
clippy) run bash -c 'rustup component add clippy >/dev/null 2>&1; cargo clippy --workspace --all-targets -- -D warnings' ;;
fmt)    run bash -c 'rustup component add rustfmt >/dev/null 2>&1; cargo fmt --all' ;;
shell)  run bash ;;
cargo)  shift; run cargo "$@" ;;
*) sed -n '2,3p' "$0"; exit 1 ;;
esac
