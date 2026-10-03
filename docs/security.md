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

Decision (to be implemented in Milestone 11, recorded now so Milestones 3-10 do not assume otherwise):

- Require LE Secure Connections + bonding, and reject any characteristic access from unbonded peers.
- Add an **application-level authenticated handshake** on top, because Just Works bonding alone does not
  authenticate the human intent: provisioning mode (entered after trust reset, indicated physically) accepts
  a controller only when a **pairing code printed/logged on the COM serial console or derived from a
  physical BOOT-button press window** is confirmed in the app. After provisioning, each connection performs
  a challenge-response using a key established during provisioning and stored in NVS.
- One trusted controller initially; trust reset by holding BOOT ~10 s erases only BLE trust data.

Until Milestone 11 the development firmware has **no BLE and no remote input at all**; the first BLE
milestones will be explicitly marked insecure/dev-only in README and firmware logs.

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
