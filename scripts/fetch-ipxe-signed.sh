#!/usr/bin/env bash
# Fetch the Secure Boot boot-drive binaries from the iPXE project's release and verify them against the pinned checksums
# (firmware/ipxe/signed/pins.env). They are committed already; run this to check them against the upstream release, or after bumping the pins.
#   scripts/fetch-ipxe-signed.sh          # download, verify, and (re)write firmware/ipxe/signed/{BOOTX64.EFI,IPXE.EFI}
# Needs curl, mtools and sha256sum. The release image is a bare FAT filesystem holding EFI/BOOT/BOOTX64.EFI (a Microsoft-signed shim),
# EFI/BOOT/IPXE.EFI (iPXE, signed with the iPXE project's CA that the shim trusts) and autoexec.ipxe (kvm-it supplies its own).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
D="$ROOT/firmware/ipxe/signed"; . "$D/pins.env"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
curl -fsSL -o "$T/sb.usb" "$USB_URL"
echo "$USB_SHA256  $T/sb.usb" | sha256sum -c --quiet - || { echo "the downloaded release image does not match USB_SHA256 in pins.env" >&2; exit 1; }
mcopy -n -i "$T/sb.usb" ::EFI/BOOT/BOOTX64.EFI ::EFI/BOOT/IPXE.EFI "$T/"
echo "$SHIM_SHA256  $T/BOOTX64.EFI" | sha256sum -c --quiet - || { echo "shim does not match SHIM_SHA256" >&2; exit 1; }
echo "$IPXE_EFI_SHA256  $T/IPXE.EFI" | sha256sum -c --quiet - || { echo "IPXE.EFI does not match IPXE_EFI_SHA256" >&2; exit 1; }
cp -f "$T/BOOTX64.EFI" "$T/IPXE.EFI" "$D/"
echo "ok: $IPXE_TAG ($IPXE_COMMIT), shim $SHIM_VERSION"
