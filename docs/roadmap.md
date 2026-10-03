# Roadmap

Status key: done = built and software-verified; **hw?** = built, awaiting verification on a physical board.

| # | Milestone | Status |
|---|---|---|
| 0 | Repository, build instructions, flash | done — flashed and boots on the real board (2026-10-03) |
| 1 | Enumerates as USB keyboard + mouse | done — hardware-verified via lsusb on a separate host (2026-10-03) |
| 2 | Self-test: `HELLO FROM KVM`, Enter, mouse nudge | done — maintainer-reported working (2026-10-03) |
| 3 | Protocol v1 spec/vectors + codecs; BLE discovery, desktop CLI connects | codecs host-tested (spec, vectors, Rust `kvmit-protocol`, C `proto_frame`); BLE transport + CLI **next**; merge gated on the BLE-gate motion |
| 4 | BLE KEY_TAP → USB keyboard | |
| 5 | Mouse move/buttons over BLE | |
| 6 | Text injection, US layout (desktop-side) | |
| 7 | UVC capture in Rust | |
| 8 | GUI: video + device controls | |
| 9 | Live keyboard/mouse capture | |
| 10 | Macros + native script engine (text/key/chord/delay/repeat/variables) | |
| 10b | Screen-aware `wait_for`, `confirm`, DuckyScript import, dry-run (needs M7/M8) | |
| 11 | Secure pairing/trust, BOOT-hold trust reset | |
| 12 | Hotplug/reconnect hardening | |
| 13 | Linux release build | |
| 14 | Windows port | |

## Backlog / known gaps

- HID idle-rate (SET_IDLE) retransmission for the keyboard (astra review #5, deferred).
- Host-testable seam around `usb_hid.c` state/rollback/pending-release logic (currently hardware-only).
- Real VID/PID allocation before public hardware distribution.
- Firmware update path (not a launch blocker): version is reported from day one. Candidates in order of
  preference: USB DFU/serial via the COM port (works today with `scripts/fw.sh flash`), BLE OTA with
  signed images + `ota_0/ota_1` partition layout (needs a custom partition table), then optional USB DFU.
- Boot-compatible 4-byte mouse report fallback if BIOS testing shows it is needed.
- PSRAM enablement, if ever needed.
- Multi-controller trust and multi-device UI (architecture avoids singletons; not built).
- Windows Secure Attention Sequence (Ctrl+Alt+Del): the macro will send it as an ordinary USB HID chord.
  Whether a given Windows/secure-desktop environment honours it is unverified and will be documented per
  environment once tested on real hardware.
