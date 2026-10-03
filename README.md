# kvm-it

When you need a KVM to provision a headless appliance and don't want to shell out for full equipment.

kvm-it is an open-source desktop KVM built from two cheap parts: a generic USB HDMI capture card and an
ESP32-S3 that pretends to be a USB keyboard and mouse. The target machine needs no software, drivers, network
or Bluetooth — it works in BIOS/UEFI, installers, login screens and recovery environments.

```
Target HDMI out ──► USB HDMI capture card ──► controller PC ──► desktop app (video preview)

Controller keyboard/mouse ──► desktop app ──► Bluetooth LE ──► ESP32-S3 ──► USB HID ──► target PC
```

Bluetooth is only the controller-to-ESP32 link. The target-facing side is plain USB HID.

## Status — v0.1.0, Milestone 2 (firmware self-test)

| Milestone | State |
|---|---|
| M0 repo, build + flash instructions | done — flashed to a real YD-ESP32-23; boots, TinyUSB installs, no crash/WDT reset (serial log) |
| M1 enumerates as USB keyboard + mouse | **hardware-verified 2026-10-03** — `lsusb` on a separate Linux host shows `303a:4008` with keyboard (boot) + mouse (boot) HID interfaces, Full Speed |
| M2 self-test types `HELLO FROM KVM`, Enter, small mouse nudge | **hardware-verified 2026-10-03** (maintainer reported "it worked"; exact text/BIOS behaviour not itemised) |
| M3 protocol v1 spec + vectors, Rust/C codecs | **host-tested** (shared vectors pass in both languages); BLE transport not started |
| M3 BLE + desktop CLI | not started — see [docs/roadmap.md](docs/roadmap.md) |

What is verified and what is not, plainly:

- **Verified by running it:** the pure-C HID state machine and ASCII→HID mapping pass host unit tests
  (with ASan/UBSan); the firmware compiles and links for ESP32-S3 with ESP-IDF 5.5.
- **Verified on the real board (serial log only):** flash succeeds and hash-verifies (ESP32-S3 rev v0.2, 16 MB
  flash, 8 MB PSRAM detected), firmware boots and the self-test task waits for a target.
- **Not verified:** anything needing a target host on the USB port — USB enumeration, BIOS/boot-protocol behaviour,
  actually typing, the mouse nudge, the watchdog. Those need your hardware; see [docs/hardware.md](docs/hardware.md)
  for the checklist to report back.

## Quick start (Linux)

Needs `podman` (or docker) only; the toolchain lives in a pinned container.

```bash
scripts/fw.sh test            # host unit tests
scripts/fw.sh build           # build firmware for ESP32-S3
scripts/fw.sh flash           # flash over the board's COM port (see docs/hardware.md for permissions)
scripts/fw.sh monitor         # serial log; Ctrl-] to exit
```

**Read [docs/hardware.md](docs/hardware.md) before flashing**: which USB-C port goes where, and why you must
not connect the board's USB port to the machine you are developing on.

## Layout

| Path | Purpose |
|---|---|
| `firmware/` | ESP-IDF project (ESP32-S3, TinyUSB HID) |
| `protocol/` | Shared wire-protocol definition and test vectors (arrives with Milestone 3) |
| `desktop/` | Rust desktop workspace (arrives with Milestone 3/7) |
| `docs/` | [ux & scripting](docs/ux.md), [architecture](docs/architecture.md), [hardware](docs/hardware.md), [security](docs/security.md), [protocol](docs/protocol.md), [roadmap](docs/roadmap.md) |
| `scripts/` | build/flash helpers |

## Security note

kvm-it will be used to type credentials. Secrets are never logged and never persisted by default. Read
[docs/security.md](docs/security.md) before using it for anything you care about.

## License

MIT — see [LICENSE](LICENSE).
