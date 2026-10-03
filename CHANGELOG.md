# Changelog

## [0.1.0] - unreleased (MVP)

Verification: see the README status table. Short version: USB HID is hardware-verified; BLE, LED, GUI and
pairing are built and host-tested but not yet run on hardware.

### Added
- Wire protocol v1 (`protocol/SPEC.md`), generated golden vectors, Rust (`kvmit-protocol`) and C (`proto_frame`)
  codecs tested against the same vectors.
- Firmware: NimBLE peripheral with LE Secure Connections bonding gated by a physical pairing window,
  protocol dispatcher (handshake gate, ack dedup, keepalive release, link-drop release), status LED language,
  BOOT button gestures (short = pairing window, 10 s = erase trust), persisted device name.
- Desktop (`kvmit`): CLI and egui GUI; BLE client with retry/keepalive/motion accumulation; US layout;
  script engine (TOML format, variables, secrets, `wait_for` on the screen, confirm, dry-run, preview,
  DuckyScript import); V4L2 capture; built-in example scripts; Windows cross-compile.
- `scripts/rs.sh` (containerised Rust build/test/clippy/Windows cross-build).

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
