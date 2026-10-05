# The adapter's boot drive (iPXE)

The adapter shows the target a read-only USB drive (a third USB interface after the keyboard and mouse). It carries iPXE, so a UEFI
target can boot from the network. The drive is the `ipxe` flash partition (`../partitions.csv`); `ipxe.img` is what gets written there.

| File | What |
|---|---|
| `ipxe.efi` | The official iPXE UEFI binary, **unmodified, unsigned**. Source: <https://boot.ipxe.org/x86_64-efi/ipxe.efi> (server `Last-Modified` 2026-10-01), fetched 2026-10-05, 1,163,776 bytes, SHA-256 `3b6285d2a1f8f184e86336a840c5e974780badfda06224acd5d3cf10a721ad81`; reports itself as `2.0.0+ (g6262f)`. |
| `autoexec.ipxe` | The script iPXE runs from the drive: DHCP, then the iPXE project's public demo menu (proves the network path without a boot server of your own). Replace the URL with yours. |
| `ipxe.img` | Built from the two files above by `scripts/build-ipxe-image.sh`: a 4 MiB MBR disk, one FAT16 partition marked as an EFI system partition, `EFI/BOOT/BOOTX64.EFI` + `autoexec.ipxe`. Deterministic: the same inputs give the same bytes. Committed so the firmware build (which runs in the ESP-IDF container) needs no disk tools. |

## Licence

iPXE is **GPL-2.0 with additional permissions** (UEFI binary distribution) by the iPXE project, <https://ipxe.org/>; its source is at
<https://github.com/ipxe/ipxe>. This binary is shipped as a separate data image, not linked into kvm-it's MIT-licensed firmware. See
[`../../THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md).

## Rebuilding

```bash
scripts/build-ipxe-image.sh        # needs dosfstools, mtools and util-linux (sfdisk)
scripts/fw.sh build                # the firmware build picks up firmware/ipxe/ipxe.img
```

To use your own iPXE (a build with your certificates or script embedded, or a signed one), replace `ipxe.efi`, rebuild the image,
and record the new source and checksum here.

## Secure Boot

A stock iPXE is not signed by anything a Secure Boot target trusts, so such a target refuses it ("Access Denied -- rejected probably
by Secure Boot" in an OVMF virtual machine with the stock keys). Validating and solving that is the 0.4.x work; see
[`docs/roadmap.md`](../../docs/roadmap.md) and `tools/ipxe-test/boot-vm.sh`, which boots a UEFI VM from the real adapter in either mode.
