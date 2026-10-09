# Architecture

Status: v0.1.0 implements everything below except where a section says otherwise; see the README status table
for what is hardware-verified. Where a choice is unproven it says so.

## Repository layout

```
firmware/   ESP-IDF project (C). hid_state + ascii_us are pure C and host-tested.
protocol/   spec + golden vectors shared by firmware and desktop tests
desktop/    Rust workspace (below)
docs/       this folder
scripts/    fw.sh (container build/test/flash)
```

## Protocol boundary

The boundary between controller and firmware is a versioned binary protocol over one BLE GATT
characteristic pair (write-without-response for input, notify for responses). See [protocol.md](protocol.md).

- The firmware knows **USB HID usage codes and button masks only**. It has no concept of text, layouts,
  Unicode, macros or key names.
- The desktop owns layouts, text→events, macros, timing, capture, and video.
- Consequence: adding a keyboard layout or macro feature never requires a firmware update.

Chosen: thin firmware, smart controller. Rejected: sending strings to the firmware for it to type (couples
layouts to firmware, makes secrets live in firmware logs/buffers longer, forces a firmware update per layout).

## Firmware architecture

Boot order (matches the product requirement that the target always sees a working HID):

1. `usb_hid_init()` — TinyUSB, two boot-protocol HID interfaces (keyboard, mouse+wheel).
2. BLE init: reconnect to the bonded controller, or open the pairing window if none is bonded.
3. Command loop: GATT write → queue → dispatcher (worker task) → `hid_state` → USB report; the BLE host task never blocks on USB.

Modules in `firmware/main/`:

| File | Role | Host-tested |
|---|---|---|
| `hid_state.[ch]` | pressed keys/modifiers/buttons; duplicate-safe; 6-key limit refuses a 7th key rather than faking phantom keys; release-all | yes |
| `ascii_us.[ch]` | ASCII→usage, self-test only | yes |
| `usb_hid.[ch]` | descriptors, TinyUSB glue, mutex-serialised senders over one `hid_state_t` | no (hardware) |
| `hid_selftest.[ch]` | M2 deterministic test, one task, always ends with release-all | no (hardware) |
| `proto_frame.[ch]` | framing: CRC, encode, decode (vectors shared with Rust) | yes |
| `proto_dispatch.[ch]` | handshake gate, ack dedup, errors, keepalive release; HID effects injected | yes |
| `led_pattern.[ch]`, `button_logic.[ch]` | LED language and BOOT gestures as pure functions | yes |
| `ble_link.c` | NimBLE peripheral: bonding, pairing window, GATT, worker task that owns the dispatcher | no (hardware) |
| `led.c`, `buttons.c`, `kvm_config.c` | WS2812 driver, GPIO0 polling, NVS (name) | no (hardware) |
| `main.c` | boot sequence | no |

Why two HID interfaces rather than one report-ID composite: BIOS/UEFI keyboard drivers reliably handle a
boot-protocol keyboard that owns its interface; report-ID composite devices are a common failure in firmware.
Cost: two endpoints. Rejected: single interface with report IDs (works in OSes, riskier pre-boot).

Watchdog/recovery: task WDT (10 s, panic→reset), interrupt WDT and bootloader WDT are enabled; the chip is
bus-powered by the target, so any hang reboots and re-enumerates. The USB device does not depend on BLE.

## Desktop architecture

Cargo workspace with one crate per concern so no layer reaches into another:

| Crate | Responsibility |
|---|---|
| `kvmit-protocol` | frame + typed message encode/decode, consumes `protocol/vectors.json` |
| `kvmit-hid` | HID usage codes, key names, mouse buttons |
| `kvmit-layout` | layout trait + US ANSI (text → key strokes); refuses what it cannot type |
| `kvmit-ble` | protocol client (handshake, ack/retry, keepalive, ordered motion frames capped at 127 units) + btleplug transport + pairing (BlueZ on Linux, WinRT on Windows, which also requests a fast connection interval) |
| `kvmit-video` | capture (MJPEG/YUYV, decoded only for display): V4L2 on Linux, Media Foundation on Windows (`mf.rs`); other platforms report unsupported; shared mode scoring and frame conversion |
| `kvmit-script` | script model/TOML, DuckyScript import, compile/validate, executor, screen comparison |
| `kvmit` | config, key mapping, script library, CLI (`kvmit`), the egui GUI (`gui.rs`; also its own executable `kvmit-gui`), and `syskeys.rs`, the OS-level keyboard grab |

