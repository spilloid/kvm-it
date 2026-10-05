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

## iPXE: GPL-2.0 with additional permissions

The adapter's read-only boot drive carries the **iPXE** UEFI binary, **unmodified** from the iPXE project
(<https://boot.ipxe.org/x86_64-efi/ipxe.efi>; SHA-256 `3b6285d2a1f8f184e86336a840c5e974780badfda06224acd5d3cf10a721ad81`; see
`firmware/ipxe/README.md`). iPXE is licensed under the **GNU General Public License v2** with additional permissions for UEFI
binary distribution; the licence text is at <https://github.com/ipxe/ipxe/blob/master/COPYING.GPLv2> and the exact source of the
upstream project is at <https://github.com/ipxe/ipxe>. It ships as a separate data image written to its own flash partition, not
linked into kvm-it's MIT-licensed firmware or app. The binary reports its own version as `2.0.0+ (g6262f)`, i.e. a build of upstream
`master` at a commit starting `6262f`; the corresponding source is that revision of the repository above. For an archive of exactly
that source, open an issue on this repository. Building iPXE from a pinned tag here (needed anyway for Secure Boot work) is planned.

---

This is the notice for the components added in 0.3.0. A complete generated listing of every dependency's licence is a
planned release item (see `docs/RELEASING.md`).
