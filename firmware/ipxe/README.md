# The adapter's boot drive (iPXE)

The adapter shows the target a read-only USB drive (a third USB interface after the keyboard and mouse). It carries iPXE, so a UEFI
target can boot from the network. The drive is the `ipxe` flash partition (`../partitions.csv`); `ipxe.img` is what gets written there.

| File | What |
|---|---|
| `signed/BOOTX64.EFI` | The **Secure Boot shim**: the iPXE project's fork of rhboot/shim 16.1 (`ipxe/shim` tag `ipxe-16.1`), signed by Microsoft's 2011 and 2023 UEFI CAs, with the iPXE project's certificate inside. Exactly as published. |
| `signed/IPXE.EFI` | **iPXE v2.0.0** for x86-64 UEFI, signed with the iPXE project's Secure Boot CA. Exactly as published; kvm-it did not build it and cannot rebuild it. |
| `signed/pins.env`, `signed/shim-NOTICES.txt` | Where the two files come from (release URLs, checksums, source commits) and the shim's licence notices (BSD-2-Clause, OpenSSL, EDK2). `scripts/fetch-ipxe-signed.sh` re-downloads and verifies them. |
| `autoexec.ipxe` | The script iPXE runs from the drive. **Inert by default:** it waits five seconds for a key and otherwise exits with a failure status, so a target that boots this drive by accident carries on with its normal boot order. A key press does DHCP and chains to the iPXE project's public demo. Replace the URL with your own server's. |
| `ipxe.img` | Built from the files above by `scripts/build-ipxe-image.sh`: a 4 MiB MBR disk, one FAT16 partition of type EFI system partition (not marked active; the boot sector is `INT 18h` so a legacy BIOS moves on), holding `EFI/BOOT/BOOTX64.EFI` (the shim), `EFI/BOOT/IPXE.EFI` and `autoexec.ipxe`. Deterministic: the same inputs give the same bytes. |

The firmware's UEFI boot path starts the shim, which verifies and starts `IPXE.EFI` from the same folder; iPXE then reads `autoexec.ipxe` from the volume.

## Licence

iPXE is free software; its files carry their own declarations, and iPXE's own `make ...licence` tool cannot determine one licence for the default build. So the binary is
treated and distributed under the **GNU GPL, version 2** (`COPYING.GPLv2` here; shipped as `ipxe-COPYING.GPLv2` in every package), as a separate data image (not linked
into kvm-it's MIT-licensed firmware). The corresponding source is upstream iPXE at tag `v2.0.0` (commit `12798ec29aa8a64d8675c4378b99f5fe28447afb`); an archive of it is attached to
each release that ships the drive (`.github/workflows/ipxe-source.yml`), and every package carries `ipxe-SOURCE.txt`, which also reproduces the shim's notices.
See [`../../THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md).

## Rebuilding

```bash
scripts/fetch-ipxe-signed.sh       # optional: re-download the signed binaries and check them against the pins
scripts/build-ipxe-image.sh        # needs dosfstools, mtools and util-linux (sfdisk)
scripts/fw.sh build                # the firmware build picks up firmware/ipxe/ipxe.img
scripts/refresh-firmware-release.py
```

To move to a newer iPXE release, change the pins in `signed/pins.env` (and re-fetch). `scripts/build-ipxe.sh` builds an **unsigned** iPXE from source (for experiments,
or with your own certificate or embedded script); it is not what ships, and Secure Boot targets refuse an unsigned build unless you enrol your own key.

## Secure Boot

With Secure Boot **on** (the OVMF virtual machine with the stock Microsoft keys, `tools/ipxe-test/boot-vm.sh sb`): the shim and iPXE start from this drive, run
`autoexec.ipxe`, get an address and fetch over HTTPS; an unsigned Linux kernel is refused ("Security Policy Violation", as intended); Windows PE through the signed
`wimboot` reaches its prompt. With Secure Boot off the same image works. **Not yet run:** any real PC (firmware, the Microsoft 2011 vs 2023 CA question, the
shim's revocation level over time). See [`docs/roadmap.md`](../../docs/roadmap.md).
