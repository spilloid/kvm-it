# Changelog

## [Unreleased]

### Added
- `kvmit flash` (in progress for 0.3.0): writes the adapter's firmware through its UART (COM) port with the `espflash` library.
  It validates the image first, refuses the board's native USB port (which would type into the flashing computer), keeps the
  pairing and settings unless `--erase-all` is given, and asks for confirmation. Host-tested (image and port rules);
  **hardware-verified once** on one board over its CH343 UART. Before writing it re-checks that the chosen port is still the same
  device, that no adapter's own USB port (a keyboard and mouse, which is HID-only and not a serial port) is plugged into the
  computer, that the chip is an ESP32-S3 with the image's flash size, and that the installed partition table keeps the settings
  where they are; it validates the image (headers, partition table, file paths inside the firmware folder). Hardware
  results: on Linux with a physical board the pairing survives a default flash and is wiped by `--erase-all`, and the native-USB
  refusal works (also with `--any-port`); on Windows 11 (VM, the board's COM bridge passed through) the CLI and the GUI wizard
  flash end to end. An interrupted flash (the Windows flasher killed halfway through the app) was recovered by flashing again, no BOOT button. Not exercised: the Linux GUI wizard, bare-metal Windows.
- **Flash adapter…** in the GUI's Adapter popup (`flashwiz.rs`): a window that finds the board's COM port, checks the firmware
  folder, blocks while any Espressif native USB port is plugged into the computer, requires a second tick before erasing the
  pairing, refuses while a script runs or the adapter is connected, flashes on a worker thread with a progress bar (no close button
  and no app exit while writing), and says how to recover if it fails. Its rules are unit-tested; a flash through the window was run end to end in the
  Windows 11 VM. Opening it cancels a pending connection attempt (which would otherwise block it with nothing to press).
- The adapter firmware ships with the app: `firmware/release/` (the exact images, with provenance and checksums) is installed
  beside the program by the MSI, included in the zip and the AppImage, and found by `kvmit flash` and **Flash adapter…** with no
  setup (`KVMIT_FIRMWARE` or `--firmware` override it). CI checks that the bundled images are not older than the firmware sources;
  `verify-release.py` checks the zip carries them. Windows executables grew from ~21 MB to ~35 MB with the flasher (espflash).
- `THIRD_PARTY_NOTICES.md` (MPL-2.0 notice for `serialport`) ships in the zip, the MSI and the AppImage; the AppImage builder moved to Rust 1.99.

### Changed
- Build toolchain is Rust 1.99 (was 1.90), needed for the in-app flasher planned for 0.3.0; clippy lints fixed (`as_chunks`, an
  always-true `min` in a test, explicit `f32` for stroke widths). No behaviour change. The AppImage builder stays on 1.90 for now.

## [0.2.1] - 2026-10-04 (video switching, logo, Linux AppImage)

Patch release on 0.2.0; firmware (0.1.0) and wire protocol (v1) are unchanged. Verification: the video-switch fix was
VM-verified (Windows 11 VM, real capture card) before the round-6 review fixes; after them it is host-tested only (the
reopen click has no test). Nothing new was run on a Linux board or the Linux GUI; the AppImage was built and `cli --version`
run, the GUI not started from it. See the README status table.

### Fixed
- Switching video devices in the GUI did nothing: the device dropdown was a second popup inside the Video popup, so opening it
  counted as a click outside and closed the Video popup before anything was chosen. Devices are now rows in the popup (one click
  switches), and a **Rescan** button re-reads the device list (it was only read at startup, so a card plugged in later never appeared).
  Clicking the open device reopens it (a stalled card no longer needs an app restart), and the old capture is closed outside the
  shared video lock so a card that will not stop cannot freeze scripts' `screen()`.

### Added
- A synthetic demo video source for documentation screenshots and tests: with `KVMIT_DEMO_VIDEO=<picture.png>` the GUI lists a
  "Demo target (synthetic picture)" device that shows that picture, so screenshots never need a real machine's screen. It is opt-in at open time too (a remembered `demo:` path does nothing
  without the variable, and is never saved as the preferred card), and refuses non-regular files and pictures over 8192 px a side.
- The kvm-it logo: the GUI's window and taskbar icon, the site's favicon and Apple touch icon, and the README and site headers. The
  small icons put the mark on a light tile so the navy monitor does not vanish on dark taskbars and browser tabs.
