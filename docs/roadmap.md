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
| 13 | Linux release build | **v0.1.0**: `scripts/rs.sh build`; no packaging yet |
| 14 | Windows controller app | **v0.2.0**, in progress on `windows-0.2.0`: native pairing, scan, Media Foundation capture, the GUI and a BLE link fix have run in a Windows 11 VM (adapter and capture card passed through over USB); keyboard grab in progress; **not yet verified on bare-metal Windows** (Windows as a *target* already works: it is plain USB HID) |

## Planned releases

Direction, not commitments; each release is cut with its own notes and only claims what was verified.

| Release | Theme | Notes |
|---|---|---|
| **0.2.0** | Windows controller | See row 14. Also: top-bar GUI, native keyboard grab on Windows. |
| **0.2.1** | Patch | Video device switching fixed (+ Rescan), logo, Linux AppImage. No firmware or protocol change. |
| **0.3.0** | Flash the adapter from the app | A technician plugs the board's COM port into the controller, presses Flash, then moves the board to the target. **Shipped in 0.3.0** (`kvmit flash` and the GUI wizard): the firmware images ship with the app (`firmware/release/`) and are written with the `espflash` library (Rust >= 1.95). The app refuses to flash while an adapter's native USB port is plugged into the computer, keeps the pairing unless a full erase is confirmed, and checks the chip and flash size first. Later: pull firmware for a newer release, flash several boards in one go (0.6.0 multi-probe). |
| **0.4.0** (candidate) | Network boot through the adapter (iPXE) | The adapter also presents a **read-only USB drive carrying an iPXE image**, so a target can boot from the network and reach WinPE, an installer ISO, a rescue image or anything else a boot server offers. There is no on-board image storage beyond that image and no SD/TF slot: *virtual-media ISO mounting is intentionally not planned.* Design questions to settle first, in this order: (1) can the ESP32-S3 expose read-only mass storage next to HID without disturbing BIOS/UEFI keyboard enumeration (some firmwares dislike composite devices); (2) one image that boots both legacy BIOS and UEFI, or two; (3) **Secure Boot is validated through the 0.4.x minors, and the 0.4.0 notes say exactly which cases were run.** iPXE is not signed by Microsoft, so a stock iPXE is refused by a Secure Boot target; the candidate paths to evaluate are a signed shim chaining a signed iPXE (with the one-time key enrolment driven through kvm-it itself, since it can drive the firmware's own screens), a path that needs no enrolment, and whatever the test matrix shows works in practice. Functional Secure Boot is the goal and will be tested heavily; it is not claimed before it has been. The matrix, with Secure Boot **on**: an OVMF virtual machine (automatable, and we already have that rig), a stock Windows 11 laptop in its default configuration, and at least one Linux-friendly UEFI board with and without the Microsoft third-party CA enabled; nothing is claimed beyond the cases actually run; (4) licensing: iPXE is GPLv2 (with UEFI additions), so it ships as a separate image with its own notice and source offer, like the `serialport` notice; (5) configuration: iPXE can load an `autoexec.ipxe` from the volume it booted from, so the app could write the boot URL there and flash the drive partition with the flasher it already has (to be confirmed). The flasher's settings check already allows this layout change as long as the settings (NVS) partition does not move, so existing adapters upgrade in place and keep their pairing. |
| **0.5.0** (candidate) | Wired link over COM | Drive the adapter over its COM (UART) port with no Bluetooth: no BT passthrough in VMs, headless boxes work, lower latency, and it is the same cable the flasher uses. The client already talks over abstract byte-frame channels, so the desktop side is a serial transport plus framing; the firmware needs a second transport into the same dispatcher and the log console moved off UART0 (or wrapped). Open questions to settle first: opening the port must not toggle DTR/RTS (they are wired to the chip's reset and boot pins, so a careless open reboots the adapter and the target sees its USB device vanish); and a wire link has no pairing, so physical access to the cable is the trust, which means off by default, enabled deliberately and shown on the LED. Natural fit for multi-probe (0.6.0). |
| **0.6.0** | Multi-probe | One running app attaches several adapters and capture cards, associates each adapter with its capture card, switches between the pairs and coordinates script runs across them (an IP-KVM-style fleet view). Builds on the multi-controller/multi-device groundwork noted below. |
| **Later** | Phone and Apple-silicon controllers | A kvm-it **Android and iPhone/iPad app** (plus a macOS build for Apple silicon): the phone becomes the controller, over the same Bluetooth LE link, with the target's picture from a USB capture card where the platform allows it (Android can read a UVC card over USB-C OTG; iOS/iPadOS external-camera support is a question to settle, not an assumption). The Bluetooth client, protocol and script engine are already separate crates, so the plan would be to share them with the mobile apps and redo only the interface. It is a big piece of work, deliberately after multi-probe, and nothing about it is started. |

## Small lifts (ride along with a release, or land as 0.3.x)

Cheap, user-visible or debt-paying, none of them a theme on its own:

- **0.3.1 review follow-ups:** disable *Pair & connect* / *Connect* while a pairing runs; disconnect on early errors in `backend::connect`; a timeout on the pairing guard; the Codex round that was owed after round 13.
- **A switch to hide the boot drive** (an NVS flag with a toggle in the Adapter popup and a reboot), for targets whose firmware dislikes the composite device or boots USB first.
- **Windows polish:** embed the icon and a proper version resource in `kvmit.exe` / `kvmit-gui.exe` and the MSI (today only the window icon is set, and the version is stamped via the window title).
- **A kinder "no Bluetooth adapter found" on Linux** (BlueZ not running, adapter blocked, adapter owned by a VM).
- **Flasher touches:** say "now unplug COM and plug USB into the target" when it finishes; flash several boards in one go (rides with 0.6.0).
- **Generated licence listing** of every dependency, next to `THIRD_PARTY_NOTICES.md` (promised there).
- **Linux video:** notice a vanished card and bound the capture shutdown (the V4L2 backend keeps showing its last frame and can wait forever on a stalled card).
- **Input ordering:** one ordered stream for keyboard and mouse so modifier+click can never reorder (review round 4, finding 6); end-to-end motion backpressure (finding 8); Enter vs keypad Enter on held-key seeding (round 5, finding 4).
- **Verification debts, not features:** a Linux hardware pass (GUI wizard, pairing with the new connection code, the shared motion change); the adapter's release-everything-if-the-app-dies behaviour on hardware; the 5-byte mouse report under real BIOS boot protocol (relevant to the iPXE work); Windows 10, non-US layouts and IMEs; a factory-fresh board through the full-erase flash path.

## Backlog / known gaps

- v0.2.0 (Windows controller): bare-metal verification, the OS-level keyboard grab (Win, Alt+Tab, ... go to the
  target; Ctrl+Alt+Esc releases), YUY2 capture path, Linux hardware regression run for the motion-frame cap.
- GUI video upload is a full-frame texture copy per frame; 1080p60 cost unmeasured.
- Only the US layout.
- Privacy-enabled (RPA) controllers are handled by identity-address lookup in the bond store; untested.
- Packaging (AppImage/MSI), signed releases, firmware OTA.

- HID idle-rate (SET_IDLE) retransmission for the keyboard (astra review #5, deferred).
- Host-testable seam around `usb_hid.c` state/rollback/pending-release logic (currently hardware-only).
- Real VID/PID allocation before public hardware distribution.
- Firmware update path (not a launch blocker): version is reported from day one. Candidates in order of
  preference: USB DFU/serial via the COM port (works today with `scripts/fw.sh flash`), BLE OTA with
  signed images + `ota_0/ota_1` partition layout (needs a custom partition table), then optional USB DFU.
- Boot-compatible 4-byte mouse report fallback if BIOS testing shows it is needed.
- PSRAM enablement, if ever needed.
- Multi-controller trust and multi-device UI (architecture avoids singletons; not built; see 0.6.0 above).
- Windows Secure Attention Sequence (Ctrl+Alt+Del): the macro will send it as an ordinary USB HID chord.
  Whether a given Windows/secure-desktop environment honours it is unverified and will be documented per
  environment once tested on real hardware.
