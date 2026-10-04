# Get started

## What you need

| Part | Notes |
|---|---|
| ESP32-S3 dev board with two USB-C ports | One port is a serial/flash port (**COM**), the other is the native USB the target sees (**USB**). Details in [hardware](hardware.md). |
| USB HDMI capture card (UVC) | Any generic one. Optional: you can drive input without video. |
| A PC with Bluetooth LE | The controller runs on Linux (BlueZ) or Windows 11. The *target* can run any OS. |

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

On **Windows 11**, install the `.msi` (or unzip the `.zip`) from the release page and check it against its `.sha256`;
the release notes say whether it is code-signed (an unsigned build makes SmartScreen warn). Open **kvm-it** from the
Start menu. The app needs a graphics driver with OpenGL 2.0+ (it tells you in a dialog if not), and the video needs
*Settings > Privacy & security > Camera > Let desktop apps access your camera* turned on.

## Pair (once)

1. Have `kvmit pair` (or the app's **Adapter** chip, then **Pair & connect**) ready on your PC. On Windows this
   pairs without any system dialog.
2. Plug the adapter's **USB** port into the target. For **15 s** after it powers up it accepts a new pairing,
   even if it was paired before, so start pairing right away. The LED is meant to blink blue fast while it offers
   to pair.
3. Missed it? Press **BOOT** briefly: that opens the window for 5 minutes. Physical presence (plugging in, or the
   button) is the whole authentication, so this is deliberate.

From then on the app reconnects by itself whenever the adapter is in range.

## Use it

Run `kvmit` to open the app.

The top bar is a row of status chips and buttons; the picture fills the rest of the window.

- **See**: click the **Video** chip and pick your capture card from the list (one click switches devices; **Rescan**
  finds a card you plugged in after the app started). The target's screen appears and the chip turns green with the
  resolution and frame rate.
- **Drive**: click the picture (or the **Input** chip). Your keyboard and mouse now go to the target. The frame
  turns red and the Input chip reads *INPUT CAPTURED*. **Ctrl+Alt+Esc** releases; that chord is never sent. On
  Windows, keys such as Win and Alt+Tab go to the target too while you are captured.
- **Send keys your OS would swallow**: the **Keys** button has Ctrl+Alt+Del, Win, Alt+Tab, PrintScreen and more
  (Ctrl+Alt+Del and Win+L can never be intercepted on Windows, so use this for them).
- **Type text**: the **Type** button types a string (tick *Secret* for passwords: masked, never logged).
- **Run a script**: put `.toml` or DuckyScript files in your scripts folder (`~/Documents/kvm-it/scripts`), open
  **Scripts**, pick one, review the preview, and press Run. Progress shows in a strip along the bottom, with Abort.
  See [scripts and replay](ux.md).

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

- The chips always show adapter, target-USB, video and capture state: green is working, amber is in progress, red
  is broken, grey is idle. Read them first.
- The app will not start and shows an OpenGL message: update the graphics driver (in a virtual machine without a
  GPU, use a software OpenGL).
- Video shows *not opened* or stops: check the card is plugged in, that no other app is using it, and (Windows) the
  camera privacy setting above.
- Adapter never appears in a scan: see *BLE troubleshooting* in the [hardware guide](hardware.md).
- Keys stuck on the target: *Release all keys* in the **Adapter** popup. If the app dies, the adapter is designed to
  release everything itself within about five seconds (not yet verified on hardware).

## Treat it with care

kvm-it types credentials into other machines. Read the [security model](security.md) first.
