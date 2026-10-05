#!/usr/bin/env bash
# Build the adapter's read-only boot drive: a 4 MiB disk image (MBR + one FAT16 partition marked as an EFI system partition)
# holding EFI/BOOT/BOOTX64.EFI (iPXE) and autoexec.ipxe. Output: firmware/ipxe/ipxe.img, flashed into the `ipxe` partition.
#   scripts/build-ipxe-image.sh
# Needs dosfstools, mtools and util-linux (sfdisk) on the build machine. The image is deterministic for the same inputs
# (fixed volume id and times), so a rebuild gives the same bytes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
D="$ROOT/firmware/ipxe"
OUT="$D/ipxe.img"
SIZE_MIB=4
OFFSET_SECTORS=2048                       # 1 MiB, the usual alignment
export SOURCE_DATE_EPOCH=1700000000       # mtools/mkfs.vfat timestamps
for f in "$D/ipxe.efi" "$D/autoexec.ipxe"; do [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }; done
rm -f "$OUT"; truncate -s "${SIZE_MIB}M" "$OUT"
# one MBR partition, type 0xEF (EFI system partition), filling the disk after the 1 MiB offset
printf 'label: dos\nlabel-id: 0x4b564d49\nunit: sectors\n\n%s,,ef,*\n' "$OFFSET_SECTORS" | sfdisk -q "$OUT"
PART_SECTORS=$(( SIZE_MIB * 2048 - OFFSET_SECTORS ))
mkfs.vfat -F 16 -s 1 -S 512 -i 4b564d49 --offset "$OFFSET_SECTORS" "$OUT" "$(( PART_SECTORS / 2 ))" >/dev/null
IMG="$OUT@@$(( OFFSET_SECTORS * 512 ))"
# fixed file and directory times (mmd would stamp directories with "now"), so the same inputs always give the same image
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/EFI/BOOT"
cp "$D/ipxe.efi" "$T/EFI/BOOT/BOOTX64.EFI"; cp "$D/autoexec.ipxe" "$T/autoexec.ipxe"
find "$T" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
mcopy -s -m -i "$IMG" "$T/EFI" "$T/autoexec.ipxe" ::/
sha256sum "$OUT"
mdir -i "$IMG" -/ ::/ | tail -12
