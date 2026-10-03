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
- No competitive/market-positioning content in this public repo; strategy lives privately elsewhere.
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
| `docs/` | architecture, hardware, security, protocol draft, roadmap, dev-process log |

## Working here

The host has no ESP-IDF or Rust toolchain; use `scripts/fw.sh` (podman). `scripts/fw.sh test` and
`scripts/fw.sh build` must pass before any commit. The board enumerates on `/dev/ttyACM0` (COM port); the
user must be in `dialout`. **Never flash with the board's USB port attached to the dev machine** — the
self-test types into the machine it is plugged into.

## Things to re-verify before trusting them (dated, STD-003 rule 4)

- 2026-10-03: "COM = CH343 UART, USB = native" port labelling is vendor-documented and consistent with
  `lsusb 1a86:55d3` on COM, but not checked against this board's silkscreen.
- 2026-10-03: BLE pairing, LED GPIO48, the GUI and HDMI capture cards are unverified on hardware (see README table).
- 2026-10-03: 5-byte mouse report acceptance by BIOS boot protocol — unverified.
- 2026-10-03: egui/eframe 1080p60 latency — unmeasured.
