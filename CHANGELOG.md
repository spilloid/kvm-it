# Changelog

## [0.1.0] - 2026-10-03 (MVP)

Verification: see the README status table. Short version: USB HID, BLE pairing and the GATT link
(`kvmit pair`/`status` on Linux) and HDMI capture (one MacroSilicon card, 1080p) are hardware-verified. Typing
over BLE (`kvmit type`/`key` from Linux into a Windows 11 target, all printable US-ASCII) is hardware-verified.
The GUI on Linux is maintainer-reported working. The LED, BOOT gestures (pairing window, 10 s trust reset) and reconnect after replug
are hardware-verified. The Windows controller app is out of scope for 0.1.0 (cross-compiles only) and is planned for 0.2.0.

### Added
- GUI: "Send keys" panel (Ctrl+Alt+Del, Win, Alt+Tab, PrintScreen, ...), F13-F24 mapping, and input capture without a video signal.
- Docs site (GitHub Pages) and a product README.
- Wire protocol v1 (`protocol/SPEC.md`), generated golden vectors, Rust (`kvmit-protocol`) and C (`proto_frame`)
  codecs tested against the same vectors.
- Firmware: NimBLE peripheral with LE Secure Connections bonding gated by a physical pairing window,
  protocol dispatcher (handshake gate, ack dedup, keepalive release, link-drop release), status LED language,
  BOOT button gestures (short = pairing window, 10 s = erase trust), persisted device name.
- Desktop (`kvmit`): CLI and egui GUI; BLE client with retry/keepalive/motion accumulation; US layout;
  script engine (TOML format, variables, secrets, `wait_for` on the screen, confirm, dry-run, preview,
  DuckyScript import); V4L2 capture; built-in example scripts; Windows cross-compile.
- `scripts/rs.sh` (containerised Rust build/test/clippy/Windows cross-build).

### Changed (pairing)
- Every power-on (plug-in or RESET) opens a 15 s pairing window, bonded or not, so a new controller pairs
  without pressing BOOT (`CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S`, 0 disables). With a bond stored, crash/watchdog
  reboots do not open it. The trusted controller reconnecting does not use up the window; a new pairing does.
  BOOT short-press still opens the 300 s window.

### Fixed
- Firmware panicked (LoadProhibited) on every BLE connection whose controller handle was >= 2: ESP-IDF v5.5
  NimBLE indexes per-link arrays (`slave_conn`, `g_max_*`) of `MAX_CONNECTIONS + 1` entries by the raw
  connection handle, and overwrote `ble_gap_update_entries`. `firmware/patches/` now moves that state into
  NimBLE's per-connection struct; `scripts/fw.sh` applies the patch to the pinned ESP-IDF and the build refuses
  an unpatched IDF. NimBLE stays at one connection, so a second controller can never connect.

### Added (diagnostics)
- `scripts/fw.sh build-diag` / `sdkconfig.diag`: radio-diagnostic firmware that logs stored bonds and an RSSI
  survey at boot (see docs/hardware.md, "BLE troubleshooting"). Not part of release builds.

### Changed
- The boot-time HID self-test is now opt-in (`CONFIG_KVMIT_SELFTEST`); the product no longer types at boot.

### Firmware foundations (Milestones 0-2)

- ESP-IDF 5.5 project for ESP32-S3 (YD-ESP32-23, N16R8) with 16 MB flash configuration.
- Native-USB TinyUSB HID: two boot-protocol interfaces (keyboard, mouse with wheel).
- Pure-C HID state machine (duplicate-safe key/button tracking, 6-key rollover refusal, release-all).
- Deterministic boot self-test: 8 s delay, types `HELLO FROM KVM`, Enter, small mouse nudge, releases all.
- Host unit tests; container-based build/flash script.
- Hardened after adversarial review: deadline-based send waits at 1 kHz ticks, state rollback on failed sends,
  retried release-all after suspend, GET_REPORT support, Caps Lock-aware self-test.
- Hardware-verified 2026-10-03: flash, enumeration (`lsusb`), self-test typing.
