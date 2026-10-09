#!/usr/bin/env bash
# Build the macOS disk image: kvm-it-<VERSION>-macos-<arch>.dmg (+ .sha256) in dist-macos/. Run it on a Mac.
#   scripts/build-dmg.sh [out-dir]          (SKIP_BUILD=1 reuses desktop/target/release)
# The app is only ad-hoc signed (not Developer-ID signed, not notarized): Gatekeeper asks the user to allow it in
# System Settings > Privacy & Security.
set -euo pipefail
[ "$(uname -s)" = Darwin ] || { echo "build-dmg.sh runs on macOS" >&2; exit 1; }
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/dist-macos}"
VERSION="$(tr -d '[:space:]' < "$ROOT/VERSION")"
ARCH="$(uname -m)"
BIN="$ROOT/desktop/target/release"
[ -n "${SKIP_BUILD:-}" ] || (cd "$ROOT/desktop" && cargo build --release --locked -p kvmit)

rm -rf "$OUT"
APP="$OUT/stage/kvm-it.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
install -m 755 "$BIN/kvmit-gui" "$BIN/kvmit" "$APP/Contents/MacOS/"
# the adapter firmware Flash adapter... writes: a `firmware` folder next to the executables
cp -R "$ROOT/firmware/release" "$APP/Contents/MacOS/firmware"
cp "$ROOT/LICENSE" "$ROOT/THIRD_PARTY_NOTICES.md" "$APP/Contents/Resources/"

# icon: build an .icns from the 256 px logo (sips resizes, iconutil packs)
ICONSET="$OUT/kvm-it.iconset"
mkdir -p "$ICONSET"
SRC="$ROOT/desktop/crates/kvmit/assets/icon-256.png"
for s in 16 32 128 256; do
    sips -z "$s" "$s" "$SRC" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
done
sips -z 32 32 "$SRC" --out "$ICONSET/icon_16x16@2x.png" >/dev/null
sips -z 64 64 "$SRC" --out "$ICONSET/icon_32x32@2x.png" >/dev/null
sips -z 256 256 "$SRC" --out "$ICONSET/icon_128x128@2x.png" >/dev/null
cp "$SRC" "$ICONSET/icon_256x256.png"
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/kvm-it.icns"
rm -rf "$ICONSET"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>kvm-it</string>
  <key>CFBundleDisplayName</key><string>kvm-it</string>
  <key>CFBundleIdentifier</key><string>io.github.spilloid.kvm-it</string>
  <key>CFBundleExecutable</key><string>kvmit-gui</string>
  <key>CFBundleIconFile</key><string>kvm-it</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSBluetoothAlwaysUsageDescription</key><string>kvm-it talks to its USB adapter over Bluetooth Low Energy.</string>
  <key>NSCameraUsageDescription</key><string>kvm-it shows the target's screen from a USB HDMI capture card.</string>
  <key>NSMicrophoneUsageDescription</key><string>kvm-it records the target's sound from the capture card, only when you choose a sound input for a recording.</string>
</dict></plist>
PLIST

# Apple-silicon binaries must be signed to run at all: ad-hoc ("-") is enough locally and is not a trust claim.
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"

ln -s /Applications "$OUT/stage/Applications"
DMG="$OUT/kvm-it-$VERSION-macos-$ARCH.dmg"
hdiutil create -volname "kvm-it $VERSION" -srcfolder "$OUT/stage" -ov -format UDZO "$DMG" >/dev/null
( cd "$OUT" && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256" && cat "$(basename "$DMG").sha256" )
echo "built $DMG (preview: ad-hoc signed, not notarized, not hardware-verified)"
