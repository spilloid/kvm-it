# kvm-it

**A KVM for the machine that has nothing on it yet.**

Provisioning a headless appliance, a fresh install, a BIOS setting, a recovery shell: you need a screen and a
keyboard on a machine that has no network, no remote-access agent and no OS. Real KVM hardware costs hundreds
of dollars. kvm-it is two cheap parts and an open-source app:

- a generic **USB HDMI capture card** shows you the target's screen, and
- an **ESP32-S3** board pretends to be a USB keyboard and mouse, driven from your PC over Bluetooth LE.

The target needs no software, drivers, network or Bluetooth. If it takes a USB keyboard, kvm-it can drive it,
from the BIOS splash screen onward.

```
Target HDMI out ──► USB capture card ──► your PC ──► kvm-it (live video)

Your keyboard/mouse ──► kvm-it ──► Bluetooth LE ──► ESP32-S3 ──► USB HID ──► target PC
```

## What you get

- **See and drive the target** in one window. Click the picture to capture your keyboard and mouse;
  **Ctrl+Alt+Esc** releases them, and that chord never reaches the target.
- **Send the keys your OS would swallow.** One-click Ctrl+Alt+Del, Win, Alt+Tab, PrintScreen and more.
- **Zero-step reconnect.** Pair once. Move the cables to the next machine and the app reconnects by itself.
- **Replayable setup scripts.** Native TOML or imported DuckyScript: text, keys, chords, delays, and
  *wait-for-the-screen* steps, so one slow install screen does not wreck everything after it. Preview and dry
  run before anything is typed; abort leaves nothing held down.
- **Safe with secrets.** Passwords are masked, never logged, never saved. Pairing needs physical presence:
  you press a button on the adapter.
- **Honest status.** One status bar shows adapter, target USB, video and input capture, always.
- **Open.** MIT-licensed; the wire protocol, firmware and every review round are in this repo.

## Quick start (Linux)

You need `podman` (or docker). Toolchains live in containers.

```bash
git clone https://github.com/spilloid/kvm-it && cd kvm-it
scripts/rs.sh build          # desktop app  -> desktop/target/release/kvmit
scripts/fw.sh build          # firmware
scripts/fw.sh flash          # with the adapter's COM port on your PC and its USB port unplugged
```

Plug the adapter's **USB** port into the target. For the next **15 s** it accepts a new pairing (after that,
press **BOOT** briefly to reopen the window), so run:

```bash
desktop/target/release/kvmit pair      # once
desktop/target/release/kvmit           # the app
```

Full walkthrough, parts list and troubleshooting: **[docs site](https://spilloid.github.io/kvm-it/)** or
[docs/getting-started.md](docs/getting-started.md). **Read [docs/hardware.md](docs/hardware.md) before flashing:**
which USB-C port goes where, what the LED and button mean.

The CLI does everything the app does:
`kvmit scan | status | type "text" | key ctrl alt delete | run script.toml [--dry-run] | import payload.txt`.

## Status: v0.1.0

Labels are strict: **built** = compiles; **host-tested** = automated tests pass in CI/containers;
**hardware-verified** = run on a physical board.

| Area | State |
|---|---|
| Firmware: USB keyboard + mouse (boot HID) | **hardware-verified** (enumeration via `lsusb`; self-test typed) |
| Firmware: protocol dispatcher, dedup, keepalive, LED language, BOOT gestures | host-tested |
| Firmware: BLE (bonding, pairing window, GATT), BOOT GPIO | **hardware-verified** (BOOT opens the window; LE SC bond; HELLO/STATUS over GATT; bonded reconnect) |
| Firmware: LED driver (GPIO48), BOOT gestures | **hardware-verified** (fast blue while pairing; 10 s BOOT hold: yellow ramp, three red flashes, fast blue, bond erased) |
| Protocol v1 spec + shared vectors (C and Rust) | host-tested |
| Desktop: client (ack/retry/keepalive), script engine, DuckyScript import, layout | host-tested |
| Desktop: BLE transport + BlueZ pairing | **hardware-verified** on Linux/BlueZ (Intel AX201): `scan`, `pair`, `status` (90 ms RTT); `type`/`key` **hardware-verified** into a Windows 11 target (every printable US-ASCII character, checked on screen through the capture card) |
| Desktop: V4L2 capture | **hardware-verified** with an HDMI capture card (MacroSilicon `345f:2109`): 1920x1080 frame of a live Windows 11 desktop via `kvmit video snap` |
| Desktop: egui GUI (capture, Send keys toolbar, scripts) | **maintainer-reported working** on Linux with an adapter and a target (2026-10-03); not independently logged |
| Windows | cross-compiles (`scripts/rs.sh windows`); never run; **no video backend yet**, pairing is the OS prompt |
| Built-in OOBE script | template only, never run on a real OOBE |
| Session recording | planned |

## Security note

kvm-it types credentials into other machines. Secrets are never logged and never persisted by default.
Read [docs/security.md](docs/security.md) before using it for anything you care about.

## Repository

| Path | Purpose |
|---|---|
| `firmware/` | ESP-IDF project (ESP32-S3, TinyUSB HID, NimBLE) |
| `protocol/` | [Wire protocol v1](protocol/SPEC.md) and generated golden vectors |
| `desktop/` | Rust workspace: `kvmit` app (CLI + GUI) and its library crates |
| `docs/` | The [docs site](https://spilloid.github.io/kvm-it/) source: UX and scripting, architecture, hardware, security, roadmap, review log |
| `scripts/` | Containerised build, test and flash helpers |

## License

MIT. See [LICENSE](LICENSE).
