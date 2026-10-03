# Hardware setup — YD-ESP32-23 (ESP32-S3-N16R8, rev 2022-v1.3)

## The two USB-C connectors

The board has two USB-C ports. They are **not interchangeable**:

| Port (silkscreen) | Wired to | Use it for |
|---|---|---|
| **COM** | CH343 USB-UART bridge → ESP32-S3 UART0 | **Development**: flashing, serial log. Connect to your controller/dev PC. |
| **USB** | ESP32-S3 native USB (GPIO19/20) | **Target**: this is the keyboard/mouse the target computer sees. Connect to the target. |

Evidence from this repo's development host: with only the COM port connected, Linux reports
`1a86:55d3 QinHeng Electronics USB Single Serial` (a WCH CH343) and creates `/dev/ttyACM0`. That matches the
COM port. The silkscreen names above are from the board vendor's documentation; **confirm them on your board**
(the label next to each connector) before relying on them.

### Rules

1. **Target gets the USB port.** In normal operation only the USB port is connected, to the target. The board is
   powered by the target.
2. **Do not plug the USB port into the machine you are developing on** while the self-test is flashed: ~8 s
   after boot the board types `HELLO FROM KVM`, presses Enter and moves the mouse *on whatever machine it is
   plugged into*. Use a separate target, or leave the USB port unplugged while you flash.
3. During development you may connect COM to your dev PC and USB to a test target at the same time. Both ports
   can power the board; if your board revision has a power-selection jumper/solder pad, check it before
   connecting two different computers at once (two hosts feeding 5 V into one board is best avoided). **This
   has not been verified on rev 2022-v1.3 — check the board's schematic or just power via one host.**
4. The firmware keeps its console on UART0 (COM port) on purpose, because the native USB peripheral belongs to
   the HID device.

### Buttons

- **BOOT** (GPIO0, read by the firmware at run time):
  - short press (< 3 s): open the **pairing window** for 300 s;
  - hold 10 s: **erase the bonded controller** and reopen the window (LED shows progress, below);
  - hold it while pressing RESET: ROM download mode (hardware behaviour, unchanged).
- **RESET** (EN): reboots the chip; the USB device re-enumerates on the target and the adapter comes back
  with its last configuration (name and bond are stored in flash). Software cannot see this button.

### Status LED (on-board RGB, GPIO48 per the vendor — **not yet verified on this board; set
`CONFIG_KVMIT_LED_GPIO` to -1 or another pin if it stays dark**)

| LED | Meaning |
|---|---|
| white, steady | booting |
| **blue, fast blink** | pairing window open — a controller may pair now |
| blue, brief tick every 1.5 s | a controller is bonded; waiting for it to connect |
| magenta, brief tick every 3 s | nothing bonded and the window is closed; radio is quiet — press BOOT |
| **green, steady** | controller connected, encrypted and handshaken |
| brief white flash | an input command was just applied |
| amber blip every 2 s (on top of anything) | the **target has not enumerated the USB port** (cable, power, suspend) |
| yellow, blinking faster, then solid (BOOT held ≥ 3 s, solid at 10 s) | trust reset in progress; release before 10 s to cancel |
| red, 3 flashes | trust erased |
| red, fast blink | BLE failed to start (USB HID still works) |

Colours are intentionally dim (≤ 40/255).

## Linux build and flash

Requirements: `podman` (or set `CONTAINER_RUNTIME=docker`). Nothing else is installed on the host. The
toolchain is the pinned image `docker.io/espressif/idf:v5.5` (override with `IDF_IMAGE`).

```bash
git clone https://github.com/spilloid/kvm-it && cd kvm-it
scripts/fw.sh test      # host unit tests (no hardware needed)
scripts/fw.sh build     # first build downloads esp_tinyusb from the ESP component registry
```

### Serial-port permission (one-time)

`/dev/ttyACM0` is `root:dialout` mode 0660. Your user must be in `dialout`:

```bash
sudo usermod -aG dialout "$USER"     # then log out and back in
```

Quick temporary alternative (resets on replug/reboot): `sudo setfacl -m u:$USER:rw /dev/ttyACM0`.
If `usermod` reports the group is missing, `getent group dialout` shows whether it exists on your host
(it does on the development host, with no members).

> **Group membership only applies after a fresh login.** `sg dialout -c scripts/fw.sh ...` does *not* work
> with rootless podman (user-namespace mapping fails). Until you re-login, flash with a host-side esptool:
> `python3 -m venv .venv-esptool && .venv-esptool/bin/pip install esptool` then, from `firmware/build`,
> `sg dialout -c ".../esptool --chip esp32s3 --port /dev/ttyACM0 -b 460800 write-flash @flash_args"`.
> This is how the first flash was done on the development host (hash-verified).

### Flash and watch

```bash
scripts/fw.sh flash                # defaults to /dev/ttyACM0; or: scripts/fw.sh flash /dev/ttyACM1
scripts/fw.sh monitor              # Ctrl-] exits
# or both:
scripts/fw.sh flash-monitor
```

If the device node is different (`ls /dev/ttyACM* /dev/ttyUSB*`), pass it as the second argument.

## Verification checklist — please report back

Already hardware-verified (2026-10-03): flash over COM; USB enumeration (`lsusb`: `303a:4008`, keyboard + mouse
boot HID); the old M2 self-test typed. The 0.1.0 firmware **no longer types at boot** (self-test is the
`KVMIT_SELFTEST` option, default off).

New in 0.1.0, all unverified on hardware until you report:

1. **Boot log** over COM: `BLE ready, trusted controller none`, `pairing window open for 300 s`, `advertising`.
2. **LED** behaves per the table above (fast blue while pairing). If dark, GPIO48 is wrong for this board.
3. **Pair**: `kvmit pair` on a Linux controller (press BOOT first if the window has closed) → "paired and connected".
4. **Status**: `kvmit status` shows target USB enumerated, a round trip time, firmware 0.1.0.
5. **Type**: with the USB port on a target, `kvmit type "hello"` and `kvmit key ctrl alt delete` work.
6. **Reconnect**: unplug/replug the adapter; `kvmit status` reconnects with no pairing step.
7. **Stuck keys**: hold a key via the GUI capture then kill the app (`kill -9`): within ~5 s the target's key
   releases. Disconnecting Bluetooth releases immediately.
8. **Trust reset**: hold BOOT 10 s → LED yellow ramp, three red flashes, fast blue; a second controller can now pair.
9. **BIOS/UEFI** (if available): keyboard works, and report whether the 5-byte mouse report is accepted.
10. **GUI** (`kvmit`): connect, capture keyboard/mouse, Ctrl+Alt+Esc releases; with a capture card, video shows.

## Known hardware unknowns

- Keyboard SET_IDLE retransmission is not implemented (hosts normally generate their own key repeat); if a BIOS
  fails to auto-repeat held keys, that is why.
- Mouse report is the TinyUSB 5-byte (buttons, X, Y, wheel, pan) descriptor in a boot-protocol interface.
  Some BIOSes only accept 3-4 byte boot mouse reports. If item 6 fails for the mouse, the fallback is a
  boot-compatible 4-byte report; the keyboard is unaffected.
- VID/PID: the firmware uses esp_tinyusb's default Espressif identifiers. Before any public release a proper
  VID/PID allocation is needed (see roadmap).
- PSRAM (8 MB octal) is not enabled in this milestone.
