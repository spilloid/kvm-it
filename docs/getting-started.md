# Get started

## What you need

| Part | Notes |
|---|---|
| ESP32-S3 dev board with two USB-C ports | One port is a serial/flash port (**COM**), the other is the native USB the target sees (**USB**). Details in [hardware](hardware.md). |
| USB HDMI capture card (UVC) | Any generic one. Optional: you can drive input without video. |
| A Linux PC with Bluetooth LE and BlueZ | Windows builds exist but are not usable yet (no video backend, never run). |

## Install

Build it (needs only `podman` or `docker`; there are no packaged releases yet):

```bash
git clone https://github.com/spilloid/kvm-it && cd kvm-it
scripts/rs.sh build            # → desktop/target/release/kvmit
```

Flash the adapter once, with its **COM** port on your PC and its **USB** port *not* plugged into anything:

```bash
scripts/fw.sh build && scripts/fw.sh flash
```

## Pair (once)

1. Plug the adapter's **USB** port into the target. The LED is meant to blink blue while it offers to pair (the LED is not yet verified on hardware; the boot log over the COM port is the reliable signal).
2. If it has been more than five minutes, or the adapter was paired before, press **BOOT** briefly to reopen
   the pairing window. Physical presence is the whole authentication, so this step is deliberate.
3. On your PC: `kvmit pair`, or open `kvmit` and use **Scan → Pair & connect**.

From then on the app reconnects by itself whenever the adapter is in range.

## Use it

Run `kvmit` to open the app.

- **See**: pick your capture card in the *Video* panel. The target's screen appears.
- **Drive**: click the picture (or *Capture keyboard & mouse*). Your keyboard and mouse now go to the target.
  The frame turns red and the status bar says so. **Ctrl+Alt+Esc** releases; that chord is never sent.
- **Send keys your OS would swallow**: the *Send keys* panel has Ctrl+Alt+Del, Win, Alt+Tab, PrintScreen and
  more.
- **Type text**: the *Type text* box types a string (tick *Secret* for passwords: masked, never logged).
- **Run a script**: put `.toml` or DuckyScript files in `~/Documents/kvm-it/scripts`, pick one, review the
  preview, and press Run. See [scripts and replay](ux.md).

Everything is also available from the command line:

```bash
kvmit scan
kvmit status
kvmit type "hello"
kvmit key ctrl alt delete
kvmit run setup.toml --dry-run
kvmit import payload.txt
```

## When something is off

- The status bar always shows adapter, target-USB, video and capture state. Read it first.
- Adapter never appears in a scan: see *BLE troubleshooting* in the [hardware guide](hardware.md).
- Keys stuck on the target: *Release all keys* in the Adapter panel. If the app dies, the adapter is designed to
  release everything itself within about five seconds (not yet verified on hardware).

## Treat it with care

kvm-it types credentials into other machines. Read the [security model](security.md) first.
