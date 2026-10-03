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

- **BOOT** (GPIO0): hold while pressing/releasing **RESET** to force ROM download mode. Not needed for
  normal flashing over COM (auto-reset), but it is the fallback. From Milestone 11 a 10-second BOOT hold will
  also erase BLE trust data.
- **RESET**: reboots the chip. The USB device re-enumerates on the target.

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

Everything below needs the physical board; none of it has been run yet. Reply with pass/fail and the serial log.

1. **Flash succeeds** over COM (note esptool's reported chip, flash size = 16 MB, and any warnings).
2. **Serial log** shows `kvm-it firmware 0.1.0 ...` then `waiting 8000 ms before test`.
3. **Enumeration**: with the USB port connected to a *test* Linux machine, `lsusb` shows the device
   (default Espressif VID `303A`, product string `kvm-it HID adapter`), and `lsusb -v -d 303a:` shows two HID
   interfaces: keyboard (boot, protocol 1) and mouse (boot, protocol 2).
   `dmesg` should show `input: kvm-it ...` for both.
4. **Self-test** on the target, with a text field focused (Caps Lock state is tracked, but start with it off): ~8 s after boot `HELLO FROM KVM`, then Enter,
   then a ~20 px right-then-left pointer nudge. Serial log ends with `all keys released`.
5. **No stuck keys** afterwards: keyboard and pointer behave normally.
6. **BIOS/UEFI** (if available): keyboard works in BIOS setup *and* the mouse behaviour — report whether
   the 5-byte mouse report is accepted by that firmware (this is the least-certain item).
7. **Replug**: unplug and replug the USB port; the self-test runs again and enumeration is clean.
8. **Hot target change**: the board survives USB power loss (it is bus-powered) and comes back with no manual step.

## Known hardware unknowns

- Keyboard SET_IDLE retransmission is not implemented (hosts normally generate their own key repeat); if a BIOS
  fails to auto-repeat held keys, that is why.
- Mouse report is the TinyUSB 5-byte (buttons, X, Y, wheel, pan) descriptor in a boot-protocol interface.
  Some BIOSes only accept 3-4 byte boot mouse reports. If item 6 fails for the mouse, the fallback is a
  boot-compatible 4-byte report; the keyboard is unaffected.
- VID/PID: the firmware uses esp_tinyusb's default Espressif identifiers. Before any public release a proper
  VID/PID allocation is needed (see roadmap).
- PSRAM (8 MB octal) is not enabled in this milestone.
