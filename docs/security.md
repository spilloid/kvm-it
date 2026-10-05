# Security and credential handling

kvm-it exists to type setup passwords and commands into machines. Treat it as a keyboard that can be
remote-controlled over radio.

## Threat model (what we defend against)

1. A nearby BLE client trying to type into the target. **This is the main risk**: a successful attacker
   gets arbitrary keystrokes on the target, i.e. full control of a logged-in or unlocked machine.
2. Passive eavesdropping of typed credentials on the BLE link.
3. Secrets leaking into logs, config files or crash dumps on the controller.

Out of scope: a malicious *target* (it can already read what the USB HID device does), physical access to the
ESP32, and a compromised controller.

## Pairing and authentication — decision

BLE LE Secure Connections bonding gives encryption and authentication *if* configured for MITM-protected
pairing. This board has no display or keypad, so pairing would fall back to "Just Works", which is
**not MITM-protected**: an attacker present during first pairing could bond.

Decision, as implemented in 0.1.0 (pairing, bonding and the encrypted GATT link hardware-verified 2026-10-03 on
Linux/BlueZ; the refusal paths below are built and reviewed but not exercised on hardware):

- **LE Secure Connections bonding is required, and both GATT characteristics need an encrypted link.** An
  unbonded peer can never reach the protocol.
- **Physical presence gates pairing.** Pairing is accepted only inside a *pairing window*, which opens (a) for
  15 s after a power-on or RESET-pin reset, bonded or not (plug-in is presence;
  `CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S`, 0 disables it), and (b) for 300 s on a BOOT short press
  (`CONFIG_KVMIT_PAIRING_WINDOW_S`). With a bond stored, a crash/watchdog/software reboot does **not** open
  the window, so a remotely triggered crash cannot reopen pairing. With no bond, any boot opens it. The power-on
  window is offered once per chip boot (a Bluetooth stack restart does not reopen it), and bonding is refused at
  the deadline itself (the window timer clears it; the only slack is timer dispatch latency). It closes as soon as a pairing
  succeeds. Outside the window, an
  unbonded peer that connects is disconnected immediately and a re-pairing attempt from a bonded address is
  ignored.
- **One link at a time, enforced by NimBLE.** The host has a single connection slot, and NimBLE will not start
  connectable advertising while it is in use, so a second controller cannot connect at all. (This depends on
  `firmware/patches/esp-idf-v5.5-nimble-conn-handle-index.patch`; see `docs/dev-process.md`, 2026-10-03.)
- **One trusted controller.** A new pairing replaces the old bond. A 10 s BOOT hold erases all bonds and
  reopens the window. RESET (the EN line) reboots, which counts as a power-on: it opens the 15 s window.
- Outside a window, with a bond stored, the adapter advertises only to be found by that controller (slow interval).
- **Trade-off of the power-on window:** whenever the adapter regains power (re-plugged, or the target reboots and
  cuts USB power), anyone in radio range during those 15 s could pair and replace the bond, exactly as during a
  BOOT window. Set `CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S=0` to require BOOT for every new pairing.
- **Residual risk, stated plainly:** Just Works is not MITM-protected. An attacker in radio range *during the
  pairing window* could pair instead of you. The window is short, physically triggered and closes on first
  success; the LED blinks blue fast while it is open so you can see it. A per-connection challenge-response on
  top (the earlier plan) was dropped: the bond's encrypted link already provides it, and there is no display
  to confirm a code on.
- The earlier plan to show a pairing code on the serial console was dropped for the same reason.

## Secrets in the controller

- Typed text and secret macro fields are never logged, including at debug level (log lengths/counts only).
- Secret fields are masked in the UI, held in zeroizing buffers where practical (`zeroize`), and **not
  persisted by default**. MVP requires entering secrets each session.
- If persistence is added: opt-in per macro, stored through the OS keyring (never plaintext config), with a
  documented warning that anyone with access to your login session can then type your credentials.

## Credential-macro risks (to be repeated in the UI)

A stored credential macro lets anyone with access to the controller session (or a stolen unlocked laptop) type
that credential into any attached target. Prefer entering secrets per session; use unique, rotatable
provisioning credentials.

## Firmware-side notes

The firmware stores no typed text and has no logging of key events beyond counters. The self-test types only
the fixed non-secret string `HELLO FROM KVM`.

## The boot drive (0.4.0)

The adapter can also present a USB mass-storage drive carrying iPXE. **It is off by default** and is switched on deliberately (the Adapter popup, or `kvmit boot-drive on`;
the adapter stores the setting and restarts). While it is off, the adapter's USB descriptor is exactly the keyboard and mouse it has always presented. What the drive does
and does not change when it is on:

- **No write path.** The firmware reports the drive write-protected and rejects every write-class command, so a compromised target cannot alter the image or use the
  drive to persist anything on the adapter. Tested on the real adapter by sending raw SCSI commands straight at it, bypassing the host's own write-protect handling:
  WRITE(10) was refused as DATA PROTECT; WRITE(6), WRITE(12), FORMAT UNIT, WRITE SAME and UNMAP as invalid commands; the whole disk hashed identically afterwards.
- **The settings and pairing are not reachable from the drive:** it is its own flash partition; the settings partition is separate. Turning the drive on or off is a
  controller command, so it needs the paired controller; a target cannot turn it on.
- **A target that boots USB first will start iPXE** while the drive is on. The default script is **inert**: it waits five seconds for a key press and otherwise exits
  with a failure status, so the target carries on with its next boot device; nothing is fetched or run unless someone presses a key (or you put your own script on the
  drive). Turn the drive off when you are not using it.
- **What it adds is trust in the network path.** iPXE boots whatever the boot server offers. The default key-press path is the public iPXE demo (over HTTPS), which
  proves the path and is **not** for anything you care about: use your own server and signed images.
- **Secure Boot is the target's guard against unsigned boot code, and it will refuse this iPXE today.** Do not disable it on a machine you care about just to try this.
  Making the drive work with Secure Boot on is planned work, not a promise yet.

## Windows controller (0.2.0)

- **Pairing on Windows** uses the WinRT custom-pairing API and accepts the pairing request in code so no system
  dialog appears. Only the Just Works ceremony is offered and accepted (a peer asking for numeric comparison is
  refused, since this headless adapter cannot show a code). The adapter's physical pairing window is still the
  authentication; nothing about the trust model changed.
- **Keyboard grab.** While input is captured the app swallows keys locally and forwards them to the adapter. Which
  keys you pressed is never logged or persisted (diagnostics report counts only). The hook runs in a helper process
  that talks to the app over a private pipe; it passes keys through to the controller whenever nobody is
  listening, and a hung or crashed app cannot keep them (see architecture). A keyboard hook can look like keylogger
  behaviour to security software: it only exists while you are captured, and its source is in this repository.
- **Release signing.** A release is code-signed only if its notes say so; checksums (`.sha256`, `SHA256SUMS`)
  accompany every release asset.
