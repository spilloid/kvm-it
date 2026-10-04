#!/usr/bin/env bash
# Run cargo for the desktop workspace in a container (the host has no Rust toolchain).
# Usage: scripts/rs.sh {image|test|clippy|fmt|build|windows|run <args>|shell|cargo <args>}
#   windows  cross-compiles kvmit.exe (x86_64-pc-windows-gnu); compile-checked only, not run on Windows here.
set -euo pipefail
IMAGE="${RS_IMAGE:-localhost/kvmit-rs:1.99}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME="${CONTAINER_RUNTIME:-podman}"
ensure_image() {
    "$RUNTIME" image exists "$IMAGE" 2>/dev/null || "$RUNTIME" build -t "$IMAGE" -f "$ROOT/scripts/rs.Containerfile" "$ROOT/scripts"
}
run() {
    ensure_image
    "$RUNTIME" run --rm $([ -t 0 ] && echo -it) ${RS_EXTRA_ARGS:-} \
        -v "$ROOT:/project:z" -v kvmit-cargo:/usr/local/cargo/registry \
        -w /project/desktop -e CARGO_TARGET_DIR=/project/desktop/target "$IMAGE" "$@"
}
case "${1:-}" in
image)   ensure_image ;;
test)    run cargo test --workspace ;;
clippy)  run cargo clippy --workspace --all-targets -- -D warnings ;;
fmt)     run cargo fmt --all ;;
build)   run cargo build --release -p kvmit ;;
windows) run cargo build --release -p kvmit --target x86_64-pc-windows-gnu ;;
run)     shift; run cargo run --release -p kvmit -- "$@" ;;
shell)   run bash ;;
cargo)   shift; run cargo "$@" ;;
*) sed -n '2,5p' "$0"; exit 1 ;;
esac
