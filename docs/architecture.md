# Architecture

Status: design for the whole product; only the firmware HID layer exists (v0.1.0). Where a choice below is
unproven it says so.

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
2. BLE init *(M3)*.
3. Trusted-controller discovery/provisioning from NVS *(M11)*.
4. Command loop: protocol frame → `hid_state` → USB report *(M4+)*.

Modules in `firmware/main/`:

| File | Role | Host-tested |
|---|---|---|
| `hid_state.[ch]` | pressed keys/modifiers/buttons; duplicate-safe; 6-key limit refuses a 7th key rather than faking phantom keys; release-all | yes |
| `ascii_us.[ch]` | ASCII→usage, self-test only | yes |
| `usb_hid.[ch]` | descriptors, TinyUSB glue, mutex-serialised senders over one `hid_state_t` | no (hardware) |
| `hid_selftest.[ch]` | M2 deterministic test, one task, always ends with release-all | no (hardware) |
| `main.c` | boot sequence | no |

Why two HID interfaces rather than one report-ID composite: BIOS/UEFI keyboard drivers reliably handle a
boot-protocol keyboard that owns its interface; report-ID composite devices are a common failure in firmware.
Cost: two endpoints. Rejected: single interface with report IDs (works in OSes, riskier pre-boot).

Watchdog/recovery: task WDT (10 s, panic→reset), interrupt WDT and bootloader WDT are enabled; the chip is
bus-powered by the target, so any hang reboots and re-enumerates. The USB device does not depend on BLE.

## Desktop architecture (planned)

Cargo workspace with one crate per concern so no layer reaches into another:

| Crate | Responsibility |
|---|---|
| `kvmit-protocol` | frame encode/decode, consumes `protocol/vectors.json` |
| `kvmit-hid` | key/button/event model, HID usage codes, release-all semantics |
| `kvmit-layout` | layout trait + US ANSI (text → key events) |
| `kvmit-ble` | transport trait + btleplug implementation, scan/identify/reconnect |
| `kvmit-video` | capture trait + V4L2 backend (Media Foundation later) |
| `kvmit-macro` | structured macro model (TOML/JSON), executor |
| `kvmit-config` | persistence; secrets never written unless explicitly opted in |
| `kvmit-input` | platform input capture/grab (Linux first) |
| `kvmit-app` | application state model (device-scoped, no singletons) |
| `kvmit-ui` | the egui front end |

State is modelled per *device session* (one ESP32 + optionally one capture device) held in a map keyed by
device UUID, so several adapters are an extension, not a rewrite.

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

### Video — planned

Linux: V4L2 through a trait (`CaptureBackend`). Prefer MJPEG/YUYV as delivered; decode only for display.
No transcoding. Windows backend (Media Foundation) slots in behind the same trait. Hotplug: udev monitor +
reopen loop. Mode list from `VIDIOC_ENUM_FMT/FRAMESIZES/FRAMEINTERVALS`.

### Input capture — planned

Relative motion while captured with cursor grab/hide; the release chord (default Ctrl+Alt+Esc) is consumed by
the app and never forwarded; release-all is sent on capture exit, focus loss, BLE reconnect and app close.

## Hotplug (first-class requirement)

Three independent link states, each with its own reconnect loop, none fatal to the others: **BLE link**,
**capture device**, **target USB** (invisible to the controller; seen as the ESP32 rebooting and the BLE
link dropping). On every BLE (re)connect the controller sends handshake then `RELEASE_ALL`. Video reacquires
the signal without touching BLE state.