- Linux AppImage packaging (`scripts/build-appimage.sh`, `installer/linux/`): built in an Ubuntu 22.04 container so it runs on
  glibc 2.35 and newer, with the AppImage tool and its embedded runtime pinned by checksum, the glibc floor enforced (and a failed inspection is an error), and
  the host libraries it relies on named in the README. Not bit-for-bit reproducible (builder apt packages float). Published with 0.2.0 after the tag (asset `kvm-it-0.2.0-x86_64.AppImage`).

## [0.2.0] - 2026-10-04 (Windows controller)

The controller app now runs on Windows. Firmware and wire protocol are unchanged (firmware 0.1.0, protocol v1).

Verification: see the README status table. Everything Windows was run in a **Windows 11 virtual machine** on a Linux
host with the Bluetooth adapter and the HDMI capture card passed through over USB (real radio, real card); nothing
here is verified on bare-metal Windows. The shared client change (mouse motion) was not re-run on a Linux board.
The code was adversarially reviewed in two rounds (docs/dev-process.md, round 4: 16 findings, two deferred).

### Added
- **Windows controller**: `kvmit.exe` (CLI) and `kvmit-gui.exe` (GUI, no console window; a failed start shows a
  dialog explaining it, e.g. when the graphics driver lacks OpenGL 2.0).
- **Windows pairing with no system dialog**: `kvmit pair` / `unpair` and the GUI's *Pair & connect* use the WinRT
  custom-pairing API and accept only the Just Works ceremony. The adapter's physical pairing window is still required.
- **Windows video**: a Media Foundation backend on the card's native MJPEG/YUY2 modes (same decode path as Linux).
- **Keyboard grab on Windows**: while input is captured, Win, Alt+Tab, Ctrl+Esc, Alt+F4 and the other keys the OS
  would keep go to the target and not to the controller. Ctrl+Alt+Esc is the only chord that gives the keyboard
  back, and is never forwarded. Ctrl+Alt+Del and Win+L cannot be intercepted by any program: use the *Keys* menu.
  It runs in a small helper process (the app starts itself with a hidden flag) that stops swallowing if the GUI
  stops responding for 3 s, handles the release chord itself, and passes keys that were already held before
  capture through to the controller.
- **Accessibility tree**: the GUI exposes its controls to screen readers and UI Automation (named buttons).
- Release packaging for Windows: `scripts/build-release.ps1` (signed-if-configured exes, MSI, zip, SHA-256 files),
  `scripts/sign.ps1`, `scripts/verify-release.py`, `installer/kvmit.wxs`, and a release runbook (docs/RELEASING.md).
- `examples/linkstress` (acked-request latency and failures under mouse-motion and video load) and
  `examples/hookcheck` (keyboard grab on real Windows; prints counts only, never keys).

### Changed
- **GUI layout**: the left sidebar is gone and the preview fills the window. Adapter, Target USB, Video and Input
  are colour-coded status chips (green working, amber in progress, red broken, grey idle); Adapter, Video and Input
  open the controls that used to be in the sidebar, and *Keys*, *Type* and *Scripts* are buttons with popups. A
  running script's log moves to a bottom strip that stays visible while it runs.
- **Scanning** returns as soon as the adapter is heard and waits up to 15 s by default on Windows (a 5 s scan found
  it in about 2 of 5 runs there).
- **Mouse motion** is sent in frames of at most 127 units per axis with a bounded backlog, and a click can no longer
  overtake the movement before it (the firmware turns each motion frame into one USB report per 127 units inside its
  Bluetooth handler, so huge frames stalled the link). This changes Linux behaviour too.
- A failed *release all keys* at the end of capture now closes the session, so the adapter's own link-drop release
  takes over instead of keepalives holding a stuck key.

### Fixed
- On Windows the Bluetooth link collapsed under ordinary mouse movement (above about 30-60 motion frames per
  second): Windows' default 60 ms connection interval cannot carry it. The app now requests the fast connection
  mode (15 ms interval; round trip 120 ms to 30 ms in the VM) while connected.
- A vanished capture card (Windows) now stops the capture and says so, instead of leaving a frozen frame that looks
  live; stopping the capture can no longer hang the GUI; COM is initialised and released in balance.
- Windows scancodes for Alt+PrintScreen and Ctrl+Pause are mapped.

### Known limitations
- Modifier+click ordering can break while capturing on Windows if a GUI frame takes longer than the click: keyboard
  and mouse buttons reach the app through two paths (planned fix: one ordered stream, review round 4 finding 6).
- The end-to-end transport queue for mouse motion is not bounded; only the work per frame is (finding 8).
- Keys held across the instant capture begins are handled by virtual key, so an alias pair that shares one (Enter and
  keypad Enter) can be misattributed once (review round 5, finding 4).
