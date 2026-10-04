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
IMAGE=localhost/kvmit-appimage-build:1.99-ubuntu2204-pinned
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
# appimagetool is pinned by version and checksum (the "continuous" build moves).
TOOL_URL=https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage
TOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
# The runtime that appimagetool embeds (the executable stub of every AppImage) is pinned too; left alone, appimagetool
# downloads whatever is newest. The digest was taken from the tagged release when it was pinned (trust on first use).
RUNTIME_URL=https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d
MAX_GLIBC=2.35

mkdir -p "$OUT"
"$RUNTIME" image exists "$IMAGE" 2>/dev/null || "$RUNTIME" build -t "$IMAGE" -f "$ROOT/scripts/appimage.Containerfile" "$ROOT/scripts"

# 1. build the two binaries (separate target dir so the everyday build is untouched)
"$RUNTIME" run --rm -v "$ROOT:/project:z" -v kvmit-appimage-cargo:/usr/local/cargo/registry -w /project/desktop \
    -e CARGO_TARGET_DIR=/project/desktop/target-appimage "$IMAGE" cargo build --release --locked -p kvmit
BIN="$ROOT/desktop/target-appimage/release"

# 2. the portability floor: refuse a binary that needs a newer glibc than MAX_GLIBC
for b in kvmit kvmit-gui; do
    # inspect first and fail if the inspection itself fails; only then is "no versioned symbols" a valid answer
    syms="$("$RUNTIME" run --rm -v "$BIN:/b:ro,z" "$IMAGE" objdump -T "/b/$b")" || { echo "FAIL: could not inspect $b" >&2; exit 1; }
    need="$(printf '%s\n' "$syms" | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 || true)"
    echo "$b needs ${need:-no versioned glibc symbols}"
    if [ -n "$need" ] && [ "$(printf '%s\n%s\n' "${need#GLIBC_}" "$MAX_GLIBC" | sort -V | tail -1)" != "$MAX_GLIBC" ]; then
        echo "FAIL: $b needs $need, newer than the glibc $MAX_GLIBC floor" >&2; exit 1
    fi
done

# Not bundled: the host supplies the window system, graphics and D-Bus/BlueZ libraries (a bundled GL or Wayland stack
# breaks more machines than it fixes). Print what the binaries load so the release notes can name it.
for b in kvmit kvmit-gui; do
    echo "$b links (besides glibc): $("$RUNTIME" run --rm -v "$BIN:/b:ro,z" "$IMAGE" sh -c "ldd /b/$b | awk '{print \$1}' | grep -v -E '^(linux-vdso|/lib|libc\.|libm\.|libdl|libpthread|librt|libgcc_s|ld-linux)' | tr '\n' ' '")"
done

# 3. AppDir
APPDIR="$OUT/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications" "$APPDIR/usr/share/icons/hicolor/256x256/apps"
install -m 755 "$BIN/kvmit" "$BIN/kvmit-gui" "$APPDIR/usr/bin/"
install -m 755 "$ROOT/installer/linux/AppRun" "$APPDIR/AppRun"
install -m 644 "$ROOT/installer/linux/kvm-it.desktop" "$APPDIR/kvm-it.desktop"
install -m 644 "$ROOT/installer/linux/kvm-it.desktop" "$APPDIR/usr/share/applications/kvm-it.desktop"
ICON="$ROOT/desktop/crates/kvmit/assets/icon-256.png"   # the project logo, same file the GUI embeds
install -m 644 "$ICON" "$APPDIR/kvm-it.png"
install -m 644 "$ICON" "$APPDIR/usr/share/icons/hicolor/256x256/apps/kvm-it.png"
install -m 644 "$ROOT/LICENSE" "$APPDIR/usr/share/LICENSE"
install -m 644 "$ROOT/THIRD_PARTY_NOTICES.md" "$APPDIR/usr/share/THIRD_PARTY_NOTICES.md"

# 4. appimagetool (pinned)
TOOL="$OUT/appimagetool-x86_64.AppImage"
if [ ! -f "$TOOL" ] || [ "$(sha256sum "$TOOL" | cut -d' ' -f1)" != "$TOOL_SHA256" ]; then
    curl -fsSL -o "$TOOL" "$TOOL_URL"
    [ "$(sha256sum "$TOOL" | cut -d' ' -f1)" = "$TOOL_SHA256" ] || { echo "FAIL: appimagetool checksum mismatch" >&2; exit 1; }
    chmod +x "$TOOL"
fi
RT="$OUT/runtime-x86_64"
if [ ! -f "$RT" ] || [ "$(sha256sum "$RT" | cut -d' ' -f1)" != "$RUNTIME_SHA256" ]; then
    curl -fsSL -o "$RT" "$RUNTIME_URL"
    [ "$(sha256sum "$RT" | cut -d' ' -f1)" = "$RUNTIME_SHA256" ] || { echo "FAIL: AppImage runtime checksum mismatch" >&2; exit 1; }
fi
# file times: the commit's, not "now" (squashfs records them)
export SOURCE_DATE_EPOCH="$(git -C "$ROOT" log -1 --format=%ct 2>/dev/null || date +%s)"
find "$APPDIR" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
APPIMAGE="$OUT/kvm-it-$VERSION-x86_64.AppImage"
rm -f "$APPIMAGE"
ARCH=x86_64 "$TOOL" --appimage-extract-and-run --runtime-file "$RT" "$APPDIR" "$APPIMAGE"
( cd "$OUT" && sha256sum "$(basename "$APPIMAGE")" > "$(basename "$APPIMAGE").sha256" && cat "$(basename "$APPIMAGE").sha256" )
echo "built $APPIMAGE"
