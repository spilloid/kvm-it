<p align="center"><img src="docs/assets/logo.png" alt="kvm-it logo" width="120"></p>

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
  **Ctrl+Alt+Esc** releases them, and that chord never reaches the target. On Windows, the keys your OS would
  keep for itself (Win, Alt+Tab, Ctrl+Esc, Alt+F4, ...) go to the target too while you are captured.
- **Send the keys your OS would swallow.** One-click Ctrl+Alt+Del, Win, Alt+Tab, PrintScreen and more.
- **Zero-step reconnect.** Pair once. Move the cables to the next machine and the app reconnects by itself.
- **Replayable setup scripts.** Native TOML or imported DuckyScript: text, keys, chords, delays, and
  *wait-for-the-screen* steps, so one slow install screen does not wreck everything after it. Preview and dry
  run before anything is typed; abort leaves nothing held down.
- **Safe with secrets.** Passwords are masked, never logged, never saved. Pairing needs physical presence:
  you press a button on the adapter.
- **Honest status.** One row of colour-coded chips (green working, amber in progress, red broken, grey idle) shows
  adapter, target USB, video and input capture, always; each opens the controls it describes.
- **Open.** MIT-licensed; the wire protocol, firmware and every review round are in this repo.

## Quick start (Windows 11)

Download `kvmit-vX.Y.Z-windows-x64.msi` (or the `.zip`) from the Releases page and check it against its `.sha256`
file. Whether a release is code-signed is stated in its release notes; an unsigned build makes Windows SmartScreen
warn. Open **kvm-it** from the Start menu (`kvmit-gui.exe`), or use `kvmit.exe` from a terminal.

Pair as on Linux: plug the adapter's **USB** port into the target and press **BOOT** briefly (or re-plug it) to open
its pairing window, then run `kvmit pair` or use the app's **Adapter** chip. Windows pairs by itself; no system
dialog appears.

You need Bluetooth LE, a graphics driver with OpenGL 2.0 or newer (the app says so in a dialog if it cannot start;
a software OpenGL works in a virtual machine), and, for the video, Settings > Privacy & security > Camera >
*Let desktop apps access your camera* turned on. Windows 11 is what was tested; the app asks Windows for a faster
Bluetooth connection where that API exists, and without it fast mouse movement can overwhelm the link.

## Quick start (Linux)

**AppImage:** download `kvm-it-X.Y.Z-x86_64.AppImage` from the Releases page (check it against its `.sha256`), `chmod +x`
it and run it for the GUI, or `./kvm-it-X.Y.Z-x86_64.AppImage cli scan` for the command line. It needs BlueZ, a
graphics driver (OpenGL) and the usual desktop libraries from your distro (`libxkbcommon`, plus `libxkbcommon-x11` and the X11
libraries or Wayland, whichever your session uses); only the C library baseline is checked at build time. It runs on Debian 12 / Ubuntu 22.04 (glibc 2.35) and newer. 0.2.0's AppImage was started on Linux (CLI
and GUI) but a Linux hardware pass with a board has not been run.

Or build it yourself. You need `podman` (or docker). Toolchains live in containers.

```bash
git clone https://github.com/spilloid/kvm-it && cd kvm-it
scripts/rs.sh build          # desktop app  -> desktop/target/release/kvmit
scripts/fw.sh build          # firmware
scripts/fw.sh flash          # with the adapter's COM port on your PC and its USB port unplugged
# or, from the app: GUI Adapter > "Flash adapter…", or `kvmit flash` (see docs/hardware.md)
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

## Status: v0.2.1

Labels are strict: **built** = compiles; **host-tested** = automated tests pass in CI/containers;
**hardware-verified** = run on a physical board; **VM-verified** = run in a Windows 11 virtual machine on a Linux
host with the Bluetooth adapter and the capture card passed through over USB (the real radio and the real card, but
not bare-metal Windows).

| Area | State |
|---|---|
| Firmware: USB keyboard + mouse (boot HID) | **hardware-verified** (enumeration via `lsusb`; self-test typed) |
| Firmware: protocol dispatcher, dedup, keepalive, LED language, BOOT gestures | host-tested |
| Firmware: BLE (bonding, pairing window, GATT), BOOT GPIO | **hardware-verified** (BOOT opens the window; LE SC bond; HELLO/STATUS over GATT; bonded reconnect) |
| Firmware: LED driver (GPIO48), BOOT gestures | **hardware-verified** (fast blue while pairing; 10 s BOOT hold: yellow ramp, three red flashes, fast blue, bond erased) |
| Protocol v1 spec + shared vectors (C and Rust) | host-tested |
| Desktop: client (ack/retry/keepalive), script engine, DuckyScript import, layout | host-tested |
| Desktop: BLE transport + BlueZ pairing | **hardware-verified** on Linux/BlueZ (Intel AX201): `scan`, `pair`, `status` (90 ms RTT); `type`/`key` **hardware-verified** into a Windows 11 target (every printable US-ASCII character, checked on screen through the capture card) |
| Desktop: BLE on Windows (WinRT pairing, `scan`, `status`, `unpair`, faster-connection request) | **VM-verified**: pairing with no system dialog, `status` (30 ms RTT with the fast connection, 120 ms without), `key --hold`; sustained mouse motion at 60 and 125 frames/s no longer collapses the link. `kvmit type` was not run from Windows. Bare-metal Windows: not tested |
| Desktop: V4L2 capture | **hardware-verified** with an HDMI capture card (MacroSilicon `345f:2109`): 1920x1080 frame of a live Windows 11 desktop via `kvmit video snap` |
| Desktop: Media Foundation capture (Windows) | **VM-verified** with the same MacroSilicon card: `video list`, a 1920x1080 MJPEG `video snap`, live video in the GUI, and a notice (not a frozen frame) when the card is unplugged mid-stream. The YUY2 path is untested |
| Desktop: egui GUI, v0.1.0 layout (left panel) | **maintainer-checked** on Linux with an adapter and a target (2026-10-03), including the input-ownership changes |
| Desktop: egui GUI, v0.2.0 layout (top-bar status chips, popups, run-log strip) | **VM-verified** on Windows (chips, popups, capture, error dialog). **Not yet checked on Linux** since the redesign; exposes a UI Automation tree for screen readers and tests. No automated GUI tests |
| Desktop: Windows keyboard grab (Win, Alt+Tab, ... go to the target while captured) | **VM-verified**: Win and Alt+Tab never reach the controller while captured, a held key reaches the adapter, Ctrl+Alt+Esc releases and the keyboard returns, and a hung GUI cannot trap the keyboard (the helper stops swallowing after 3 s). Not verified: non-US layouts and IMEs, bare metal. Linux has no equivalent |
| Desktop: shared client change (mouse motion split into 127-unit frames, ordered before clicks) | host-tested; **not re-run on a Linux board yet** |
| Windows as the controller (app) | **v0.2.1**: runs in a Windows 11 VM with real hardware passed through (rows above); **not verified on bare-metal Windows**. Windows as the *target* works as before: it only sees a USB keyboard and mouse |
| Desktop: flash the adapter (`kvmit flash`, GUI **Flash adapter…**) | **hardware-verified** on Linux with a physical board over its COM port (`kvmit flash`): verified write of bootloader, partition table and app; the pairing survives a default flash and is wiped by `--erase-all`; refuses while the adapter's own USB port is plugged in (also with `--any-port`). **VM-verified** on Windows 11 (board's COM bridge passed through): the same CLI and the GUI wizard, end to end. **Not exercised:** an interrupted flash, the Linux GUI wizard, bare-metal Windows |
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
