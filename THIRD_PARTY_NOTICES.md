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

## iPXE and the Secure Boot shim: GNU GPL version 2 and BSD-2-Clause

The adapter's read-only boot drive carries two UEFI binaries **exactly as the iPXE project published them** (release `v2.0.0`, file `ipxe-x86_64-sb.usb`, SHA-256
`1bba4318a1818ef148a80b32c7cfe3199d4f7cd93b600d74f1d8f64cea64eb46`; pins and checks in `firmware/ipxe/signed/pins.env` and `scripts/fetch-ipxe-signed.sh`):

- **`EFI/BOOT/IPXE.EFI` is iPXE** (<https://ipxe.org/>), signed with the iPXE project's Secure Boot CA. iPXE is free software; its source files carry their own licence
  declarations (mostly GPL, many also under the project's Unmodified Binary Distribution Licence), and iPXE's own `make ...licence` tool could not determine a single
  licence for the default build. We therefore treat and distribute the binary as a whole under the **GNU General Public License, version 2** (the licence text is
  `firmware/ipxe/COPYING.GPLv2`, shipped as `firmware/ipxe-COPYING.GPLv2` in every package), and we make no stronger claim. See <https://ipxe.org/licensing>. It is a
  separate data image written to its own flash partition, not linked into kvm-it's MIT-licensed firmware or app.
  **Corresponding source:** upstream <https://github.com/ipxe/ipxe> at tag `v2.0.0`, commit `12798ec29aa8a64d8675c4378b99f5fe28447afb`, unmodified. An archive of that
  source, `ipxe-12798ec29aa8-source.tar.gz` (with a `.sha256`), is attached to every release that ships the drive (built and attached by `.github/workflows/ipxe-source.yml`;
  `scripts/ipxe-source-archive.sh` makes the same archive by hand), and every package carries `firmware/ipxe-SOURCE.txt` saying so. `IPXE.EFI` has SHA-256
  `6558e37887516b246d6a97122e8d18bedfe4197b7ba7f67bf1bf102a16678d33`. kvm-it cannot rebuild this binary (the signature is the iPXE project's); building iPXE yourself
  from that source gives an unsigned binary that Secure Boot targets refuse.
- **`EFI/BOOT/BOOTX64.EFI` is the Secure Boot shim**, <https://github.com/rhboot/shim> `16.1` (commit `dad4f20725731a8b16ce136918c312c29694848a`), signed by Microsoft
  (UEFI CA) and carrying the iPXE project's certificate. It is under the BSD-2-Clause licence; its copyright notice is `firmware/ipxe/signed/shim-COPYRIGHT` and is
  reproduced in `firmware/ipxe-SOURCE.txt` in every package. SHA-256 `83ad71c7d4f2cf328b75b653d09bf3bea5f29bee2e67ca058f37d83c07133885`.

---

This is the notice for the components added in 0.3.0. A complete generated listing of every dependency's licence is a
planned release item (see `docs/RELEASING.md`).
