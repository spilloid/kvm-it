# Roadmap

Status key: done = built and software-verified; **hw?** = built, awaiting verification on a physical board.

| # | Milestone | Status |
|---|---|---|
| 0 | Repository, build instructions, flash | done — flashed and boots on the real board (2026-10-03) |
| 1 | Enumerates as USB keyboard + mouse | done — hardware-verified via lsusb on a separate host (2026-10-03) |
| 2 | Self-test: `HELLO FROM KVM`, Enter, mouse nudge | done — maintainer-reported working (2026-10-03) |
| 3 | Protocol v1 spec/vectors, codecs | done — host-tested |
| 3b | BLE discovery, bonding, desktop connects | done — hardware-verified on Linux/BlueZ (2026-10-03) |
| 4 | BLE key events → USB keyboard (dispatcher, dedup, keepalive) | done — typing and keepalive release of a held key after `kill -9` hardware-verified into a Windows 11 target |
| 5 | Mouse move/buttons/scroll over BLE | host-tested; **hw?** |
| 6 | Text injection, US layout (desktop-side) | host-tested |
| 7 | UVC capture (V4L2) | done — hardware-verified with an HDMI capture card (MacroSilicon `345f:2109`) |
| 8 | GUI: video + device controls | maintainer-reported working with an adapter and a target |
| 9 | Live keyboard/mouse capture | built; **hw?** |
| 10 | Macros + native script engine | host-tested |
| 10b | Screen-aware `wait_for`, `confirm`, DuckyScript import, dry-run | host-tested on synthetic frames; not on real OOBE |
| 11 | Pairing window, bond, BOOT trust reset | done — hardware-verified (15 s power-on window, BOOT window, 10 s trust reset, LED); residual risk in docs/security.md |
| 12 | Hotplug/reconnect hardening | basic auto-reconnect built; soak testing outstanding |
| 13 | Linux release build | done: AppImage since 0.2.1, built and attached by CI |
| 14 | Windows controller app | done: since 0.2.0; MSI and zip signed in CI; **hardware-verified on bare-metal Windows** (0.4.6) |
| 15 | macOS controller app (Apple silicon) | done: since 0.4.6; **hardware-verified on a real Mac** (0.4.6); ad-hoc signed, not notarized |

## Shipped

| Release | Date | Theme |
|---|---|---|
| 0.1.0 | 2026-10-03 | MVP: Linux controller, adapter firmware, BLE pairing, capture, scripts |
| 0.2.0 / 0.2.1 | 2026-10-04 | Windows controller; video switching, logo, Linux AppImage |
| 0.3.0 | 2026-10-04 | Flash the adapter from the app |
| 0.4.0 | 2026-10-05 | Network boot through the adapter (read-only iPXE drive) |
| 0.4.1-0.4.5 | 2026-10-06 | Honest video state, button contract, input fidelity, session recording, light/dark theme |
| 0.4.6 / 0.4.7 | 2026-10-07/08 | macOS (preview, then hardware-verified); `kvmit run --video` for several adapters at once |

Details for each are in the CHANGELOG; the README status table says what each part has been verified on.

## Road to 1.0

1.0 means: what kvm-it does today, finished. No new themes; every claim backed by the kind of test it names; known limits
written down instead of open-ended. Direction, not commitments; each release only claims what was verified.

| Release | Theme | Contents |
|---|---|---|
| **0.4.8** (shipped 2026-10-08) | Polish and housekeeping | Icon and version information on `kvmit.exe`, `kvmit-gui.exe` and the MSI; plain-language Bluetooth errors (off, blocked, no adapter, no `bluetoothd`); the generated licence listing of every Rust dependency (`THIRD_PARTY_LICENSES.html`) and the firmware's components in `THIRD_PARTY_NOTICES.md`; the Secure Boot boot drive from PR #15 (VM-verified only; real PCs are #40); this roadmap. |
| **0.4.9** | Your own boot server, input ordering, review debt | **Point the boot drive at your own server from the app:** enter a boot URL in the Adapter popup (or `kvmit boot-drive --url`), the app writes it into the drive's `autoexec.ipxe` and reflashes only the drive partition, keeping the pairing; the drive stays safe by default (nothing runs without a key press), and the docs get a worked example with an object-storage bucket behind a custom domain, IP-restricted, since anything on the drive is readable by whoever holds the adapter. One ordered stream for keyboard and mouse so modifier+click can never reorder (review round 4, finding 6); Enter vs keypad Enter on held-key seeding (round 5, finding 4); the pairing UI follow-ups (disable *Pair & connect* / *Connect* while a pairing runs, disconnect on early errors, a timeout on the pairing guard); the cross-model review rounds still owed where Opus stood in for Codex (rounds 13-16). |
| **1.0.0** | Finished | When the checklist below is done. |

