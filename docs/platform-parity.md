# Platform parity: Windows, Linux, macOS

The goal is one kvm-it: the same flows, the same words and the same safety on every controller OS, held to the
[UX principles](ux.md). This page lists every place the app behaves differently per platform, says whether that difference is
the contract working or a gap, and how we know. It is a living page: update the row, and its evidence, whenever platform code
changes. First written in the cross-platform audit of 2026-10-19 (review round 36 in [dev-process.md](dev-process.md)).

## Evidence key

Labels are as strict as the README's. Nothing below is upgraded because it "should" work.

| Label | Meaning |
|---|---|
| **run** | Run on that OS by the audit, 2026-10-19 (Windows 11 only: the audit had no Linux or Mac machine) |
| **hw 10-08** | The maintainer's hardware pass on 0.4.6, 2026-10-08 (bare-metal Windows and a real Mac); not re-run since |
| **hw Linux** | Hardware-verified on Linux on the date the README status table gives (0.1.0-0.3.0); not re-run since |
| **test (CI)** | A unit test covers it and CI runs that test on that OS (Linux in the container, macOS on `macos-14`) |
| **test (local)** | A unit test covers it, but CI never runs the tests on that OS (Windows: run locally by the audit) |
| **static** | Read in the source (and, where named, the dependency's source); never run on that OS |

Contract: ✅ matches the UX contract · ⚠️ works, but differs from the other platforms in a way the user notices · ❌ a gap.

## What CI actually checks per OS

| | Linux | Windows | macOS |
|---|---|---|---|
| Unit tests | `rs.sh test` (Ubuntu container) | **never run** (cross-built only) | `cargo test` on `macos-14` |
| Clippy | `rs.sh clippy` | **never run** on the Windows target | **not in CI**: `rs.sh macos` exists but only runs by hand |
| Build | `rs.sh build` locally; AppImage on packaging PRs and releases | `rs.sh windows` (cross, `x86_64-pc-windows-gnu`) | `.dmg` on every push to main |
| Package smoke test | AppImage: `kvmit --version`, firmware and licence files | release workflow: signatures, PE version resource | `.dmg`: `kvmit --version`, `hdiutil verify`, licence files |
| GUI started | never | never | never |

The audit ran the whole workspace's tests natively on Windows 11 for the first time (214 pass, 1 ignored), and clippy for the
Windows and macOS targets. That found a Windows-only settings race (below) and a lint error in Windows-only code. Neither was
visible to CI.

## The matrix

### Install, start, files

| Flow | Windows | Linux | macOS | Contract | Evidence |
|---|---|---|---|---|---|
| Package | Signed `.msi` (Start menu entry, optional PATH) and `.zip` | `.AppImage`, checksum only, not signed | `.dmg`, ad-hoc signed, **not notarized**: first launch needs *Privacy & Security > Open Anyway* | ⚠️ trust prompts differ by signing (decision 1 below) | Win: hw 10-08; Linux: CI smoke test; mac: CI smoke test, hw 10-08 |
| Command line | `kvmit` on PATH (MSI) or beside `kvmit-gui.exe` | `./kvm-it-X.Y.Z-x86_64.AppImage cli <args>` | `/Applications/kvm-it.app/Contents/MacOS/kvmit` | ⚠️ three spellings; documented in getting-started since round 36 | static (installer, `AppRun`, `build-dmg.sh`) |
| App cannot start (e.g. no OpenGL) | Error dialog with an OpenGL hint | Text on stderr only, invisible when started from a launcher | Text on stderr only, invisible when started from Finder | ❌ Linux/macOS fail silently from a launcher (decision 2) | static (`gui_main.rs`) |
| Settings file | `%APPDATA%\kvm-it\config.json` | `~/.config/kvm-it/config.json` | `~/Library/Application Support/kvm-it/config.json` | ✅ same content, OS-native place | static (`dirs` 6) |
| Two processes saving settings at once | **Fixed in round 36**: a read during another process's save failed ("Access is denied", or briefly "not found") and wrote defaults over everything | Atomic rename; never failed | Atomic rename; never failed | ✅ after the fix | Win: **run** (4/60 failures before, 0/120 after); Linux: test (CI); mac: test (CI) |
| Scripts folder | `Documents\kvm-it\scripts` | `~/Documents/kvm-it/scripts`; **fixed in round 36** for systems without `xdg-user-dirs` (was relative to the working directory) | `~/Documents/kvm-it/scripts` | ✅ | test (CI) for the fallback; folders static |
| Recordings folder | `Videos\kvm-it` | XDG Videos or `~/kvm-it` | `~/Movies/kvm-it` | ✅ OS-native | static |
| Autostart, tray / menu-bar icon | none | none | none (default app menu only) | ✅ consistently absent | static |
| `KVMIT_DEBUG=1` diagnostics | stderr: visible only via `kvmit gui` in a console (`kvmit-gui.exe` has no console) | stderr of the terminal | stderr: only when started from a terminal | ⚠️ how to collect them differs; not documented per OS | static |

### Bluetooth link

| Flow | Windows | Linux | macOS | Contract | Evidence |
|---|---|---|---|---|---|
| Scan | WinRT via btleplug, up to 15 s (stops ~1 s after the first hit) | BlueZ discovery, 5 s | CoreBluetooth via btleplug, up to 15 s | ✅ | Win: hw 10-08; Linux: hw Linux; mac: hw 10-08 |
| Adapter id (`--device`) | Bluetooth address | Bluetooth address | CoreBluetooth UUID, **different on every Mac** | ⚠️ the multi-adapter examples show addresses only | static (`backend.rs`) |
| Pair | WinRT custom pairing, no system dialog | BlueZ agent, no dialog | Pairs on first encrypted write; macOS may show its own prompt | ✅ physical pairing window on all | Win: VM + hw 10-08; Linux: hw Linux; mac: hw 10-08 |
| Unpair / forget | `kvmit unpair` | `kvmit unpair` | Not possible from an app: the error says to use *System Settings > Bluetooth* | ⚠️ OS limitation, explained | static |
| "Bluetooth is off / blocked / missing" | Off, no adapter | rfkill soft/hard, no adapter, no `bluetoothd` | Off, no adapter | ✅ plain language on all | Win: no-adapter case run on hardware (0.4.8); Linux, mac: test (CI) only |
| Short connection interval for smooth mouse | Requested (Windows 11 API) | OS default | OS default | ⚠️ pointer smoothness on Linux/macOS is unmeasured | Win: VM-verified; others: static |
| Permission prompt | none | none (BlueZ over D-Bus) | Bluetooth (`NSBluetoothAlwaysUsageDescription`) | ✅ | mac: hw 10-08 |

### Video and recording

| Flow | Windows | Linux | macOS | Contract | Evidence |
|---|---|---|---|---|---|
| Capture backend | Media Foundation (MJPEG; YUY2 untested) | V4L2 (MJPEG, YUYV) | AVFoundation (macOS decodes) | ✅ | Win: VM + hw 10-08; Linux: hw Linux; mac: hw 10-08 |
| Card key for `--video` | Device path (stable per port) | `card @ bus` (stable per USB port) | Unique ID | ✅ | static |
| No permission to use the card | Error names the Windows camera privacy switch | **An inaccessible `/dev/videoN` is silently left out of the list**: the user sees "no card" | Prompts, then names the Camera privacy setting | ❌ Linux (decision 3) | static (`device.rs` skips nodes it cannot open) |
| Recording needs ffmpeg | On PATH | On PATH | On PATH, **or Homebrew/MacPorts since round 36** (a Finder-launched app never sees the shell's PATH) | ✅ after the fix; no install hint per OS yet | test (CI, macOS logic tested on all) |
| Recording sound | dshow devices | `pactl` sources | AVFoundation devices by index | ✅ | host-tested only on all three (issue #32) |

### Driving the target

| Flow | Windows | Linux | macOS | Contract | Evidence |
|---|---|---|---|---|---|
| Release chord | Ctrl+Alt+Esc | Ctrl+Alt+Esc | Same keys, labelled **Ctrl+Option+Esc** in the chip and (since round 36) every doc | ✅ | test (local + CI); mac chord hw 10-08 |
| Keys the OS keeps (Win/Cmd, Alt+Tab, Cmd+Tab...) go to the target | Yes: low-level hook in a helper process | **No**: no grab exists, so the desktop keeps Super, Alt+Tab, Ctrl+Alt+F-keys | Yes, once kvm-it has the Accessibility permission | ⚠️ Linux by design; the Keys menu is the way | Win: VM-verified, hw 10-08; mac: hw 10-08; Linux: static |
| Capture without a grab explains itself | **Since round 36**: if the helper cannot start | **Since round 36**: always, once per run | Yes (Accessibility hint) | ✅ after the fix | test (all wording tested on every OS) |
| A grab that stops mid-capture | Ends capture, notice names Windows | n/a | Ends capture; notice **named Windows** until round 36 | ✅ after the fix | test |
| Super/Win as a modifier without the grab | Not forwarded (egui reports no Super on Windows/Linux) | **Not forwarded**: Super+key combos cannot be typed | Cmd forwarded as GUI | ⚠️ Linux and grab-less Windows: use Keys (decision 4) | static (egui-winit modifier mapping) |
| Pointer kept in the window while captured | Clipped to the window | **Fixed in round 36 for X11**: only "locked" was asked for, which X11 refuses, so the hidden pointer could leave and a click elsewhere ended capture. Wayland: locked | Locked | ✅ after the fix, unverified on Linux | static (winit 0.30.13 sources) + test of the request order |
| Pointer speed on the target | Raw-input deltas | X11 raw / Wayland relative-pointer deltas | macOS deltas | ⚠️ unmeasured; may feel different per OS | static |
| Touchpad scrolling | Pixel deltas accumulated into notches | same | same | ✅ logic shared | test (CI); hardware issue #34 |
| Physical key layout | Scan codes | egui physical keys | `kVK` codes; **ISO keyboards swap two keys** | ⚠️ known mac gap | test; ISO unverified |
| Ctrl+Alt+Del, Win+L | Keys menu (Windows never lets a program intercept them) | Keys menu | Keys menu | ✅ | hw 10-08 |

### Window and appearance

| Flow | Windows | Linux | macOS | Contract | Evidence |
|---|---|---|---|---|---|
| Smallest window | **Since round 36**: 640x400 points minimum; the bar wraps by whole buttons; long notices wrap | same code | same code | ✅ | Win: **run** (screenshots at 150 %, 1600x1000 to 960x600 px); Linux/mac: not looked at |
| Glyphs | Bundled egui fonts, no system fonts. **Since round 36** no character renders as a box (Record dot and dismiss cross did) | same | same | ✅ | test (every OS) + **run** on Windows |
| HiDPI | Follows the monitor's scale | Follows the scale winit reports (X11: `Xft.dpi`; Wayland: per output) | Retina (`NSHighResolutionCapable`) | ✅ expected | Win: **run** at 150 %; others: static |
| Theme "auto" | Follows the system | Follows the system where winit can read it (Wayland/portal); otherwise the default | Follows the system | ⚠️ Linux may not follow | static |
| Screen readers / UI Automation | UIA tree (AccessKit) | AT-SPI via AccessKit | VoiceOver via AccessKit | ✅ expected | Win: UIA used by the screenshot harness; others: static |

### Flashing the adapter

| Flow | Windows | Linux | macOS | Contract | Evidence |
|---|---|---|---|---|---|
| Port shown | `COMn` | `/dev/ttyACMn` | `/dev/cu.*` (the `tty.*` twin hidden, since it blocks on open) | ✅ | Win: hw 10-08; Linux: hw Linux; mac: hw 10-08 |
| Permission to open it | none needed | `dialout` group (`uucp` on Arch). **Since round 36** the error says so, and getting-started says so up front | none needed | ✅ after the fix | test (all wordings) |
| Port held by another program | "Access is denied" plus, since round 36, what to do | busy plus what to do | busy plus what to do | ✅ after the fix | test |
| "Adapter's own USB port is plugged in" refusal | yes | yes | yes | ✅ never loosened | test (CI) + hw on all three |

## Decisions the matrix leaves open

1. **macOS notarization and Linux signing.** First launch differs most here. Notarization needs an Apple Developer account.
2. **A visible start-up error on Linux and macOS.** Windows shows a dialog; elsewhere a launcher-started app that fails says
   nothing. Options: a native dialog (`osascript`/`zenity` fallback, or a small dependency), or a log file.
3. **Linux capture cards the user may not open.** Today they are silently left out of the list. Listing them with "no
   permission (join `video`)" changes the `list_devices` contract on every platform, so it was not done in this audit.
4. **Super+key on Linux.** Without an OS grab the GUI never sees Super as a modifier. A Linux grab (an X11 keyboard grab;
   Wayland's keyboard-shortcuts-inhibit protocol) would bring parity but is new platform code that needs a Linux desktop to
   test.

## Things to re-verify (dated, STD-003 rule 4)

Everything here was changed or judged on 2026-10-19 without the OS in question. Run these before calling a row verified.

- **2026-10-19, Linux X11 and Wayland:** while captured, the pointer stays inside the window (move it hard toward an edge, click
  outside where the window was): X11 should stay confined, Wayland locked. Nothing else on the desktop should get the click.
- **2026-10-19, Linux:** the first capture shows the "desktop keeps Super, Alt+Tab" notice once; the notice wraps and its
  dismiss cross is inside the window at the minimum window size.
- **2026-10-19, Linux:** flash with the user *not* in `dialout`: the error names `dialout`; then with a serial monitor holding
  the port: the error says another program has it.
- **2026-10-19, Linux without `xdg-user-dirs`** (or with `~/.config/user-dirs.dirs` removed): Scripts uses `~/Documents/kvm-it/scripts`.
- **2026-10-19, macOS:** with ffmpeg from Homebrew and the app started from Finder, *Record…* lists sound inputs and records.
- **2026-10-19, macOS:** with Accessibility denied, the first capture shows the Accessibility notice, wrapped, dismissable; with
  it allowed, a grab that macOS turns off ends capture with a notice naming **macOS**.
- **2026-10-19, macOS and Linux:** the window at its minimum size (640x400 points) and at 1280x800: no clipped chip or button,
  the Record dot and dismiss cross drawn (not boxes). Compare with the Windows screenshots described in round 36.
- **2026-10-19, Windows (bare metal):** two `kvmit run` processes plus the GUI changing the theme, for a minute: no setting
  lost. (The race itself was reproduced and fixed by the unit test, not with real processes.)
- **Still open from earlier rounds**, unchanged by this audit: the Linux GUI since the 0.2.0 redesign, the Linux flasher wizard,
  AVFoundation audio, ISO Apple keyboards, the Windows grab on non-US layouts, pointer feel per OS.
