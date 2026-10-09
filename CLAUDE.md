# CLAUDE.md

## What this repo is

kvm-it: an open-source desktop KVM — generic USB HDMI capture card + ESP32-S3 acting as USB HID keyboard/mouse,
controlled over BLE from a Rust desktop app. Read `README.md`, then `docs/architecture.md`.

## Non-negotiable discipline

- **Never claim hardware behaviour was verified unless it was run on a physical board.** Say "built",
  "host-tested" or "hardware-verified" precisely. The README status table and the CHANGELOG must agree.
- **Never log typed text or secret macro contents**, at any log level. Log lengths/counts only.
- **Target-facing input is USB HID only.** Bluetooth is solely controller↔ESP32.
- **Firmware knows usage codes, not text.** Layouts, macros and text→keys live in the desktop app.
- **No secrets persisted by default.**
- Adversarially review non-trivial diffs with a different model (astra); reproduce findings before
  accepting; log rounds in `docs/dev-process.md` (company standard STD-001).

## Where things live

| Path | What |
|---|---|
| `firmware/main/hid_state.*` | pure-C HID state machine (host-tested) |
| `firmware/main/usb_hid.*` | TinyUSB descriptors + senders |
| `firmware/test/host/` | host unit tests |
| `firmware/main/proto_*`, `led_pattern.*`, `button_logic.*` | pure-C protocol framing/dispatcher, LED language, BOOT gestures (host-tested) |
| `firmware/main/ble_link.c`, `led.c`, `buttons.c`, `kvm_config.c` | hardware glue: NimBLE, LED driver, GPIO, NVS (hardware-only) |
| `protocol/` | SPEC.md + generated golden vectors shared by C and Rust tests |
| `desktop/` | Rust workspace (`kvmit` app + crates); `scripts/rs.sh test\|clippy\|build\|windows` (container) |
| `scripts/fw.sh` | test/build/flash via the ESP-IDF container |
| `desktop/crates/kvmit-flash/` , `kvmit/src/{flashcmd,flashwiz}.rs` | flash the adapter over its COM port (`kvmit flash`, GUI wizard): image/port/identity safety rules are in the crate and tested; never loosen them in the UI |
| `desktop/crates/kvmit/src/syskeys.rs` | keyboard grab: pure logic (tested), Windows helper process, macOS Quartz event tap; never log which keys |
| `desktop/crates/kvmit-video/src/avf.rs` | macOS capture (AVFoundation); `scripts/rs.sh macos` type-checks/lints the macOS build from Linux, the `macOS (preview)` CI job links and tests it |
| `desktop/crates/kvmit-video/src/demo.rs` | synthetic video source (`KVMIT_DEMO_VIDEO=<png>`): use it for any screenshot or demo, never a real machine's screen |
| `scripts/build-release.ps1`, `sign.ps1`, `verify-release.py`, `installer/`, `docs/RELEASING.md` | Windows release packaging (run on the release machine) |
| `docs/` | architecture, hardware, security, protocol draft, roadmap, dev-process log |

## New machine or fresh session: bootstrap first

When you start work on a machine you have not used in this repo, or the user says they hopped machines or just hooked
this one up, **run `scripts/bootstrap.sh` before anything else and report its output** (`--check` reports without
building). It checks git, `gh` login, podman, ffmpeg (needed to record), the `codex` CLI (astra review), free disk,
worktrees, the board's COM port access and BlueZ, and builds the Rust build container the first time. Do not assume any
of those exist on a new machine. If it says NOT ready, fix the MISS lines with the user before building or releasing.
Known trap: `/var` filling up (container builds then fail with "no space left"); `rm -rf desktop/target/debug` is
safe, it is only a cache.

## Working here

The host has no ESP-IDF or Rust toolchain; use `scripts/fw.sh` (podman). `scripts/fw.sh test` and
`scripts/fw.sh build` must pass before any commit. The board enumerates on `/dev/ttyACM0` (COM port); the
user must be in `dialout`. **Never flash with the board's USB port attached to the dev machine** — the
self-test types into the machine it is plugged into.

## Things to re-verify before trusting them (dated, STD-003 rule 4)

- 2026-10-08: maintainer hardware pass on 0.4.6 from **bare-metal Windows** and a **real Mac**: every button in the
  app's menu, with an adapter and a live capture card picture, and two boards reflashed from each OS (working after).
  Not covered by that pass: issues #31-#34 (capture-card unplug/no-signal/fps readback, recording with sound, YUYV
  colour, wheel/touchpad), the Windows keyboard grab's edge cases, non-US layouts, ISO Apple keyboards. 0.4.7's
  `kvmit run --video` / multi-adapter CLI is host-tested only: no two adapters have run at once.

- 2026-10-07: the macOS port was built and CI-tested only until the 2026-10-08 hardware pass above; the parts that pass
  did not reach (AVFoundation audio, the Accessibility-denied fallback) are still unrun. Known gap: ISO Apple keyboards swap the codes of the key left of 1 and the key left of Z (the table follows ANSI).
- 2026-10-06: 0.4.1-0.4.5 (video honesty, button contract, wheel/Esc abort, recording, light/dark theme) are host-tested only. Nothing ran on a capture card, a real target, the Linux GUI or Windows; see the "Needs hardware" issues #31-#34. Neither theme has been looked at on a real screen (contrast is unit-tested, not eyeballed).
- 2026-10-03: "COM = CH343 UART, USB = native" port labelling is vendor-documented and consistent with
  `lsusb 1a86:55d3` on COM, but not checked against this board's silkscreen.
- 2026-10-04: Windows-controller claims were VM-verified only (Windows 11 VM, adapter and capture card passed
  through over USB) until the 2026-10-08 bare-metal pass above; detail beyond that pass is still VM-only.
- 2026-10-04: the mouse-motion change in `kvmit-ble` (127-unit frames, ordered before clicks) and the GUI redesign
  are untested on a Linux board / the Linux GUI. Run `scripts/rs.sh build` and a Linux hardware pass before release.
- 2026-10-04: untested: the keyboard grab's hook-removal fallback and pre-held-key handling on real keys, the Media
  Foundation stalled-card shutdown, YUY2 capture, Windows 10, non-US layouts/IMEs. Deferred findings (review round 4):
  modifier+click ordering, end-to-end motion backpressure.
- 2026-10-04: 0.2.1's video-switch fixes after review round 6 (reopen click, sequence reset) are host-tested only; the
  AppImage's GUI start and a Linux board pass are unrun.
- 2026-10-04: releases are unsigned until a signing certificate is configured on the release machine; never state a
  release is signed unless `dist/SIGNATURES.txt` (from Get-AuthenticodeSignature) says Valid.
- 2026-10-06: 0.4.1's Linux video changes (dead-card detection, bounded drop, applied-mode readback with `set_params`, YUYV stride, stable device key, no-signal chip) are host-tested only. Unrun on a card: how a real UVC driver answers `set_params` / `set_format`, the `set_timeout` stall behaviour, real unplug detection, and whether the MacroSilicon no-signal fill is flagged blank. Tracked in the "Needs hardware" milestone (#31-#34).
- 2026-10-03: BLE pairing + GATT and HDMI capture (one MacroSilicon card) were hardware-verified on Linux; LED
  GPIO48 verified.
- 2026-10-03: 5-byte mouse report acceptance by BIOS boot protocol — unverified.
- 2026-10-03: egui/eframe 1080p60 latency — unmeasured.