- Windows 10, non-US keyboard layouts and IMEs, and bare-metal Windows are untested. The Linux GUI has not been
  re-checked since the redesign. The V4L2 (Linux) backend still shows its last frame if the card vanishes.
- Releases may be unsigned; the release notes say so. An unsigned build makes Windows SmartScreen warn.

## [0.1.0] - 2026-10-03 (MVP)

Verification: see the README status table. Short version: USB HID, BLE pairing and the GATT link
(`kvmit pair`/`status` on Linux) and HDMI capture (one MacroSilicon card, 1080p) are hardware-verified. Typing
over BLE (`kvmit type`/`key` from Linux into a Windows 11 target, all printable US-ASCII) is hardware-verified.
The GUI on Linux is maintainer-checked (including the round-3d/3e input-ownership changes). The LED, BOOT gestures (pairing window, 10 s trust reset) and reconnect after replug
are hardware-verified. The Windows controller app is out of scope for 0.1.0 (cross-compiles only) and is planned for 0.2.0.

### Added
- GUI: "Send keys" panel (Ctrl+Alt+Del, Win, Alt+Tab, PrintScreen, ...), F13-F24 mapping, and input capture without a video signal.
- Docs site (GitHub Pages) and a product README.
- Wire protocol v1 (`protocol/SPEC.md`), generated golden vectors, Rust (`kvmit-protocol`) and C (`proto_frame`)
  codecs tested against the same vectors.
- Firmware: NimBLE peripheral with LE Secure Connections bonding gated by a physical pairing window,
  protocol dispatcher (handshake gate, ack dedup, keepalive release, link-drop release), status LED language,
  BOOT button gestures (short = pairing window, 10 s = erase trust), persisted device name.
- Desktop (`kvmit`): CLI and egui GUI; BLE client with retry/keepalive/motion accumulation; US layout;
  script engine (TOML format, variables, secrets, `wait_for` on the screen, confirm, dry-run, preview,
  DuckyScript import); V4L2 capture; built-in example scripts; Windows cross-compile.
- `scripts/rs.sh` (containerised Rust build/test/clippy/Windows cross-build).

### Added (CLI)
- `kvmit key --hold <duration> <keys>`: press and hold a key or chord (e.g. `--hold 15s f12` while the target
  boots); Ctrl+C releases early.

### Changed (pairing)
- Every power-on (plug-in or RESET) opens a 15 s pairing window, bonded or not, so a new controller pairs
  without pressing BOOT (`CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S`, 0 disables). With a bond stored, crash/watchdog
  reboots do not open it. The trusted controller reconnecting does not use up the window; a new pairing does.
  BOOT short-press still opens the 300 s window.

### Fixed
- Firmware panicked (LoadProhibited) on every BLE connection whose controller handle was >= 2: ESP-IDF v5.5
  NimBLE indexes per-link arrays (`slave_conn`, `g_max_*`) of `MAX_CONNECTIONS + 1` entries by the raw
  connection handle, and overwrote `ble_gap_update_entries`. `firmware/patches/` now moves that state into
  NimBLE's per-connection struct; `scripts/fw.sh` applies the patch to the pinned ESP-IDF and the build refuses
  an unpatched IDF. NimBLE stays at one connection, so a second controller can never connect.

### Added (diagnostics)
- `scripts/fw.sh build-diag` / `sdkconfig.diag`: radio-diagnostic firmware that logs stored bonds and an RSSI
  survey at boot (see docs/hardware.md, "BLE troubleshooting"). Not part of release builds.

### Changed
- The boot-time HID self-test is now opt-in (`CONFIG_KVMIT_SELFTEST`); the product no longer types at boot.

### Firmware foundations (Milestones 0-2)

- ESP-IDF 5.5 project for ESP32-S3 (YD-ESP32-23, N16R8) with 16 MB flash configuration.
- Native-USB TinyUSB HID: two boot-protocol interfaces (keyboard, mouse with wheel).
- Pure-C HID state machine (duplicate-safe key/button tracking, 6-key rollover refusal, release-all).
- Deterministic boot self-test: 8 s delay, types `HELLO FROM KVM`, Enter, small mouse nudge, releases all.
- Host unit tests; container-based build/flash script.
- Hardened after adversarial review: deadline-based send waits at 1 kHz ticks, state rollback on failed sends,
  retried release-all after suspend, GET_REPORT support, Caps Lock-aware self-test.
- Hardware-verified 2026-10-03: flash, enumeration (`lsusb`), self-test typing.