(Config, app state and UI live in the single `kvmit` crate for now; split them if a second front end appears.)

Several adapters at once (multi-probe, roadmap 0.6.0): the script engine (`run` over a `Host`), the BLE `Device` and
`Capture` are all per instance, with no globals, so independent runs need only one host each. The CLI does this today
(one `kvmit run --device … --video …` process per adapter). The GUI does not yet: its `App` holds a single link,
capture and run, and becomes a list of per-probe sessions in 0.6.0.

### UI framework — chosen: egui/eframe

Rationale: the hard parts of this app are a live video surface and low-level input capture, not forms.
egui gives a straightforward immediate-mode texture upload for frames, trivial cross-platform (Linux/Windows
via wgpu or glow), and eframe exposes winit so raw device events and cursor grab are reachable. Single
language/toolchain, no web view.

Alternatives rejected: **Slint** — nicer declarative styling but video-texture and raw-input integration is
more work and its licensing needs checking for an MIT project; **iced** — capable but smaller ecosystem for
custom video widgets and still moving on API; **Electron/Tauri** — excluded by requirements (and web-view
video latency).

Caveat: this is a reasoned choice, not a measured one. Milestone 8 should verify 1080p60 frame upload
latency before the choice is considered settled, with iced as the documented fallback.

### Video

One `Capture` API (`list_devices`, `open`, `latest`, `failed`) with a backend per platform; the GUI, the `video`
CLI and script `wait` steps use only that. Prefer MJPEG/YUYV as delivered; decode only for display; no
transcoding. Linux: V4L2 (mode list from `VIDIOC_ENUM_*`). Windows: a Media Foundation Source Reader on the card's
native MJPEG/YUY2 types with converters disabled; everything COM stays on the capture thread, shutdown is bounded
(a stalled card cannot hang the GUI) and a terminal read error sets `failed()` so the GUI drops the stale frame.
Mode scoring (closest to 1080p, penalise < 25 fps, prefer MJPEG) is shared by both backends.

### Input capture

Relative motion while captured with cursor grab/hide; the release chord (Ctrl+Alt+Esc) is consumed by the app and
never forwarded; release-all is sent on capture exit, focus loss, BLE reconnect and app close.

**Windows keyboard grab (`syskeys.rs`).** The keys an OS keeps for itself (Win, Alt+Tab, ...) never reach a normal
window, so while captured a low-level keyboard hook swallows every key locally and hands it to the app. The logic
(physical key to HID usage by scan code, auto-repeat, keys already held before capture, the release chord) is
pure and unit-tested. The hook runs in a **helper process** (the app launches itself with `--keyboard-grab-helper`,
events over a pipe): inside the GUI process the hook callback was never invoked, and a separate process is also
the safer place for it. The helper handles the release chord itself, stops swallowing if the app's heartbeat (sent
from the input-handling thread) stops for 3 s, and exits when its parent goes away, so a hung or crashed GUI cannot
trap the keyboard. Keyboard events and egui's mouse buttons are separate streams, which is why modifier+click
ordering has a documented limit. Ctrl+Alt+Del and Win+L are handled by Windows itself and cannot be hooked.

## Hotplug (first-class requirement)

Three independent link states, each with its own reconnect loop, none fatal to the others: **BLE link**,
**capture device**, **target USB** (invisible to the controller; seen as the ESP32 rebooting and the BLE
link dropping). On every BLE (re)connect the controller sends handshake then `RELEASE_ALL`. Video reacquires
the signal without touching BLE state.
