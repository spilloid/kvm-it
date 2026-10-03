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

Decision, as implemented in 0.1.0 (built and host-tested; **not yet hardware-verified**):

- **LE Secure Connections bonding is required, and both GATT characteristics need an encrypted link.** An
  unbonded peer can never reach the protocol.
- **Physical presence gates pairing.** Pairing is accepted only inside a *pairing window*, which opens (a) at
  boot when no controller is bonded (plug-in is presence), and (b) on a BOOT short press. It lasts
  `CONFIG_KVMIT_PAIRING_WINDOW_S` (120 s) and closes as soon as a pairing succeeds. Outside the window, an
  unbonded peer that connects is disconnected immediately and a re-pairing attempt from a bonded address is
  ignored.
- **One trusted controller.** A new pairing replaces the old bond. A 10 s BOOT hold erases all bonds and
  reopens the window. RESET (the EN line) only reboots; it cannot be observed by software.
- With a bond stored, the adapter advertises only to be found by that controller (slow interval) and never
  opens pairing on its own.
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