### 1.0 checklist

- [ ] Hardware-verify issues **#31-#34** run, or each one written down as a known limit with what was seen.
- [ ] **#40** (Secure Boot on real PCs) run on at least a stock Windows 11 laptop; the docs say exactly which machines worked.
- [ ] A **Linux hardware pass** on the release build: GUI wizard, pairing, the shared motion change (owed since 0.2.0).
- [ ] 0.4.9's input ordering on a real target, including wheel/touchpad (#34).
- [ ] Every STD-001 deviation (a review round run by a stand-in reviewer) followed by the owed cross-model round.
- [ ] README status table, getting-started and the docs site current for all three platforms; no row older than the code it describes.
- [ ] Upgrade check: settings, pairings and saved scripts from 0.4.x still load in 1.0 on each OS.
- [ ] Known limits listed in one place: US layout only, macOS not notarized, Windows Secure Attention Sequence per environment, unmeasured 1080p60 cost, Bluetooth link count per computer.

## After 1.0 (candidates)

| Theme | Notes |
|---|---|
| **Wired link over COM** | Drive the adapter over its COM (UART) port with no Bluetooth: no BT passthrough in VMs, headless boxes work, lower latency, and it is the same cable the flasher uses. The client already talks over abstract byte-frame channels, so the desktop side is a serial transport plus framing; the firmware needs a second transport into the same dispatcher and the log console moved off UART0 (or wrapped). Open questions to settle first: opening the port must not toggle DTR/RTS (they are wired to the chip's reset and boot pins, so a careless open reboots the adapter and the target sees its USB device vanish); and a wire link has no pairing, so physical access to the cable is the trust, which means off by default, enabled deliberately and shown on the LED. Natural fit for multi-probe. |
| **Multi-probe** | One running app attaches several adapters and capture cards, associates each adapter with its capture card, switches between the pairs and coordinates script runs across them (an IP-KVM-style fleet view). Builds on the multi-controller/multi-device groundwork noted below. |
| **Phone and tablet controllers** | A kvm-it **Android and iPhone/iPad app**: the phone becomes the controller, over the same Bluetooth LE link, with the target's picture from a USB capture card where the platform allows it (Android can read a UVC card over USB-C OTG; iOS/iPadOS external-camera support is a question to settle, not an assumption). The Bluetooth client, protocol and script engine are already separate crates, so the plan would be to share them with the mobile apps and redo only the interface. It is a big piece of work, deliberately after multi-probe, and nothing about it is started. |

## Small lifts still open

Cheap, user-visible or debt-paying, none of them a theme on its own (done ones are in the CHANGELOG):

- **Flasher:** flash several boards in one go (rides with multi-probe).
- **Input:** end-to-end motion backpressure (review round 4, finding 8).
- **Verification debts, not features:** the adapter's release-everything-if-the-app-dies behaviour on hardware; the 5-byte mouse report under real BIOS boot protocol (relevant to the boot drive); Windows 10, non-US layouts and IMEs; a factory-fresh board through the full-erase flash path.

## Backlog / known gaps

- Windows controller: the keyboard grab's edge cases on bare metal, the YUY2 capture path, the Linux hardware regression
  run for the motion-frame cap.
- GUI video upload is a full-frame texture copy per frame; 1080p60 cost unmeasured.
- Only the US layout.
- Privacy-enabled (RPA) controllers are handled by identity-address lookup in the bond store; untested.
- Firmware OTA (see the update path below); macOS notarization.

- HID idle-rate (SET_IDLE) retransmission for the keyboard (astra review #5, deferred).
- Host-testable seam around `usb_hid.c` state/rollback/pending-release logic (currently hardware-only).
- Real VID/PID allocation before public hardware distribution.
- Firmware update path (not a launch blocker): version is reported from day one. Candidates in order of
  preference: USB DFU/serial via the COM port (works today with `scripts/fw.sh flash`), BLE OTA with
  signed images + `ota_0/ota_1` partition layout (needs a custom partition table), then optional USB DFU.
- Boot-compatible 4-byte mouse report fallback if BIOS testing shows it is needed.
- PSRAM enablement, if ever needed.
- Multi-controller trust and multi-device UI (architecture avoids singletons; not built; see multi-probe above).
- Windows Secure Attention Sequence (Ctrl+Alt+Del): the macro will send it as an ordinary USB HID chord.
  Whether a given Windows/secure-desktop environment honours it is unverified and will be documented per
  environment once tested on real hardware.
