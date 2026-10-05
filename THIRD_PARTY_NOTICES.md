# Third-party notices

kvm-it is MIT-licensed (see `LICENSE`). It is built with many open-source Rust crates under their own licences (mostly MIT
and Apache-2.0); their exact versions are pinned in `desktop/Cargo.lock`. This file names the components that need more than
an attribution.

## serialport 4.10.1: Mozilla Public License 2.0

The in-app flasher (0.3.0) uses the `serialport` crate to talk to the adapter's COM port. It is licensed under the
**MPL-2.0** and is used here **unmodified**, as a library linked into `kvmit` and `kvmit-gui`.

- Licence text: <https://www.mozilla.org/en-US/MPL/2.0/>
- Source code of the exact version used: <https://crates.io/crates/serialport/4.10.1> (repository:
  <https://github.com/serialport/serialport-rs>), also reproducible from `desktop/Cargo.lock`.

MPL-2.0 is file-level: it applies to serialport's own source files and does not change the licence of kvm-it's code.

## espflash 4.6.0 (MIT OR Apache-2.0) and nusb 0.2 (Apache-2.0 OR MIT)

Used for writing firmware and for finding USB devices. Sources: <https://crates.io/crates/espflash/4.6.0>,
<https://crates.io/crates/nusb>.

## iPXE: GPL-2.0-or-later (many files also under the UBDL)

The adapter's read-only boot drive carries the **iPXE** UEFI binary, **built from unmodified upstream source** by `scripts/build-ipxe.sh` (a pinned commit,
default configuration, in a container; the build is reproducible). iPXE (<https://ipxe.org/>) is free software licensed under the **GNU General Public
License, version 2 or later**, with many files additionally available under the iPXE project's **Unmodified Binary Distribution Licence (UBDL)**: see
<https://ipxe.org/licensing>. We distribute the binary under the GPL, as a separate data image written to its own flash partition, not linked into kvm-it's
MIT-licensed firmware or app.

**Corresponding source:** upstream <https://github.com/ipxe/ipxe> at commit `6262f1081fe185564e8ec8365a1d23597ec6e6f5` (2026-10-01, `v2.0.0-375`),
unmodified, plus `scripts/build-ipxe.sh` in this repository (the exact recipe; this repository at the release tag is the rest). An archive of that
upstream source is attached to each release that ships the drive (`ipxe-6262f1081fe1-source.tar.gz`). The resulting `firmware/ipxe/ipxe.efi` has SHA-256
`1e3252f2dd6163368e7908bb7ee53642aacc73bdf2bcfa6a41d0764332e9e8d4`.

---

This is the notice for the components added in 0.3.0. A complete generated listing of every dependency's licence is a
planned release item (see `docs/RELEASING.md`).
