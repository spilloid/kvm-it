# The adapter's boot drive (iPXE)

The adapter shows the target a read-only USB drive (a third USB interface after the keyboard and mouse). It carries iPXE, so a UEFI
target can boot from the network. The drive is the `ipxe` flash partition (`../partitions.csv`); `ipxe.img` is what gets written there.

| File | What |
|---|---|
| `ipxe.efi` | iPXE for x86-64 UEFI, **built from unmodified upstream** (`https://github.com/ipxe/ipxe` at `6262f1081fe185564e8ec8365a1d23597ec6e6f5`, 2026-10-01, `v2.0.0-375`) by `scripts/build-ipxe.sh` in a Debian 12 container with SOURCE_DATE_EPOCH pinned to the commit time: **reproducible** (two builds are byte-identical). SHA-256 `1e3252f2dd6163368e7908bb7ee53642aacc73bdf2bcfa6a41d0764332e9e8d4`. Unsigned. |
| `autoexec.ipxe` | The script iPXE runs from the drive. **Inert by default:** it waits five seconds for the key `n` and otherwise exits, so a target that boots this drive by accident carries on with its next boot device; on `n` it does DHCP and chains to the iPXE project's public demo menu over HTTPS (proves the path without a boot server of your own). Replace the URL with yours. |
| `ipxe.img` | Built from the two files above by `scripts/build-ipxe-image.sh`: a 4 MiB MBR disk, one FAT16 partition of type EFI system partition (not marked active; the boot sector is `INT 18h` so a legacy BIOS moves on), `EFI/BOOT/BOOTX64.EFI` + `autoexec.ipxe`. Deterministic: the same inputs give the same bytes. Committed so the firmware build (which runs in the ESP-IDF container) needs no disk tools. |

## Licence

iPXE is free software under the **GPL, version 2 or later**, with many files also available under the project's Unmodified Binary Distribution Licence
(<https://ipxe.org/licensing>). We ship the binary under the GPL, as a separate data image (not linked into kvm-it's MIT-licensed firmware). The
corresponding source is the pinned upstream commit plus `scripts/build-ipxe.sh`; an archive of that source accompanies each release. See
[`../../THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md).

## Rebuilding

```bash
scripts/build-ipxe.sh              # iPXE from the pinned upstream commit (podman, network)
scripts/build-ipxe-image.sh        # needs dosfstools, mtools and util-linux (sfdisk)
scripts/fw.sh build                # the firmware build picks up firmware/ipxe/ipxe.img
```

To use your own iPXE (a build with your certificates or script embedded, or a signed one), change `IPXE_COMMIT`/the recipe in `scripts/build-ipxe.sh` (or replace
`ipxe.efi`), rebuild the image, and record the new source and checksum here.

## Secure Boot

A stock iPXE is not signed by anything a Secure Boot target trusts, so such a target refuses it ("Access Denied -- rejected probably
by Secure Boot" in an OVMF virtual machine with the stock keys). Validating and solving that is the 0.4.x work; see
[`docs/roadmap.md`](../../docs/roadmap.md) and `tools/ipxe-test/boot-vm.sh`, which boots a UEFI VM from the real adapter in either mode.
