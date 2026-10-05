# Building kvm-it from source

**You do not need any of this to use kvm-it.** The [releases](https://github.com/spilloid/kvm-it/releases) are a signed MSI/zip for
Windows and an AppImage for Linux: install, run, done. This page is for people who want to hack on it.

## What you need

`podman` (or `docker`, with `CONTAINER_RUNTIME=docker`). Nothing else touches your machine: the Rust and ESP-IDF toolchains live in
pinned containers.

```bash
git clone https://github.com/spilloid/kvm-it && cd kvm-it

scripts/rs.sh test       # desktop tests
scripts/rs.sh clippy     # lints
scripts/rs.sh build      # Linux app   -> desktop/target/release/{kvmit,kvmit-gui}
scripts/rs.sh windows    # Windows exes (cross-built) -> desktop/target/x86_64-pc-windows-gnu/release/

scripts/fw.sh test       # firmware host tests (no hardware)
scripts/fw.sh build      # adapter firmware -> firmware/build
```

## Flashing a build you made

```bash
desktop/target/release/kvmit flash --firmware firmware/build     # or: scripts/fw.sh flash
```

`firmware/release/` holds the exact images every package ships. If you change anything under `firmware/`, rebuild, copy
`firmware/build` over `firmware/release`, and update `FIRMWARE.txt` and `SHA256SUMS` (CI checks it, see
[RELEASING](RELEASING.md)). Port permissions on Linux, the esptool fallback and the LED/button reference are in the
[hardware guide](hardware.md).

## Where things are

| Path | What |
|---|---|
| `desktop/` | Rust workspace: `kvmit` (CLI + GUI) and its crates (BLE, HID, video, script engine, flasher) |
| `firmware/` | ESP-IDF project: ESP32-S3, TinyUSB HID, NimBLE |
| `protocol/` | [Wire protocol v1](https://github.com/spilloid/kvm-it/blob/main/protocol/SPEC.md) and the golden vectors both sides test against |
| `tools/screenshots/` | The harness that takes the pictures on this site, against a synthetic demo target |
| `scripts/`, `installer/` | Containerised builds, release packaging, the MSI and AppImage recipes |

Working agreements, review rounds and what is verified where: [development process](dev-process.md), [architecture](architecture.md),
[releasing](RELEASING.md).
