# Roadmap

Status key: done = built and software-verified; **hw?** = built, awaiting verification on a physical board.

| # | Milestone | Status |
|---|---|---|
| 0 | Repository, build instructions, flash | done — flashed and boots on the real board (2026-10-03) |
| 1 | Enumerates as USB keyboard + mouse | done — hardware-verified via lsusb on a separate host (2026-10-03) |
| 2 | Self-test: `HELLO FROM KVM`, Enter, mouse nudge | done — maintainer-reported working (2026-10-03) |
| 3 | Protocol v1 spec/vectors, codecs | done — host-tested |
| 3b | BLE discovery, bonding, desktop connects | done — hardware-verified on Linux/BlueZ (2026-10-03) |
| 4 | BLE key events → USB keyboard (dispatcher, dedup, keepalive) | done — typing and keepalive release of a held key after `kill -9` hardware-verified into a Windows 11 target |
| 5 | Mouse move/buttons/scroll over BLE | host-tested; **hw?** |
| 6 | Text injection, US layout (desktop-side) | host-tested |
| 7 | UVC capture (V4L2) | done — hardware-verified with an HDMI capture card (MacroSilicon `345f:2109`) |
| 8 | GUI: video + device controls | maintainer-reported working with an adapter and a target |
| 9 | Live keyboard/mouse capture | built; **hw?** |
| 10 | Macros + native script engine | host-tested |
| 10b | Screen-aware `wait_for`, `confirm`, DuckyScript import, dry-run | host-tested on synthetic frames; not on real OOBE |
| 11 | Pairing window, bond, BOOT trust reset | done — hardware-verified (15 s power-on window, BOOT window, 10 s trust reset, LED); residual risk in docs/security.md |
| 12 | Hotplug/reconnect hardening | basic auto-reconnect built; soak testing outstanding |
| 13 | Linux release build | **v0.1.0**: `scripts/rs.sh build`; no packaging yet |
| 14 | Windows controller app | **v0.2.0**, in progress on `windows-0.2.0`: native pairing, scan, Media Foundation capture, the GUI and a BLE link fix have run in a Windows 11 VM (adapter and capture card passed through over USB); keyboard grab in progress; **not yet verified on bare-metal Windows** (Windows as a *target* already works: it is plain USB HID) |

## Planned releases

Direction, not commitments; each release is cut with its own notes and only claims what was verified.

| Release | Theme | Notes |
|---|---|---|
| **0.2.0** | Windows controller | See row 14. Also: top-bar GUI, native keyboard grab on Windows. |
| **0.2.1** | Patch | Video device switching fixed (+ Rescan), logo, Linux AppImage. No firmware or protocol change. |
| **0.3.0** | Flash the adapter from the app | A technician plugs the board's COM port into the controller, presses Flash, then moves the board to the target. **Shipped in 0.3.0** (`kvmit flash` and the GUI wizard): the firmware images ship with the app (`firmware/release/`) and are written with the `espflash` library (Rust >= 1.95). The app refuses to flash while an adapter's native USB port is plugged into the computer, keeps the pairing unless a full erase is confirmed, and checks the chip and flash size first. Later: pull firmware for a newer release, flash several boards in one go (0.6.0 multi-probe). |
| **0.5.0** (candidate) | Network boot through the adapter | The adapter also presents a **read-only USB drive carrying an iPXE image**, so a target can boot from the network and reach WinPE, an installer ISO or anything else a boot server offers. There is no on-board image storage and no SD/TF slot: *virtual-media ISO mounting is intentionally not planned.* The first design step is whether the ESP32-S3 can expose a read-only mass-storage interface alongside HID without disturbing BIOS keyboard enumeration. |
| **0.6.0** | Multi-probe | One running app attaches several adapters and capture cards, associates each adapter with its capture card, switches between the pairs and coordinates script runs across them (an IP-KVM-style fleet view). Builds on the multi-controller/multi-device groundwork noted below. |

## Backlog / known gaps

- v0.2.0 (Windows controller): bare-metal verification, the OS-level keyboard grab (Win, Alt+Tab, ... go to the
  target; Ctrl+Alt+Esc releases), YUY2 capture path, Linux hardware regression run for the motion-frame cap.
- GUI video upload is a full-frame texture copy per frame; 1080p60 cost unmeasured.
- Only the US layout.
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
- Multi-controller trust and multi-device UI (architecture avoids singletons; not built; see 0.6.0 above).
- Windows Secure Attention Sequence (Ctrl+Alt+Del): the macro will send it as an ordinary USB HID chord.
  Whether a given Windows/secure-desktop environment honours it is unverified and will be documented per
  environment once tested on real hardware.
