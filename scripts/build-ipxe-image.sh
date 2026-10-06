#!/usr/bin/env bash
# Build the adapter's read-only boot drive: a 4 MiB disk image (MBR + one FAT16 partition of type EFI system partition, not active)
# holding EFI/BOOT/BOOTX64.EFI (a Microsoft-signed shim), EFI/BOOT/IPXE.EFI (iPXE, signed by the iPXE project; the shim loads it) and autoexec.ipxe.
# The two signed binaries are the iPXE project's own release files (firmware/ipxe/signed, see pins.env), so the drive boots with Secure Boot on or off. Output: firmware/ipxe/ipxe.img, flashed into the `ipxe` partition.
#   scripts/build-ipxe-image.sh
#   AUTOEXEC=/path/to/my-autoexec.ipxe OUT=/path/to/my-ipxe.img scripts/build-ipxe-image.sh    # a private script/output: nothing is written into the repo
# Needs dosfstools, mtools and util-linux (sfdisk) on the build machine. The image is deterministic for the same inputs
# (fixed volume id and times), so a rebuild gives the same bytes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
D="$ROOT/firmware/ipxe"
S="$D/signed"
OUT="${OUT:-$D/ipxe.img}"
AUTOEXEC="${AUTOEXEC:-$D/autoexec.ipxe}"
SIZE_MIB=4
OFFSET_SECTORS=2048                       # 1 MiB, the usual alignment
export SOURCE_DATE_EPOCH=1700000000       # mtools/mkfs.vfat timestamps
export TZ=UTC                              # FAT stores local time: pin the zone too, or the bytes depend on where it is built
for f in "$S/BOOTX64.EFI" "$S/IPXE.EFI" "$AUTOEXEC"; do [ -f "$f" ] || { echo "missing $f" >&2; exit 1; }; done
rm -f "$OUT"; truncate -s "${SIZE_MIB}M" "$OUT"
# one MBR partition, type 0xEF (EFI system partition), filling the disk after the 1 MiB offset
printf 'label: dos\nlabel-id: 0x4b564d49\nunit: sectors\n\n%s,,ef\n' "$OFFSET_SECTORS" | sfdisk -q "$OUT"
# The MBR is not bootable on purpose: this disk is for UEFI. A legacy BIOS that tries it anyway would run empty boot code and hang, so
# the boot sector holds INT 18h (CD 18: "boot failed, try the next device") and a spin loop (EB FE), and no partition is marked active.
printf '\xcd\x18\xeb\xfe' | dd of="$OUT" bs=1 seek=0 conv=notrunc status=none
PART_SECTORS=$(( SIZE_MIB * 2048 - OFFSET_SECTORS ))
mkfs.vfat -F 16 -s 1 -S 512 -i 4b564d49 --offset "$OFFSET_SECTORS" "$OUT" "$(( PART_SECTORS / 2 ))" >/dev/null
IMG="$OUT@@$(( OFFSET_SECTORS * 512 ))"
# fixed file and directory times (mmd would stamp directories with "now"), so the same inputs always give the same image
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
mkdir -p "$T/EFI/BOOT"
cp "$S/BOOTX64.EFI" "$S/IPXE.EFI" "$T/EFI/BOOT/"; cp "$AUTOEXEC" "$T/autoexec.ipxe"
find "$T" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
mcopy -s -m -i "$IMG" "$T/EFI" "$T/autoexec.ipxe" ::/
sha256sum "$OUT"
mdir -i "$IMG" -/ ::/ | tail -12
