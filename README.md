# kvm-it

When you need a KVM to provision a headless appliance and don't want to shell out for full equipment.

kvm-it is an open-source desktop KVM built from two cheap parts: a generic USB HDMI capture card and an
ESP32-S3 that pretends to be a USB keyboard and mouse. The target machine needs no software, drivers, network
or Bluetooth — it works in BIOS/UEFI, installers, login screens and recovery environments.

```
Target HDMI out ──► USB HDMI capture card ──► controller PC ──► desktop app (video preview)

Controller keyboard/mouse ──► desktop app ──► Bluetooth LE ──► ESP32-S3 ──► USB HID ──► target PC
```

Bluetooth is only the controller-to-ESP32 link. The target-facing side is plain USB HID.

## Status — v0.1.0 (MVP, in progress)

Verification labels are strict: **built** = compiles; **host-tested** = automated tests pass on this
repo's CI/containers; **hardware-verified** = run on a physical board.

| Area | State |
|---|---|
| Firmware: USB keyboard + mouse (boot HID) | **hardware-verified** (enumeration via `lsusb`; M2 self-test typed) |
| Firmware: protocol dispatcher, dedup, keepalive, LED language, BOOT gestures | host-tested |
| Firmware: BLE (bonding, pairing window, GATT), LED driver, BOOT GPIO | built, **not hardware-verified** |
| Protocol v1 spec + shared vectors (C and Rust) | host-tested |
| Desktop: client (ack/retry/keepalive), script engine, DuckyScript import, layout | host-tested |
| Desktop: BLE transport + BlueZ pairing | built, **not hardware-verified** |
| Desktop: V4L2 capture | hardware-verified with a laptop UVC camera (frame grabbed); not with an HDMI capture card |
| Desktop: egui GUI | built, **not run against an adapter** |
| Windows | cross-compiles (`scripts/rs.sh windows`); never run; **no video backend yet**, pairing is the OS prompt |
| Built-in OOBE script | template only, never run on a real OOBE |

What a technician gets when it all checks out: plug the adapter's USB port into the target; its LED blinks
blue until a controller pairs (physical presence gates this), then goes green. The desktop app reconnects by
itself next time, shows the target's HDMI output via the capture card, forwards keyboard and mouse, and
replays scripts (text, keys, chords, waits on the screen) for setup flows.

## Quick start (Linux)

Needs `podman` (or docker) only; toolchains live in containers.

```bash
scripts/fw.sh test && scripts/fw.sh build     # firmware: host tests, build for ESP32-S3
scripts/fw.sh flash                           # over the board's COM port (see docs/hardware.md)
scripts/rs.sh build                           # desktop app -> desktop/target/release/kvmit
scripts/rs.sh windows                         # cross-compile kvmit.exe (compile-checked only)

desktop/target/release/kvmit pair             # press BOOT briefly on the adapter first
desktop/target/release/kvmit status
desktop/target/release/kvmit type "hello"
desktop/target/release/kvmit run script.toml --dry-run
desktop/target/release/kvmit                  # GUI
```

**Read [docs/hardware.md](docs/hardware.md) before flashing**: which USB-C port goes where, the LED and
button meanings, and the hardware checklist.

## Layout

| Path | Purpose |
|---|---|
| `firmware/` | ESP-IDF project (ESP32-S3, TinyUSB HID) |
| `protocol/` | [Wire protocol v1](protocol/SPEC.md) and generated golden vectors |
| `desktop/` | Rust workspace: `kvmit` app (CLI + GUI) and its library crates |
| `docs/` | [ux & scripting](docs/ux.md), [architecture](docs/architecture.md), [hardware](docs/hardware.md), [security](docs/security.md), [protocol](docs/protocol.md), [roadmap](docs/roadmap.md) |
| `scripts/` | build/flash helpers |

## Security note

kvm-it will be used to type credentials. Secrets are never logged and never persisted by default. Read
[docs/security.md](docs/security.md) before using it for anything you care about.

## License

MIT — see [LICENSE](LICENSE).
