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
  - short press (< 3 s): open the **pairing window** for 300 s (a 15 s window also opens by itself at every
    power-on or RESET; with a bond stored, crash/watchdog reboots do not open it);
  - hold 10 s: **erase the bonded controller** and reopen the window (LED shows progress, below);
  - hold it while pressing RESET: ROM download mode (hardware behaviour, unchanged).
- **RESET** (EN): reboots the chip; the USB device re-enumerates on the target and the adapter comes back
  with its last configuration (name and bond are stored in flash). Like a plug-in, it opens the 15 s pairing
  window.

### Status LED (on-board RGB, GPIO48 — verified on this board 2026-10-03; set
`CONFIG_KVMIT_LED_GPIO` to -1 or another pin if a different board stays dark)

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

Hardware-verified 2026-10-03 (Surface Laptop 4, Intel AX201, BlueZ 5.x; board on COM only, USB port not on a
target): boot log, BOOT short press opening the window, `kvmit scan`, `kvmit pair` ("paired and connected",
LE Secure Connections bond), and `kvmit status` (protocol v1.0, firmware 0.1.0, 90 ms round trip) including a
bonded reconnect. Items 1, 3 and 4 below are done except the "target USB enumerated" part of 4.

Still unverified on hardware until you report:

1. **Boot log** over COM: `BLE ready, trusted controller none`, `pairing window open for 300 s`, `advertising`.
   *(Verified.)*
2. **LED** behaves per the table above (fast blue while pairing). If dark, GPIO48 is wrong for this board.
   *(Verified 2026-10-03: fast blue in the pairing window; the trust-reset sequence below. Other states not
   individually checked.)*
3. **Pair**: `kvmit pair` on a Linux controller within 15 s of power-on (or after a BOOT press) → "paired and
   connected". *(Verified.)*
4. **Status**: `kvmit status` shows target USB enumerated, a round trip time, firmware 0.1.0. *(Verified
   except target USB, which needs the USB port on a target.)*
5. **Type**: with the USB port on a target, `kvmit type "hello"` and `kvmit key ctrl alt delete` work.
   *(Verified 2026-10-03 into a Windows 11 target: every printable US-ASCII character, chords Win+R and Ctrl+N,
   Enter/Escape; Ctrl+Alt+Del not sent. Read back through the capture card. If the target sleeps or locks
   mid-test, keys go to the lock screen: disable sleep on the target first.)*
6. **Reconnect**: unplug/replug the adapter; `kvmit status` reconnects with no pairing step. *(Verified 2026-10-03:
   physical replug and many COM-triggered power-on resets.)*
7. **Stuck keys**: hold a key via the GUI capture then kill the app (`kill -9`): within ~5 s the target's key
   releases. Disconnecting Bluetooth releases immediately.
8. **Trust reset**: hold BOOT 10 s → LED yellow ramp, three red flashes, fast blue; a second controller can now pair.
   *(Verified 2026-10-03: LED sequence as described; log `erasing BLE trust`, 300 s window; the controller's old
   keys are then rejected (0x205) and it pairs afresh.)*
9. **BIOS/UEFI** (if available): keyboard works, and report whether the 5-byte mouse report is accepted.
10. **GUI** (`kvmit`): connect, capture keyboard/mouse, Ctrl+Alt+Esc releases; with a capture card, video shows.

## BLE troubleshooting

- **Opening the COM port resets the board** (the CH343's DTR/RTS lines drive EN/IO0). That reset closes a BOOT
  window but opens the 15 s power-on window, so pair right after opening the serial monitor, or press BOOT.
- **The controller PC can't see the adapter** (`kvmit scan` empty, phone sees it): check the PC's scanner
  before the firmware. Run `sudo btmon -w /tmp/bt.log` during `bluetoothctl --timeout 20 scan le` and count
  `LE Advertising Report`s with `btmon -r /tmp/bt.log`. A healthy scan in a normal room gets hundreds in 20 s.
  On the development laptop (Intel AX201) the controller once got into a state where it passed up ~5 % of
  reports and missed every strong advertiser nearby; `sudo btmgmt power off && sudo btmgmt power on` fixed it.
- **Diagnostic firmware** — `scripts/fw.sh build-diag` builds into `firmware/build-diag/` with
  `sdkconfig.diag` (observer role + `CONFIG_KVMIT_RADIO_DIAG`). At boot it logs the stored bond(s), passively
  scans for 15 s and prints every advertiser with min/avg/max RSSI, then starts normally. To measure the
  PC→adapter path, make the PC advertise the marker first:
  `sudo btmgmt add-adv -d 0CFFFFFF4B564D495444494147 1` (manufacturer data `KVMITDIAG`); the board flags that
  line with `<<< KVMITDIAG HOST`. Remove with `sudo btmgmt rm-adv 1`. Never ship a diagnostic build.
- **Panics on connect**: decode with `xtensa-esp32s3-elf-addr2line -pfiaC -e <elf> <backtrace addrs>` inside
  `scripts/fw.sh shell`. See the NimBLE `slave_conn` note in `firmware/sdkconfig.defaults` for the one found so far.

## HDMI capture card notes

Verified 2026-10-03 with a MacroSilicon `345f:2109` UVC card (MJPEG, up to 1920x1080) on `/dev/video5`:
`kvmit video list` lists it and `kvmit video snap --path /dev/video5 out.png` saves a 1080p frame of the source.

- **"No signal" looks like a flat dark frame**, not an error: every pixel at luma 22. Check with
  `ffmpeg -i out.png -vf signalstats,metadata=print -f null -` (YMAX=22 means no signal).
- **The card may only lock onto a source that appears after it started.** If the source shows the card as a
  display but frames stay flat, replug the card (or `echo 0 > /sys/bus/usb/devices/<port>/authorized`, then `1`).
- **The source must actually drive HDMI**: on Windows, Win+P → Duplicate.
- **Most of this card's MJPEG packets can be empty** (~92 % in one test). `kvmit` skips them; stock `ffmpeg`
  aborts on its default decode-error rate, so any ffmpeg pipeline must tolerate or filter empty packets.

## Known hardware unknowns

- Keyboard SET_IDLE retransmission is not implemented (hosts normally generate their own key repeat); if a BIOS
  fails to auto-repeat held keys, that is why.
- Mouse report is the TinyUSB 5-byte (buttons, X, Y, wheel, pan) descriptor in a boot-protocol interface.
  Some BIOSes only accept 3-4 byte boot mouse reports. If item 6 fails for the mouse, the fallback is a
  boot-compatible 4-byte report; the keyboard is unaffected.
- VID/PID: the firmware uses esp_tinyusb's default Espressif identifiers. Before any public release a proper
  VID/PID allocation is needed (see roadmap).
- PSRAM (8 MB octal) is not enabled in this milestone.
