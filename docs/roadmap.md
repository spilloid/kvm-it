# Roadmap

Status key: done = built and software-verified; **hw?** = built, awaiting verification on a physical board.

| # | Milestone | Status |
|---|---|---|
| 0 | Repository, build instructions, flash | done — flashed and boots on the real board (2026-10-03) |
| 1 | Enumerates as USB keyboard + mouse | done — hardware-verified via lsusb on a separate host (2026-10-03) |
| 2 | Self-test: `HELLO FROM KVM`, Enter, mouse nudge | done — maintainer-reported working (2026-10-03) |
| 3 | Protocol v1 spec/vectors, codecs | done — host-tested |
| 3b | BLE discovery, bonding, desktop connects | built; **hw?** |
| 4 | BLE key events → USB keyboard (dispatcher, dedup, keepalive) | host-tested; **hw?** |
| 5 | Mouse move/buttons/scroll over BLE | host-tested; **hw?** |
| 6 | Text injection, US layout (desktop-side) | host-tested |
| 7 | UVC capture (V4L2) | built; verified with a laptop camera, not an HDMI card |
| 8 | GUI: video + device controls | built; **not run with an adapter** |
| 9 | Live keyboard/mouse capture | built; **hw?** |
| 10 | Macros + native script engine | host-tested |
| 10b | Screen-aware `wait_for`, `confirm`, DuckyScript import, dry-run | host-tested on synthetic frames; not on real OOBE |
| 11 | Pairing window, bond, BOOT trust reset | built; **hw?** (see docs/security.md for the residual risk) |
| 12 | Hotplug/reconnect hardening | basic auto-reconnect built; soak testing outstanding |
| 13 | Linux release build | `scripts/rs.sh build`; no packaging yet |
| 14 | Windows port | cross-compiles; no video backend, never run |

## Backlog / known gaps

- Windows: capture backend (Media Foundation), native pairing call, running it at all.
- GUI video upload is a full-frame texture copy per frame; 1080p60 cost unmeasured.
- Only the US layout; scroll `h` (pan) is accepted by the protocol but ignored by the firmware.
- Privacy-enabled (RPA) controllers are handled by identity-address lookup in the bond store; untested.
- Packaging (AppImage/MSI), signed releases, firmware OTA.

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
