#!/usr/bin/env bash
# Build the Linux AppImage from this checkout: kvm-it-<VERSION>-x86_64.AppImage (+ .sha256) in dist-linux/.
#   scripts/build-appimage.sh [out-dir]
# Needs podman (or docker via CONTAINER_RUNTIME) and network access the first time. The binaries are built in an
# Ubuntu 22.04 container (glibc 2.35), so the AppImage runs on Debian 12 / Ubuntu 22.04 and newer; the script fails if
# the result needs anything newer. Release what was tagged: run it from a checkout of the release tag.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/dist-linux}"
RUNTIME="${CONTAINER_RUNTIME:-podman}"
IMAGE=localhost/kvmit-appimage-build:1.90-ubuntu2204
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
# appimagetool is pinned by version and checksum (the "continuous" build moves).
TOOL_URL=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
TOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
MAX_GLIBC=2.35

mkdir -p "$OUT"
"$RUNTIME" image exists "$IMAGE" 2>/dev/null || "$RUNTIME" build -t "$IMAGE" -f "$ROOT/scripts/appimage.Containerfile" "$ROOT/scripts"

# 1. build the two binaries (separate target dir so the everyday build is untouched)
"$RUNTIME" run --rm -v "$ROOT:/project:z" -v kvmit-appimage-cargo:/usr/local/cargo/registry -w /project/desktop \
    -e CARGO_TARGET_DIR=/project/desktop/target-appimage "$IMAGE" cargo build --release --locked -p kvmit
BIN="$ROOT/desktop/target-appimage/release"

# 2. the portability floor: refuse a binary that needs a newer glibc than MAX_GLIBC
for b in kvmit kvmit-gui; do
    need="$("$RUNTIME" run --rm -v "$BIN:/b:ro,z" "$IMAGE" sh -c "objdump -T /b/$b | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1")"
    echo "$b needs ${need:-no versioned glibc symbols}"
    if [ -n "$need" ] && [ "$(printf '%s\n%s\n' "${need#GLIBC_}" "$MAX_GLIBC" | sort -V | tail -1)" != "$MAX_GLIBC" ]; then
        echo "FAIL: $b needs $need, newer than the glibc $MAX_GLIBC floor" >&2; exit 1
    fi
done

# 3. AppDir
APPDIR="$OUT/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/scalable/apps"
install -m 755 "$BIN/kvmit" "$BIN/kvmit-gui" "$APPDIR/usr/bin/"
install -m 755 "$ROOT/installer/linux/AppRun" "$APPDIR/AppRun"
install -m 644 "$ROOT/installer/linux/kvm-it.desktop" "$APPDIR/kvm-it.desktop"
install -m 644 "$ROOT/installer/linux/kvm-it.desktop" "$APPDIR/usr/share/applications/kvm-it.desktop"
install -m 644 "$ROOT/installer/linux/kvm-it.svg" "$APPDIR/kvm-it.svg"
install -m 644 "$ROOT/installer/linux/kvm-it.svg" "$APPDIR/usr/share/icons/hicolor/scalable/apps/kvm-it.svg"
install -m 644 "$ROOT/LICENSE" "$APPDIR/usr/share/LICENSE"

# 4. appimagetool (pinned)
TOOL="$OUT/appimagetool-x86_64.AppImage"
if [ ! -f "$TOOL" ] || [ "$(sha256sum "$TOOL" | cut -d' ' -f1)" != "$TOOL_SHA256" ]; then
    curl -fsSL -o "$TOOL" "$TOOL_URL"
    [ "$(sha256sum "$TOOL" | cut -d' ' -f1)" = "$TOOL_SHA256" ] || { echo "FAIL: appimagetool checksum mismatch" >&2; exit 1; }
    chmod +x "$TOOL"
fi
APPIMAGE="$OUT/kvm-it-$VERSION-x86_64.AppImage"
rm -f "$APPIMAGE"
ARCH=x86_64 "$TOOL" --appimage-extract-and-run "$APPDIR" "$APPIMAGE"
( cd "$OUT" && sha256sum "$(basename "$APPIMAGE")" > "$(basename "$APPIMAGE").sha256" && cat "$(basename "$APPIMAGE").sha256" )
echo "built $APPIMAGE"
