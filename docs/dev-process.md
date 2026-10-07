# Dev process log

Orchestrator (Claude) writes/verifies; a different model (astra, Codex `gpt-6-astra`) reviews adversarially.
Every finding is reproduced against source before being accepted.

## 2026-10-03 — Milestones 0-2 firmware

- Asked: repository + firmware for ESP32-S3 HID self-test.
- Implemented: HID state machine, ASCII→HID, TinyUSB layer, self-test, container build.
- Verified by running: host tests (ASan/UBSan) pass; firmware builds in `espressif/idf:v5.5`.
- Not verified: anything on hardware.
- Adversarial review: astra (`codex exec -m gpt-6-astra`, high effort, read-only, static; ~47k tokens), 7 findings.
  Adjudicated by the orchestrator against source:

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | `pdMS_TO_TICKS(2)` is 0 at 100 Hz ticks, so the readiness wait is not ~100 ms and Shift+key back-to-back can time out | **Reproduced** (`CONFIG_FREERTOS_HZ=100` in generated sdkconfig). Fixed: 1 kHz ticks + deadline-based wait |
| 2 | High | State mutated before send; a failed send makes the retry a silent no-op, leaving the host key held | **Confirmed by reading code.** Fixed: snapshot/rollback on failed send (keys and buttons) |
| 3 | High | release-all while suspended clears local state, sends nothing, returns OK | **Confirmed.** Fixed: `g_release_pending` + housekeeping task retries until delivered |
| 4 | Med | GET_REPORT always STALLs | **Confirmed.** Fixed: returns current keyboard (8 B) / mouse (5 B) input report |
| 5 | Med | SET_IDLE accepted but idle-rate retransmission not implemented | Accepted as real, **deferred** to roadmap (not needed for M2; hosts normally do their own typematic repeat); hardware check added |
| 6 | Med | Uppercase self-test yields lowercase if target Caps Lock is on | **Confirmed.** Fixed: track Caps Lock from LED output report and invert Shift for letters; also documented as a prerequisite |
| 7 | Low | `xTaskCreate` result ignored for self-test | **Confirmed.** Fixed: logged; USB stays up |

  astra also reported no defect in endpoint addresses/sizes, esp_tinyusb 1.7 config fields, N16R8 flash/PSRAM
  settings, or the task-WDT interaction with the 8 s delay, and confirmed the 5-byte boot mouse report is
  permitted by HID 1.11 Appendix B (so the "least-certain" mouse item in hardware.md is lower risk than first
  stated, but still needs a real BIOS to confirm).
- After fixes: host tests pass; firmware rebuilds clean (no warnings). The rollback, pending-release and
  GET_REPORT paths live in `usb_hid.c` and are **not covered by host tests** (hardware-coupled); they are
  covered only by static review and the hardware checklist.
- Limits of this review: static, no hardware, one model, one pass. Not a substitute for the checklist.

## 2026-10-03 — MVP 0.1.0: BLE link, protocol, desktop app (review round 2)

- Asked: BLE transport, protocol v1, LED/BOOT, desktop CLI/GUI, script engine.
- Adversarial review: astra (`codex exec -m gpt-6-astra`, high effort, read-only, static), 20 findings, all
  accepted and fixed in `7239dd3`. *This table was written after the fact (same day, from the Codex session
  log) because it was omitted when the fixes were committed; the fixes were spot-checked in source while
  writing it (bonded+SC gate, unconditional `keep_only`, connection generations, wall-time watchdog, aged
  dedup, MTU error path, name truncation, `Drop` for the client, zeroize, saturating preview maths, h-scroll).*

| # | Sev | Finding | Verdict / fix |
|---|---|---|---|
| 1 | High | Encryption accepted without bonding; an unbonded peer could type | Accepted. `ENC_CHANGE` requires `sec_state.bonded`, else disconnects |
| 2 | High | Secure Connections enabled but not required (legacy fallback) | Accepted. `CONFIG_BT_NIMBLE_SM_LEGACY=n` |
| 3 | High | Pairing across window expiry kept the old controller's bond | Accepted. `keep_only()` runs on every bonded encryption, window or not |
| 4 | High | Full queue could drop the session-end event, leaving keys held | Accepted. Session start/end are not queue items; worker reconciles a generation counter |
| 5 | High | Queued frames crossed connection boundaries | Accepted. Queue items carry the connection generation; stale ones are dropped |
| 6 | High | Seq wrap replayed a cached ACK instead of releasing a key | Accepted. Dedup entries age out (`PROTO_DEDUP_AGE_MS`) and match seq+type |
| 7 | High | Failed `KEY_TAP` release left the key held | Accepted. Failed up path releases/cleans up before reporting |
| 8 | High | Dropping the last client handle kept keepalives running | Accepted. `Drop for Inner` stops the tasks so the firmware watchdog can release |
| 9 | Med | Invalid traffic starved the keepalive watchdog | Accepted. Watchdog runs on wall time every 250 ms, independent of traffic |
| 10 | Med | Cache eviction let a retry repeat a non-idempotent action | Accepted. Dedup cache retention covers the retry window |
| 11 | Med | Client seq wrap overwrote outstanding requests | Accepted. Occupied sequence numbers are not reused |
| 12 | High | Unrelated ACK could confirm `RELEASE_ALL` | Accepted. Replies are matched on seq **and** type (ERROR on its original type) |
| 13 | Med | HELLO could not complete at the default ATT MTU | Accepted. Preferred MTU 247; below the minimum the device answers `MTU_TOO_SMALL` instead of timing out |
| 14 | Med | A 30-32 byte name disabled advertising | Accepted. Scan-response name truncated to 29 bytes (marked incomplete) |
| 15 | Med | Clicks could overtake accumulated motion | Accepted. Pending motion is flushed before a button request |
| 16 | Med | Secret defaults/literals serialised as plaintext | Accepted. Serialisation redacts secrets |
| 17 | Med | `Debug` exposed secrets and reconstructable keystrokes | Accepted. Redacting `Debug` impls |
| 18 | Med | Secret copies left in ordinary heap allocations | Accepted. `zeroize` on substituted text and rejected characters |
| 19 | Med | Nested repeats overflowed preview counts | Accepted. Saturating arithmetic in preview/duration estimates |
| 20 | Med | Horizontal scroll ACKed but not applied | Accepted. `h` passed through to `usb_hid_mouse_wheel` |

- Limits: static, one model, one pass. Host tests cover the dispatcher paths; BLE paths were unverified on
  hardware at the time.

## 2026-10-03 — BLE bring-up on hardware; NimBLE connect panic; radio diagnostics

- Problem: the dev laptop (Surface Laptop 4, Intel AX201, BlueZ) almost never saw the adapter's adverts; a phone
  saw it at -50 dBm. Desktop pairing and GATT had never run.
- Investigation (run as root on the dev host, board on COM only):
  - `btmon` during an LE scan: the laptop's controller delivered ~50 advertising reports in 25 s from ~10 devices,
    all weak (-83 to -99 dBm), and none from the adapter.
  - New diagnostic firmware (`scripts/fw.sh build-diag`) scanned from the board: it heard the laptop's
    `KVMITDIAG` marker at -50 dBm avg (-39 max), and several -67 to -72 dBm advertisers the laptop had missed.
    So the RF path was fine and the laptop's scanner was not. It also showed the stored bond was a different
    device (`EC:28:D3:33:F9:88`), not this laptop.
  - `btmgmt power off/on` restored the laptop's scanner (1095 reports in 20 s; adapter at -39 dBm). Root cause
    on the laptop side is unknown; `ll-privacy` (BlueZ `KernelExperimental`) was still on afterwards, so disabling it was not needed for the fix.
- Second fault, exposed once the laptop could connect: the adapter panicked (LoadProhibited, EXCVADDR 0x11) in
  `ble_gap_update_next_exp` / `ble_gap_update_entry_find` right after every new connection. Decoded with
  addr2line; `nm` showed `slave_conn` immediately before `ble_gap_update_entries`. ESP-IDF v5.5 NimBLE declares
  `int slave_conn[MYNEWT_VAL(BLE_MAX_CONNECTIONS) + 1]` and writes `slave_conn[conn_handle] = 1`; the S3
  controller handed out handle 2 (logged after the fix), so with `MAX_CONNECTIONS=1` it overwrote the list head.
  First fix: `CONFIG_BT_NIMBLE_MAX_CONNECTIONS=6` plus single-link enforcement in `ble_link.c`; **replaced** after
  review round 3 (below) by patching NimBLE itself and returning to one connection.
- Verified by running (diagnostic build on the board, which contains the fix): `kvmit scan`; `kvmit pair` →
  "paired and connected" (new peer, handle 2, LE SC bond, MTU 247); `kvmit status` → protocol v1.0, firmware
  0.1.0, 90 ms RTT, after a bonded reconnect on handle 2. No panics. `fw.sh test` passes; `fw.sh build` and
  `fw.sh build-diag` build clean.
- Not verified at that point: the final patched build (see round 3); typing over BLE into a target.

### Review round 3 (astra, `gpt-6-astra`, high effort, read-only, static)

**3a: first fix (MAX_CONNECTIONS=6 + app guards) and the diagnostic build.** 3 findings.

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | With 6 host slots, a refused second link could still bond: NimBLE persists keys in `ble_sm_persist_keys()` *before* `ENC_CHANGE`, `ble_gap_terminate()` failures were ignored, and `CONNECT` is delivered late (after remote-feature read), so a BOOT press could restart advertising while a link existed | **Confirmed** against the v5.5 NimBLE source (persist order; `store_write_cb` called under the host lock; delayed `CONNECT`). Fixed in 3b, then superseded (see 3b) |
| 2 | Low | Diagnostic log printed advertiser names verbatim (newline/ANSI injection) | **Confirmed.** Names reduced to printable ASCII |
| 3 | Low | `KVMIT_RADIO_DIAG_SCAN_S` accepted 0/negative/overflowing values | **Confirmed.** Kconfig `range 1 300` |

**3b: app-level trust gate** (a `store_write_cb` wrapper refusing keys unless one link and an open window;
`live_links()` via NimBLE's private `ble_hs_conn_foreach`; handle-bound restart). 6 findings; astra confirmed
3a-2 and 3a-3 fixed.

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | NimBLE ignores store-write failures: a refused peer still gets `ENC_CHANGE` as bonded, `keep_only()` then deletes the real bond; partial `OUR_SEC`/`PEER_SEC` writes possible at window expiry | **Confirmed** (`ble_sm_persist_keys` discards both return values) |
| 2 | High | Counting links does not bind persistence to the admitted link; BOOT-task/host race between the count and `adv_start`; legitimate CCCD writes refused while two links exist | **Confirmed** by reading the code paths |
| 3 | Med | Security completed before the delayed `CONNECT` is discarded (`g_encrypted` reset) | Plausible ordering; **accepted** |
| 4 | Med | Advertising can stay stopped after the last link goes (refused-link `DISCONNECT` ignored; NimBLE suppresses `DISCONNECT` when `CONNECT` was never sent) | **Confirmed** in source |
| 5 | Med | The handle guard misses other raw-handle accesses: `CONNECT` with non-zero status, pre-`CONNECT` disconnect, and the `g_max_*` data-length arrays | **Confirmed**: four more arrays indexed the same way |
| 6 | Low | The gate also refused NimBLE's local-IRK write at boot (no links yet) | **Confirmed** |

**Resolution (all of 3a-1 and 3b):** the app-level approach was abandoned. Both reviews named the structural
alternative, and it removes every one of these orderings at once: `firmware/patches/esp-idf-v5.5-nimble-conn-
handle-index.patch` moves `slave_conn` and `g_max_{tx,rx}_{time,octets}` into `struct ble_hs_conn` (zeroed on
allocation, freed with the link), so no state is indexed by raw handle. `CONFIG_BT_NIMBLE_MAX_CONNECTIONS` is back
to 1: NimBLE refuses to start connectable advertising while its one slot is used (`ble_hs_conn_can_alloc()` →
`BLE_HS_ENOMEM`), so no second link can exist and the store gate, link counting, handle-bound restart and private
API declarations were all removed from `ble_link.c`. `scripts/fw.sh` applies `firmware/patches/*.patch` in every
container run; `firmware/CMakeLists.txt` fails configuration if the IDF is unpatched (checked by configuring
against a clean image). `nm` confirms neither ELF contains the old arrays. Kept from the earlier fix: the handle
in the connect log line and a logged `ble_gap_terminate()` failure.

**3c: final firmware diff (NimBLE patch, MAX_CONNECTIONS=1) plus the GUI "Send keys" change.** astra found the
patch correct and complete (all 11 hunks match esp-nimble `cc3ac541`; no other consumers of the removed symbols in
348 host/port files; zero-init and locking sound; `fw.sh` wrapper sound) and raised 7 findings, mostly older
lifecycle issues the earlier rounds had not reached:

| # | Sev | Finding | Verdict / fix |
|---|---|---|---|
| 1 | High | NimBLE accepts SMP bonding as soon as a link exists, before the delayed `CONNECT` runs our window check, so a peer could bond with the window closed and `keep_only()` would then drop the real bond | **Accepted** (predates today). `ble_hs_cfg.sm_bonding` is now 1 only while the window is open (`set_window()`); outside it no keys are persisted and the existing "peer did not bond" path disconnects. Bonded re-encryption is unaffected (hardware-checked) |
| 2 | Med | A delayed `CONNECT` reset `g_encrypted` after encryption was already up | **Accepted.** `CONNECT` adopts `sec_state.encrypted && bonded` and only requests security if not encrypted |
| 3 | Med | A link lost before `CONNECT` gets no `DISCONNECT` (NimBLE suppresses it), so advertising stayed stopped | **Accepted.** The worker's 250 ms tick restarts advertising when idle and it should be advertising; `adv_start` treats `EALREADY`/`ENOMEM` as expected |
| 4 | Med | GUI: captured Enter/Space could activate a focused "Send keys" button | **Accepted.** Chord buttons disabled while capturing; focus surrendered when capture begins |
| 5 | Med | GUI: chord key-up errors ignored, so a key could stay held | **Accepted.** Any failure sends `RELEASE_ALL` and shows a notice |
| 6 | Med | GUI: chords could interleave with scripts, Type, or capture release | **Accepted.** `chord_busy` serialises them: chords disabled while a script/capture/chord runs; `start_run` and `begin_capture` refuse while a chord is in flight |
| 7 | Low | The CMake guard accepted a half-applied patch | **Accepted.** It also fails if `ble_gap.c` still contains the raw arrays |

Also from 3c: the patch now sets `data_len_chg.conn_handle` (upstream left it 0), and docs no longer claim "no
second link on any path" beyond the configuration used (extended advertising is disabled; its start path does not
recheck capacity).

**3d: verification of the 3c fixes** (run after a reviewer usage-limit delay). Confirmed sound: NimBLE (`cc3ac541`)
reads `sm_bonding` when building the responder's pairing response and persists keys only if both sides bond;
bonded re-encryption does not depend on it; delayed-`CONNECT` adoption; advertising recovery; chord error paths;
patch hunks and the CMake guard. 6 findings, all accepted:

| # | Sev | Finding | Fix |
|---|---|---|---|
| 1 | High | Window deadline and `sm_bonding` were written from three tasks (BOOT, esp_timer, host) without synchronisation; an interleaving could leave "window closed, bonding enabled" | Every window/bonding/advertising change now runs on the NimBLE host task (the task that runs SMP): BOOT, the window timer, trust reset and the worker tick only post `ble_npl_event`s (`ev_open`, `ev_close`, `ev_reset`, `ev_reconcile`) |
| 2 | Med | The advertising-recovery tick could race window expiry and leave forbidden advertising running | `ev_reconcile` on the host task both starts **and stops** advertising to match policy |
| 3 | Med | During capture, Tab/Enter could still drive other sidebar widgets (Type, Disconnect, Release all) | The whole sidebar is disabled while capturing |
| 4 | Med | `chord_busy` did not cover capture's queued keys/RELEASE_ALL draining, overlapping script starts, or Release all vs a pending chord | Chords and Release all now run **inside** the ordered input pump; a pending counter (`InputTx`) tracks queued + in-flight items; `input_idle()` (no capture, no run, nothing pending) gates chords, Type and `start_run` |
| 5 | Med | If a chord's key-up and the follow-up RELEASE_ALL both failed, keepalives kept the held key alive | The pump then closes the session (`shutdown`), so the adapter's link-drop/keepalive release applies |
| 6 | Low | Type cleared its text before a busy rejection | Type is disabled unless input is idle, so `start_run` cannot reject it |

**3e: verification of the 3d fixes.** Confirmed sound: host-task ownership removes the BOOT/ENC_CHANGE interleaving
and the advertising check/start race; reconcile stops forbidden advertising; event init precedes the BOOT task's
first post; disabled sidebar cannot be re-enabled by nested widgets; one healthy pump orders chords, capture and
RELEASE_ALL. 3 findings, all accepted:

| # | Sev | Finding | Fix |
|---|---|---|---|
| 1 | High | After the deadline passed, `sm_bonding` stayed 1 until the queued `ev_close` ran, so a pairing admitted in that gap bonded after "expiry" | The window is now host-task state: open until `set_window(0)` runs on the host task, the same step that clears `sm_bonding`; the timer only requests the close. "Window open" and "bonding allowed" can no longer disagree |
| 2 | Med | Resetting a shared pending counter when replacing the pump let the old pump's late completions corrupt (and wrap) it | Each pump owns its counter (`InputTx::pending`); `input_idle()` reads the current pump's |
| 3 | Med | After an automatic reconnect the GUI kept using the pump bound to the closed `Device` | Pumps are keyed by connection session (`Device::same_session`, `Arc::ptr_eq`), not adapter id |

## 2026-10-03 — Pairing window at every power-on

- Asked (maintainer): advertise for pairing by default at power-on, BOOT only if that first 15 s is missed.
- Implemented: `CONFIG_KVMIT_BOOT_PAIRING_WINDOW_S` (default 15, 0 = off) opens the window at power-on, bonded or
  not. With a bond stored it opens only for a **physical** reset (`ESP_RST_POWERON`/`ESP_RST_EXT`), so a remotely
  triggered crash or watchdog reboot cannot reopen pairing. The window now closes on a **new** pairing only; the
  trusted controller reconnecting no longer uses it up (previously any bonded encryption closed it).
- Trade-off (docs/security.md): a power loss and return opens a 15 s window in which anyone in range could pair.
- Hardware-verified (release build, Linux controller): reset → "pairing window open for 15 s"; `kvmit pair` with no
  BOOT press at ~1.4 s → paired, window closed by the pairing; bonded reconnect at ~1.3 s left the window open, the
  timer closed it at 15.6 s and advertising went to "known controller only"; pairing after the window refused (0x205).
- Not verified: the non-physical-reset path (no crash/watchdog reset was induced).

**Review round 3f** (astra, on the two commits above). Confirmed sound: per-pump counters and session-keyed pumps;
host-task serialisation; no pairing/disconnect ordering lets a second new controller use a consumed window (NimBLE
persists the bond and delivers `ENC_CHANGE` in the same host execution); bonded reset classification is
conservative (USB-JTAG, brownout, deep sleep, software, panic, watchdog excluded). 2 findings, both accepted:

| # | Sev | Finding | Fix |
|---|---|---|---|
| 1 | High | A NimBLE host reset re-runs `on_sync()`; `esp_reset_reason()` still says POWERON, so the 15 s window would reopen without any physical action | The power-on window is offered once per chip boot (`boot_window_used`); a host re-sync only restores advertising |
| 2 | High | The deadline was soft: until the queued close ran on the host task, a fresh SMP request could still be admitted | The window timer's callback clears `sm_bonding` at the deadline (it only ever clears; the host task alone sets it, and `ev_close` undoes a stale clear after a reopen). NimBLE has no SMP-admission hook, so the residual gap is esp_timer dispatch latency |

Hardware-verified after 3f (release build): reset, wait past 15 s, `kvmit pair` refused; reset, `kvmit pair` at
~1.2 s paired and closed the window; `status` 90 ms.


- Hardware-verified (patched **diagnostic** build, whose boot scan occupies a controller slot so links land on
  handle 2, the case that used to panic): BOOT short press → `kvmit pair` "paired and connected" (in-window
  re-pair of a stored peer); then three consecutive bonded links on **handle 2** (pair, `status`, `status`),
  each encrypted+bonded, MTU 247, clean disconnect, advertising restarted, no panic; RTT 90/89 ms. Earlier the
  same session: a closed-window pairing attempt refused. Final patched **release** build flashed afterwards:
  bonded reconnect (handle 1), `kvmit status` RTT 90 ms, no diagnostic output.
- After the 3c fixes (release build, `sm_bonding` gated by the window): bonded reconnect with the window closed
  works (RTT 89 ms); a controller that lost its keys is refused with the window closed (0x205); after a BOOT
  press it pairs ("paired and connected", encrypted+bonded, MTU 247) and reconnects bonded (RTT 90 ms).

## 2026-10-03 — Typing over BLE into a real target

- Setup: Linux controller (Surface Laptop 4) → BLE → adapter USB port in a Windows 11 laptop; that laptop's HDMI into
  the MacroSilicon capture card on the controller, so every result was read back from a `kvmit video snap` frame.
- Verified: `kvmit key win r`, `kvmit key ctrl n`, Enter/Escape; `kvmit type` of five lines in a fresh Notepad tab
  (lower, upper, digits, every shifted and unshifted US-ASCII symbol) arrived exactly; `status` afterwards: 0 keys
  held, 0 bad frames. Seven further Run-box trials (~290 characters) had no losses.
- **Open issues found:**
  - Two single-character drops (`notepad` → `notead`; `p` lost from an alphabet run), both in the Windows Run box in
    the first typing after the target had just woken and signed in; not reproduced in 11 later runs. Cause unknown
    (Run-box autocomplete loading is a guess, not shown). The CLI reported `done` both times: it confirms adapter
    delivery, not what the target did with the keys.
  - Early tests were spoiled by the target sleeping/locking (keys went to the lock screen). Target power settings
    must keep it awake during provisioning; worth a note in the user docs.
  - `bluez-async` 0.8.2 panics in a tokio worker (`messagestream.rs:40`, `unwrap()` on D-Bus "No match with that
    id found") on some runs; output was unaffected each time, but a panicking task must be fixed before release.
  - `kvmit video snap` grabs the first frame; this capture card needs a few seconds of streaming (and sometimes a USB
    reset) before it shows the source, so snapshots took 1-8 retries.

## 2026-10-03 — LED, trust reset, replug

- Hardware-verified (maintainer observing the LED, serial log on COM): physical unplug/replug → `kvmit status`
  reconnects without pairing; 10 s BOOT hold → yellow ramp, three red flashes, fast blue; log `BOOT held 10 s:
  erasing BLE trust`, `pairing window open for 300 s`; the laptop's old keys are then refused (`new peer`, 0x205)
  and `kvmit pair` pairs afresh (`status` 91 ms). LED GPIO48 is correct for this board.
- Open (minor, desktop): the first `kvmit pair` after the trust reset failed while BlueZ still held an unpaired
  connection from the failed `status` ("Connected: yes, Paired: no"); a retry paired. `pair` should drop a stale
  link (or retry once) before pairing.

## 2026-10-03 — Keepalive release of a held key (deterministic)

- Added `kvmit key --hold <dur>` (also a user feature: hold F12/Del while a target boots). `kvmit type`/`key` no
  longer open a capture device unless the script waits on the screen (they held `/dev/video5` and blocked `snap`).
- Calibration: `--hold 3s j` → 80 characters in Notepad (Windows repeat ≈ 31/s after 0.5 s).
- Test: `--hold 60s k`, `kill -9` after 2 s, BLE link stayed connected (BlueZ keeps it). Notepad gained 146 `k`
  (≈ 5.2 s held, so released ≈ 3.2 s after the kill, consistent with the 5 s timeout from the last 1 s keepalive),
  then stayed at 226 characters from +8.6 s to +21.5 s. Read back through the capture card.

## 2026-10-03 — GUI check and release

- Maintainer exercised the GUI after the round-3d/3e changes (Send keys via the ordered pump, capture and
  Ctrl+Alt+Esc release, sidebar disabled during capture, Type): all worked as described.
- 0.1.0 (Linux controller) released from `main`; the Windows controller app is 0.2.0.


## 2026-10-04 — 0.2.0 (Windows controller): review round 4

- Asked: Windows pairing/scan, Media Foundation capture, BLE link fix (fast connection interval, bounded motion
  frames), top-bar GUI, OS-level keyboard grab (Win/Alt+Tab to the target, Ctrl+Alt+Esc releases).
- Adversarial review: astra (`codex exec -m gpt-6-astra`, high effort, read-only, static; ~147k tokens) of
  `main...windows-0.2.0` through `d19559d`: 16 findings, verdict "do not merge" until keyboard release, release-chord
  ordering and a key-logging violation were fixed. Orchestrator (Claude) adjudicated each against source; a
  finding right about the problem got the fix the orchestrator chose, not necessarily the reviewer's.

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | A hung GUI keeps the hook swallowing the controller's keyboard (release depended on the GUI) | **Confirmed; my "a hung GUI cannot trap the keyboard" comment was false.** Fixed: the helper handles the release chord itself (stops swallowing in the hook), and stops swallowing if the GUI's heartbeat (sent from the input-handling thread) stops for 3 s, telling the parent to release. **Hardware-verified in the VM**: GUI frozen with NtSuspendProcess, Win swallowed during the grace period, then opened the controller's Start menu after the timeout; the thawed GUI ended capture |
| 2 | Med | Keys held before capture: repeat becomes a captured press, release swallowed (stuck key); pre-held Ctrl breaks the chord | **Confirmed.** Fixed: the helper seeds the tracker from `GetAsyncKeyState`; pre-held keys' repeats/releases pass to the OS and count for the chord. Unit-tested, not hardware-tested |
| 3 | Med | Alt+PrintScreen (0x54) and Ctrl+Pause (ext 0x46) scan codes missing | **Confirmed.** Fixed + test |
| 4 | Med | Windows silently removes a slow hook; capture stays "on" | **Plausible.** Mitigated: a key press that reaches egui while the grab is on means the hook is not swallowing, so the GUI drops the grab and falls back to egui keys. Not hardware-tested (cannot force a hook timeout) |
| 5 | High | Keys/clicks queued after the release chord are still forwarded | **Confirmed.** Fixed: `until_release` makes the chord a terminal boundary (keys and the frame's mouse events); unit-tested. The helper also stops swallowing at the chord |
| 6 | Med | Hook keys and egui mouse buttons are two streams: Ctrl-click can reorder | **Confirmed; deferred.** Needs one ordered stream (mouse buttons through the helper's WH_MOUSE_LL). Documented limit: modifier+click ordering can break if a GUI frame takes longer than the click. Tracked in the roadmap backlog |
| 7 | Med | Keyboard-only capture without video: no prompt GUI wakeup | **Plausible, fixed**: the helper's reader wakes the GUI per event, and capture repaints every 100 ms (also the heartbeat) |
| 8 | Med | The 16-frame motion cap does not bound the unbounded transport queue | **Confirmed (inherited); deferred.** The cap bounds work per frame (the observed link-killing failure); end-to-end backpressure needs a bounded transport channel. Backlog |
| 9 | Med | Timer vs click: a click can overtake motion between take and send | **Confirmed (inherited, affects Linux).** Fixed: motion is taken and sent under the accumulator lock; a test asserts wire order for 50 interleaved clicks. Linux not re-run on hardware |
| 10 | High | A lost ReleaseAll at capture end leaves target keys held while keepalives continue | **Confirmed (inherited).** Fixed: a failed ReleaseAll closes the session so the adapter's link-drop release takes over |
| 11 | Med | Pairing accepts numeric comparison without comparing | **Confirmed.** Fixed: only `ConfirmOnly` (Just Works) is offered and accepted |
| 12 | High | Dropping a Media Foundation capture can hang the GUI (blocking `ReadSample`) | **Plausible.** Fixed: bounded wait (1.5 s) then detach. Not hardware-tested |
| 13 | Med | `MF_SOURCE_READERF_ERROR` retried forever; frozen video stays "good" | **Confirmed.** Fixed: terminal on ERROR/EOS or 20 failed reads; `Capture::failed()`; the GUI drops it and says so. **Hardware-verified in the VM** by unplugging the card mid-stream. The V4L2 backend has no equivalent yet |
| 14 | Low | `CoInitializeEx` never balanced | **Confirmed.** Fixed (guard calls `CoUninitialize`) |
| 15 | High | `hookcheck` prints pressed-key identities | **Confirmed: a violation of the no-logging rule by my own example.** Fixed: counts only, ends at the chord |
| 16 | Low | `linkstress` can exit 0 despite failed pings/no video frames; `hz` misdescribed | **Confirmed.** Fixed |

- What the reviewer found sound: motion arithmetic, normal hook state, process launch (no argument injection),
  Media Foundation ownership of the enumerated activates, privacy of the added diagnostics.
- Test grading (the reviewer's point that a green suite is evidence about the tests): the hook tests are pure
  tracker calls; the helper, heartbeat and process teardown are covered only by the hardware checks above. New unit
  tests cover pre-held keys, the full chord, the release boundary, the added scan codes and motion/click ordering.
- Not verified: bare-metal Windows; non-US keyboards/IME; the hook-removal fallback; capture-stall shutdown.

### Review round 5 (re-review of the round-4 fix commit `d73c0b5`)

astra (`gpt-6-astra`, high effort, read-only, static; `git diff d19559d d73c0b5`): 6 findings, 3 High, all in the fixes
themselves (the reason STD-001 re-reviews after a fix round). Verdict "do not merge yet". Adjudicated by the orchestrator:

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | The egui fallback after a failed grab can leave a forwarded right-hand modifier held on the target | **Confirmed.** Fixed differently from the suggestion: no fallback at all; the GUI ends capture (which sends release-all and drops the grab) |
| 2 | High | A pre-held key's repeat counted as "hook failed"; a late release notice could be lost in the same fallback | **Confirmed.** Fixed: the evidence is now only a fresh (non-repeat) press of a key the grab maps, and the response is the same safe end-capture; unit-tested predicate |
| 3 | High | The release chord and a click in the same frame re-capture at once | **Confirmed.** Fixed: `released_this_frame` blocks `begin_capture` for that frame |
| 4 | Med | Seeding held keys from virtual keys cannot tell Enter from keypad Enter (shared VK) | **Confirmed, inherent; accepted and documented** (CHANGELOG known limitations): a key of such an alias held across the instant capture begins can misbehave once |
| 5 | Med | The held-keys snapshot is taken before the hook is installed | **Confirmed (small).** Fixed: sampled and installed under the state lock inside the hook thread |
| 6 | Med | A blocked stdout write stops the heartbeat watchdog | **Confirmed.** Fixed: the watchdog is its own thread and turns swallowing off itself |

- Test grading from the review: the motion-order tests were "D for race coverage, B for serial ordering". Added a
  multi-thread, real-time stress test (clicks vs the concurrent motion timer; a race, so a stress test, not a proof).
- Hardware (Windows 11 VM) after the fixes: normal capture (Win x2 and Alt+Tab swallowed, chord releases, Win reaches the
  controller afterwards); helper process killed mid-capture (capture ends, keyboard returns); GUI process frozen
  (keyboard returns after the helper's timeout). Not hardware-tested: the same-frame release+click case, the new
  end-capture-on-unswallowed-key path, the held-key snapshot reordering.

## 2026-10-04 — Video-device switch, logo, Linux AppImage: review round 6

Three branches reviewed read-only by a different model (codex `gpt-6-astra`) before merging: `fix/video-device-switch`
(A), `branding/logo` (B), `linux-appimage` (C). Findings were adjudicated by reading the code; each fix is in the branch it
belongs to.

| Br | Sev | Finding | Verdict |
|---|---|---|---|
| A | Med | Switching back to the demo after a failed open leaves the picture blank (`last_seq` kept, still source always `seq 1`) | **Confirmed.** Fixed: sequence reset on every switch |
| A | Med | No way to reopen a stalled device now that the dropdown's Open is gone | **Confirmed.** Fixed: clicking the open row reopens it |
| A | Med | Demo path saved as `last_video`; `video snap` opens `demo:` without the env var | **Confirmed.** Fixed: never saved; opening is opt-in like listing |
| A | Med | Oversized PNG can assert in the texture upload | **Confirmed.** Fixed: dimensions checked (max 8192 a side) before decode; regular files only (also covers the FIFO remark) |
| A | Low | Non-UTF-8 demo path corrupted by lossy conversion | **Confirmed.** Fixed: such a path disables the source |
| A | High (inherited) | Dropping the old capture under the shared mutex can hang on a stalled V4L card | **Partly fixed.** The drop now happens outside the lock, so scripts' `screen()` is not blocked; the V4L thread's unbounded frame wait is unchanged and stays a known limitation (no hardware occurrence) |
| B | — | No substantive finding | — |
| C | Med | `--runtime-file` not given: the embedded runtime floats | **Confirmed.** Fixed: runtime pinned by URL and sha256 (digest taken on first use from the tagged release) |
| C | Med | glibc check passes if `objdump` fails (no pipefail in the container shell) | **Confirmed.** Fixed: inspect separately, failure is an error |
| C | Med | Host libraries not bundled (e.g. `libxkbcommon-x11`) | **Confirmed, by design.** Documented in README; the script prints what the binaries link; no bundling |
| C | Med | Not reproducible: floating base image, apt, file times | **Partly fixed.** Base pinned by digest, times from the commit; apt floats and is documented as such |

- Tests after the fixes: `scripts/rs.sh test` green (new: oversized and non-file demo pictures); the AppImage rebuilt with
  the changed script, `cli --version` run from it. The GUI popup interaction (reopen click) is not covered by a test.

## 2026-10-04 — 0.3.0 flasher (`feat/flash`): review round 7

Read-only review by a different model (codex `gpt-6-astra`) of `main...feat/flash` at `8bc17e9`: verdict "do not ship until device
identity, native-USB detection, NVS preservation and flash-operation lifecycle are enforced". 13 findings, all confirmed by reading;
fixed in the commit after this log.

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | A reused port name (`/dev/ttyACM0`) bypasses the checks: nothing re-identifies the device at flash time | **Confirmed.** `flash()` now takes the chosen `PortInfo` (name, vid:pid, USB serial), re-enumerates, and refuses if the same name is another device or gone. A small window between the check and the open remains (stated, unavoidable) |
| 2 | High | The native-USB rule is blind: the adapter's native port is HID-only and is not a serial port at all | **Confirmed, and the worst one.** Seen on the real board (`303a:4008`, no CDC). Now found by enumerating USB devices (`nusb`): the adapter's pid, a "kvm-it" name, or an unreadable Espressif device blocks; a differently named Espressif board does not. Checked in the CLI before the prompt, in the wizard on open/rescan, and again inside `flash()`. A cable plugged in *after* the check is not detectable |
| 3 | High | CH343 does not prove it is a kvm-it board; the chip is never checked | **Confirmed.** After connecting: must be an ESP32-S3 with the image's flash size and not in secure download mode, else nothing is written. Another ESP32-S3 with 16 MB behind a CH343 is still accepted; the person's choice and confirmation remain the last line |
| 4 | High | Validation ignores padding and erase sectors | **Confirmed.** Sector footprints (4 KiB) are used for overlap and for the settings check |
| 5 | High | A replaced partition table can move or shrink the settings | **Confirmed.** The installed table is read from the chip and its nvs region must equal the new image's, else a full erase is required |
| 6 | High | "Firmware OK" accepts one-byte images | **Confirmed.** Header checks (magic, segment count, ESP32-S3 chip id), a partition table with an app partition at the app's offset that fits it, nvs required. No checksum/hash walk of the images; they come from our own build |
| 7 | High | Manifest paths can read files outside the folder | **Confirmed.** Relative-only, no `..`, symlinks resolved and contained, regular files only |
| 8 | Med | Reads are unbounded; FIFOs hang; the wizard loads on the UI thread | **Confirmed, mostly.** Manifest 64 KiB, 8 entries, files regular and at most the flash size, duplicate keys rejected. Loading is still on the UI thread, now bounded and cannot block on a FIFO |
| 9 | High | Closing the wizard or the app mid-flash abandons a running write | **Confirmed.** No close button while writing, no second wizard, and an app close request is cancelled with a notice while flashing |
| 10 | Med | Flashing ignores scripts and the adapter connection | **Confirmed.** Blocked while a script runs or the controller is connected/connecting; the wizard is hidden while capturing |
| 11 | Med | "Verified" claimed even in secure download mode | **Confirmed.** Secure download mode is refused |
| 12 | Med | espflash needs Rust 1.95, the AppImage builder has 1.90 | **Confirmed.** AppImage builder moved to 1.99; rebuilt, glibc floor still 2.35, `cli flash --list` runs from it |
| 13 | Med | `serialport` is MPL-2.0 and nothing in the packages says so | **Confirmed.** `THIRD_PARTY_NOTICES.md` (shipped in the zip, the MSI and the AppImage; `verify-release.py` expects it). A generated all-dependencies licence listing is still to do |

- Tests after the fixes: 21 flash-crate tests (traversal incl. symlink, duplicate keys, sector sharing, headers, partition rules,
  same-name-other-device, the real board's HID-only descriptor, installed-vs-new tables) and the wizard's blocker rules;
  clippy clean; real firmware build validated by the tests.
- Hardware (second board; evening of 2026-10-04): the hardened `kvmit flash` on Linux: chip and flash-size identity, installed-table
  read-back, verified write of all three parts; the pairing survived a default flash (boot log "trusted controller stored", and a
  Windows controller reconnected after the pairing window closed) and was wiped by `--erase-all` ("trusted controller none"); with
  the adapter's native USB port plugged in, `kvmit flash` and `--any-port` both refused. Windows 11 VM (COM bridge passed through,
  COM3): `kvmit flash` and the GUI wizard each wrote and verified. Fixed in passing: the wizard was blocked by a connection
  attempt that keeps retrying. A write killed halfway through the app (Windows CLI) left a half-written app; flashing again recovered it with no BOOT button. **Not exercised on hardware:** the Linux GUI wizard, bare-metal Windows.
- The new MSI component (`THIRD_PARTY_NOTICES.md`) is built only by CI/the release machine, not tried here. (Later, in the VM: the
  zip and MSI built with `build-release.ps1`, the MSI installed, uninstalled, and `verify-release.py` passed.)
- Found while bundling the firmware: the local `firmware/build` that had been flashed so far was built 2.5 minutes before the last
  firmware commit (the pairing-window-once-per-boot fix), and builds are not byte-reproducible (the build time is embedded), so it
  cannot be said whether that board had the fix. `firmware/release` was rebuilt from current sources and flashed; pairing and
  reconnect were re-checked with exactly those bytes.

### Round 8 (re-review of the round-7 fixes, `8bc17e9..59a6f22`)

The reviewer's verdict: "do not ship until wrong-device/native-port gaps, temporary-file safety, validation/preservation holes and
BLE cancellation are fixed and re-reviewed". Of the 13 round-7 fixes it called 4 fully fixed, 7 partial and 1 regressed (round 7's
findings 3, 9, 11, 12, 13 fixed; 1, 2, 4, 5, 6, 8 partial; 10 regressed by the wizard's own connection cancel). 13 new findings (F1-F13):

| # | Sev | Finding | Verdict |
|---|---|---|---|
| F1 | High | Bridges with no (or duplicate) USB serial number: a swapped board under the same port name compares equal | **Confirmed; narrowed, not closed.** Without a serial two identical boards cannot be told apart by software. The app now says so beside the choice (GUI and CLI), re-checks the chip is an ESP32-S3 of the right flash size with a compatible settings layout, and prints the chip's MAC when done. The residual (a physical cable swap in the seconds between choosing and confirming, onto a board that passes all of those) is accepted |
| F2 | High | The adapter's ROM-mode native port (`303a:1001`, generic debug unit) was explicitly allowed | **Confirmed.** My test blessed it as "unrelated". Now *any* Espressif USB device blocks flashing (an adapter's own port cannot be told from another ESP board), documented |
| F3 | High | USB enumeration is not a complete inventory (nusb drops unreadable devices on Linux; half-readable descriptors slipped through) | **Partly fixed.** The descriptor-string heuristics are gone (vendor id alone decides). A device the OS will not let nusb describe can still be missing from the list, and a cable plugged in after the check is not seen: stated limits |
| F4 | High | The installed-table temp file is a predictable, replaceable path | **Confirmed.** A private, exclusively created, owner-only folder, removed on drop; the file is read as a bounded regular file |
| F5 | High | Invalid images pass (bare 24-byte header; partition table with a flipped MD5) | **Confirmed.** Images are walked like the ROM does (segments inside the file, XOR checksum, appended SHA-256); the table is parsed strictly (bad entries, MD5 verified, data after the MD5, ranges, overlaps, one `nvs`). The committed release images are checked by an always-on test |
| F6 | High | Equal NVS ranges do not prove the pairing stays reachable (labels, flags ignored) | **Confirmed.** The settings partition must be identical (offset, size, name, flags) in the installed and new tables; exactly one nvs partition, named `nvs` (what the firmware opens) |
| F7 | Med | Table location accepted anywhere, read-back always at 0x8000 | **Confirmed.** The table part must be at 0x8000 |
| F8 | Med | Erase footprint arithmetic assumes aligned offsets | **Confirmed.** Every part and partition must be 4 KiB aligned |
| F9 | Med | Opening the wizard does not cancel or exclude BLE work (retry in `Failed`, in-flight connect publishing late, Adapter popup still usable) | **Confirmed.** An attempt cancelled while in flight disconnects and never publishes (also on its error paths); the wizard cancels `Connecting` and `Failed` loops without forgetting the remembered adapter; Scan/Connect are disabled while it is open. A live connection must still be Disconnected by the person |
| F10 | Med | Containment/size checks race the actual open | **Partly fixed.** One open, checks on the handle, bounded read. A concurrent swap of a directory component needs write access to the firmware folder: accepted |
| F11 | Med | Implicit firmware lookup can pick attacker-controlled firmware (cwd; env var wins) | **Partly fixed.** Release builds never look in the working directory; only a debug build tries `firmware/release`. `KVMIT_FIRMWARE` still overrides on purpose (shown in the wizard and the CLI before confirming); there is no runtime signature on firmware |
| F12 | Med | The CI freshness guard compares commit ids (stale images pass, squash merges fail, scripts not covered) | **Confirmed.** A content hash of the firmware build inputs (sources, patches, lockfile, `scripts/fw.sh`) is recorded in `FIRMWARE.txt` and checked by CI and by `verify-release.py` (which the signed release runs). It binds the inputs to the record, not to the output bytes (builds are not reproducible) |
| F13 | Med | Windows `autocrlf` can rewrite the bundled JSON so its checksum is stale; verification never checks the inner sums | **Confirmed.** `.gitattributes` marks `firmware/release/**` as `-text`; `verify-release.py` verifies the zip's inner `SHA256SUMS` against its files |

- Tests after the fixes: 23 flash-crate tests (strict images and tables built with real checksums/SHA-256/MD5, the real release images
  always validated, label/size/move refusals, ROM-mode and half-readable USB devices, scratch folder), wizard rules; clippy clean.
- Hardware after the fixes (Windows 11 VM, COM bridge): the CLI default flash read back and strictly parsed the board's installed
  table; the GUI wizard blocked while connected ("Disconnect first"), then after Disconnect flashed and verified.

### Round 9 (re-review of the round-8 fixes, `59a6f22..7b8eafa`)

Verdict "do not ship yet": F5, F9, F10, F12, F13 incomplete, two regressions of mine, and a coverage gap. 11 findings (+1 low):

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | Med | A cancelled BLE attempt can still publish `Connected` (generation checked before taking the link lock) and its poll loop never notices cancellation | **Confirmed.** Every link-state write now goes through one `publish` that checks the generation *under the link lock*; the cancel bumps it under the same lock; the connected poll loop exits when cancelled |
| 2 | Med | `pair_first` paths write state unguarded; a failed handshake drops the connection without disconnecting (`Connection` has no `Drop`, its doc said it did) | **Confirmed.** Pair paths guarded; `session::connect` disconnects on handshake failure; the doc is corrected. Cancelling does not abort an in-flight OS operation, it closes it when it completes |
| 3 | High | Image walk accepts hash flag 2 (skipping SHA) and odd segment lengths | **Confirmed.** Any nonzero flag means a digest is present and checked; segment lengths must be multiples of 4; checked arithmetic (also the 32-bit low finding) |
| 4 | High | A table filled to its last slot (no terminator) passes | **Confirmed.** An erased terminator entry inside the 0xC00 window is required |
| 5 | High | App not on a 64 KiB boundary, a 0x1000 `data/ota`, bounds from a global maximum | **Confirmed.** App partitions on 64 KiB boundaries, OTA data exactly 0x2000, partition bounds from the image's flash size. This is a bounded rule set, not a reimplementation of ESP-IDF's partition semantics |
| 6 | Med | Valid installed tables without an MD5 entry are refused (needless "full erase") | **Confirmed.** The MD5 is required of the *new* table only; an installed table without one is accepted for the settings comparison |
| 7 | Med | Opening a FIFO blocks before it can be rejected | **Confirmed (my regression).** The file type is checked before opening, and again on the handle |
| 8 | Med | The manifest is read unbounded after its size check | **Confirmed.** One bounded reader for the manifest, parts and read-back; too big is an error, not a truncation |
| 9 | Med | `autocrlf` rewrites the hash inputs on Windows, so the freshness check fails there | **Confirmed (my regression).** `.gitattributes`: `firmware/**` and `scripts/fw.sh` are `-text` |
| 10 | Med | Inner `SHA256SUMS` coverage not required complete or unique | **Confirmed.** Must cover exactly the bundled files (except itself and FIRMWARE.txt); duplicates refused |
| 11 | High | Native USB of an S3 already running other firmware under another VID is not detected | **Accepted, documented.** Nothing in the descriptors says that device is an adapter; the rule stays "any Espressif device blocks". A stated detection limit alongside the OS-hidden-device and late-cable ones |

- Tests after the fixes: 28 flash-crate tests (new: hash flag, odd and overflowing segments, full table, OTA geometry, 64 KiB alignment,
  flash-size bounds, MD5-less installed table, FIFO, oversize); clippy clean. The BLE publish race has no automated test (the
  window is a few microseconds); it is covered by construction (one lock) and by the hardware run below.
- Hardware after the fixes (Windows 11 VM, COM bridge, the board): CLI flash; GUI Scan > Connect > wizard blocks while connected >
  Disconnect > flash verified.

### Round 10 (re-review of the round-9 fixes, `7b8eafa..18f2916`)

Verdict: "Medium BLE lifecycle defects remain; no High defect remains within the reviewed scope and stated accepted residuals". The reviewer
re-derived that the shipped firmware passes every check and found no path to the wrong port, a default erase of the pairing, or typing into the
flashing machine. 3 Medium, 2 Low, all confirmed, all in `gui.rs` start/cancel and the parser boundary:

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | Med | A superseded attempt (`start_link(B)` while A is mid-publish) can still publish: the bump did not take the link lock | **Confirmed.** `start_link` bumps the generation and sets `Connecting` under the link lock, in the same step |
| 2 | Med | An attempt queued but not yet running (state still `Disconnected`) escapes the wizard's cancel | **Confirmed.** Same change: the attempt is registered as `Connecting` synchronously, so `cancel_pending_link` always sees it |
| 3 | Med | A pairing that succeeds after cancellation leaves BlueZ's link open with no owner | **Confirmed (Linux).** After a cancelled pairing succeeds the controller calls the new `backend::release(id)` (BlueZ `Disconnect`; no-op elsewhere). **Compiled, not exercised**: the Bluetooth adapter belongs to the VM. (Superseded in round 11: removed.) |
| 4 | Low | Cancelled attempts can still write status and notices | **Confirmed.** Writes check the generation; `stop_link` and `cancel_pending_link` clear the status |
| 5 | Low | The public parser accepts a slice longer than the 0xC00 window | **Confirmed**, not exploitable (callers cap at 0xC00). The parser now refuses it |

- Tests: 29 flash-crate tests (new: the 0xC00 window); clippy clean. The cancellation interleavings still have no automated test (they are
  covered by construction: one lock, one registration step) and the hardware GUI flow (Scan > Connect > wizard blocks > Disconnect > flash)
  passes on the real board. **Not exercised:** the wizard opening while an attempt is mid-flight, and `backend::release`.

### Round 11 (re-review of the round-10 fixes, `18f2916..21aa7c1`)

Round-10 findings 1, 2 and 5 verified fixed. The new `backend::release` (round-10 finding 3) drew three Medium findings and the status-guard fix
a Low one:

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | Med | Cancel between pairing success and the connection loop bypasses the `release` cleanup | **Confirmed.** |
| 2 | Med | `release` identifies an address, not an owner: a stale pairing's cleanup can disconnect a newer attempt's wanted connection | **Confirmed (regression from round 10).** |
| 3 | Med | `release` re-resolves the default controller and can miss the one used for pairing (hci0/hci1); errors are discarded | **Confirmed (conditional on controller hotplug).** |
| 4 | Low | Generation checks are separate from the writes: a stale status/notice write can still land after a cancel | **Confirmed.** |

Decision: **`backend::release` is removed rather than patched.** It was Linux-only code that cannot be exercised on this rig and each fix
invited another ownership question. A pairing cannot be cancelled and takes seconds, so the case is removed by construction: the Flash
adapter button is disabled while a pairing is in flight (and Pair & connect is already disabled while the wizard is open). For finding 4,
all writes an attempt makes (link state, status, notices) now run under the link lock only if the attempt is still current, and the cancels
clear state under the same lock (lock order: link, then status/notice), so a write cannot slip between a check and the cancel.
- Tests: unchanged (29 flash-crate tests); clippy clean; hardware GUI flow (Scan > Connect > wizard blocks > Disconnect > flash) passes. The
  pairing-in-flight guard is not exercised (the test board was already paired, and Windows pairing returns at once).

### Round 12 (re-review of the round-11 fixes, `21aa7c1..af10e5c`)

Round-11 findings 2, 3 and 4 verified resolved (removing `release` removed its ownership and controller problems; attempt writes are atomic with the
cancels). The replacement design's pairing flag drew three Medium findings, all confirmed:

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | Med | The flag is cleared as soon as pairing returns, before the connection loop owns anything: a cancel in that gap still strands the link | **Confirmed.** The guard is now held until the first connection attempt has resolved (Connected published, or failed) |
| 2 | Med | A queued pairing can start after the wizard opened (the flag is set inside the task; the entry generation gate was gone) | **Confirmed.** The pairing is registered synchronously in `start_link`, before the task is spawned, and the task checks its generation before calling `pair` |
| 3 | Med | One shared boolean: overlapping pairings clear each other's flag | **Confirmed.** A counter with a drop guard (`PairingGuard`): registered per attempt, released on drop even if the task panics or is dropped |

- Tests: 29 flash-crate tests; clippy clean. The guard has no unit test (it lives in the egui app); hardware (Windows 11 VM, the board):
  Scan > Connect > wizard blocks > Disconnect > flash verified, and, after a Windows unpair, **Pair & connect** through the GUI inside the board's
  pairing window went Connecting > Connected. Not exercised: Flash adapter… enablement while a pairing is in flight (too brief to observe on
  Windows), Linux pairing.

### Round 13 (final verification of the round-12 fixes, `af10e5c..96cb43f`) — reviewer: Claude Opus 5.5

**Deviation from STD-001, recorded:** the usual reviewer (codex `gpt-6-astra`) was out of quota until 23:55 on 2026-10-04 (round 13 failed with a usage
limit and produced no findings). With the maintainer's explicit agreement, this round was done by a Claude Opus 5.5 subagent (read-only, same scope and
prompt shape as the Codex rounds), a different model from the one that wrote the code but in the same family. A Codex round 13 over the same diff is to be
run after the quota resets and its result logged here; it also covers the follow-up below.

Result: the three round-12 findings are resolved; **no High or Medium defect**; three Low (plausible) items, accepted for 0.3.0 and listed in the CHANGELOG
known limitations:

| # | Sev | Finding | Disposition |
|---|---|---|---|
| 1 | Low | A pairing that is *superseded* (a deliberate second click on Connect while it runs) returns without connect and may leave BlueZ's leftover link with no owner; the wizard only checks `Connected`, so it cannot see it | Accepted for 0.3.0 (needs a second click inside the pairing seconds; the link is idle and flashing resets the board, which drops it). Fix planned: disable Pair & connect / Connect while a pairing is in flight |
| 2 | Low | `backend::connect` returns early with `?` after `p.connect()` succeeded (on `discover_services`/`subscribe`/`notifications` errors) without disconnecting; if that is the first attempt after a pairing the guard is released with the link open | Accepted for 0.3.0 (older code outside this diff). Fix planned: disconnect on those error paths |
| 3 | Low | The guard is held for as long as `pair` plus the first connect take; neither has an overall timeout, so a hang keeps Flash adapter… disabled until it returns | Accepted; no path leaks the count permanently |

Checked and found sound by the reviewer: guard lifetime and `take()` placement; no task leaves the state `Connecting`; lock order (link, then status/notice) and
no await under a std mutex; `cancel_pending_link`/`stop_link`; auto-connect at startup; no path to flashing while connected or pairing other than the two orphan
links above.

### Release cut (0.3.0): verifier found a latent flaw

The first `v0.3.0` release run failed in `verify-release.py` ("kvmit-gui.exe: the version 0.3.0 is not stamped into the binary"); signing, packaging and the new
firmware checks had all passed. The check searches the executable for the version string; for 0.2.1 it passed only because the dependency path
`windows-future-0.2.1/...` is in the binary. The GUI had never embedded its own version. Fix: the window title is `kvm-it <version>` (a test guards it). Nothing was ever
attached to the v0.3.0 release, so with the maintainer's agreement the tag and the empty release were deleted and 0.3.0 re-cut on the fixed commit. (The Opus-5.5 review
did not cover this; it is one format string and a test.)

## 2026-10-05 - 0.4.0 boot drive (`feat/ipxe`): review round 14 (Claude Opus 5.5)

**Deviation from STD-001, recorded:** the usual reviewer (codex `gpt-6-astra`) was out of quota (it resets at 23:55), and the maintainer chose to release 0.4.0 on a Claude Opus 5.5 review ("if opus
signs off ... before codex"). A Codex round over the same diff is queued and its result will be logged here. The Opus subagent was read-only.

First report (`main...feat/ipxe` at `a54bd50`): **no High; three Medium; six Low.** Adjudication, all by reading and then by test:

| # | Sev | Finding | Verdict |
|---|---|---|---|
| M1 | Med | The new image script is a hash input but not `-text`: a Windows CRLF checkout breaks `fw-source-hash.py --check` in `verify-release.py` | **Confirmed** (same class as round 9 #9). `.gitattributes` now covers `scripts/build-ipxe*.sh` |
| M2 | Med | The drive is always on and its default script boots over HTTP unattended, then drops to an iPXE shell forever on failure; legacy BIOS runs empty boot code; real-PC keyboard enumeration unchecked | **Confirmed.** Default script is now inert: waits 5 s for `n`, else `exit` (and `exit` on failure); the boot sector is `INT 18h` and no partition is active; docs state the boot-order effect and the escape hatch (flash the 0.3.0 firmware; pairing kept); a switch to hide the drive is on the roadmap. Tested in a UEFI VM (no key: falls through to the boot menu; `n`: HTTPS demo, then a network Linux). **Not exercised:** a real PC's keyboard enumeration with the composite device |
| M3 | Med | Licence wording ("additional permissions for UEFI") is unsupported; a link plus "open an issue" is not a GPL source offer; the commit was only partly known | **Confirmed.** Wording corrected (GPL-2.0-or-later, many files also UBDL). The exact commit was resolved (`6262f1081fe1...`, `v2.0.0-375`); the official binary did not reproduce, so iPXE is now **built from that pinned unmodified commit by `scripts/build-ipxe.sh`** (reproducible: SOURCE_DATE_EPOCH pinned), and the source archive is attached to the release |
| L1 | Low | TinyUSB overrides the read-error sense data | **Confirmed**; documented in the code. Reads past the end are a host bug |
| L2 | Low | The write-refused counter cannot rise; "writes refused" evidence was the host's own write-protect flag | **Confirmed.** Tested properly on the real adapter with raw SCSI (SG_IO) bypassing the host: WRITE(10) refused as DATA PROTECT, WRITE(6)/(12), FORMAT UNIT, WRITE SAME, UNMAP as invalid commands, disk hash unchanged. Docs reworded |
| L3 | Low | README said a Windows target "only sees a USB keyboard and mouse" | **Confirmed**, fixed |
| L4 | Low | `refresh-firmware-release.py` left `../ipxe/ipxe.img` in the per-part manifest blocks | **Confirmed**, fixed |
| L5 | Low | Nothing ensured `ipxe.img` was rebuilt from current inputs | **Confirmed**; the refresh script now rebuilds it first |
| L6 | Low | Leftovers from the product-id change | Doc comment fixed; any VM passthrough by vid:pid needs `400a` |
| tests | - | The "never into the settings/phy_init" tests failed on an earlier check | The data-image rule is now a function tested directly against a table with every partition kind, plus a four-part manifest end to end |

### Round 14, continued: Codex result, the off-by-default toggle, and the sign-off

- **Codex (`gpt-6-astra`) at `a7ac215`:** agreed with the Opus adjudication and added three findings, all confirmed and fixed: the iPXE build script was not a hash input of the bundled firmware; the packages did not carry the GPL text; the image builder's timestamps depended on the local timezone (now `TZ=UTC`).
- **Opus re-verify (first):** NO SIGN-OFF on the fixes: bare `exit` in the default script (EDK2 then stops at the boot menu; now `exit 1`), the source-archive mechanism (now a workflow plus a manual fallback in `RELEASING.md`), plus Lows.
- **Design change by the maintainer:** the drive must not be on unless turned on by a button. Implemented as protocol 1.1 (`SET_BOOT_DRIVE` 0x61, a STATUS byte, capability bit 4), an NVS setting that defaults off and restarts the adapter on change, a CLI command and an Adapter-popup row.
- **Hardware check of the toggle (2026-10-05, real adapter)** found that HELLO still reported firmware 0.1.0 (hardcoded); fixed to 0.2.0.
- **Opus review of the toggle (`a7ac215..8863517`):** NO SIGN-OFF, one Medium, docs only: they claimed the USB product id differs between drive off (`303a:4008`) and on (`303a:400a`); the id is `400a` in both states (esp_tinyusb derives it from the compiled-in classes). **Confirmed on hardware** (both states enumerate as `400a`; the interface count is what changes). Fixed everywhere, with a note for anyone whose allow-list or VM passthrough keys on the id. Lows fixed: the restart timer is now created before the setting is committed (a creation failure can no longer leave the stored setting changed under a BUSY reply), a stale header comment, a hardcoded iPXE tag in the generated source statement, and the status docs now state what was verified. Lows left: three comments in `usb_hid.c` and the flasher that still say "exactly as before" about the interface list (accurate for interfaces, loose for the id), and the drive's setting persists across power cycles and re-flashes (documented; an always-visible indicator in the app is a candidate for 0.4.x).
- **Opus re-verify of those fixes (`8863517..9075114`):** **SIGN-OFF: no High or Medium defect remains.** Hash checks, host tests and the claims against the hardware facts all confirmed. The Codex round over the whole 0.4.0 diff, including the toggle, is still owed and will be logged here.

## 2026-10-06 - 0.4.1-0.4.5 video honesty, button contract, recording, themes (`dev/0.4.x`, PR #35): review round 17 (Codex `gpt-6-astra`)

The usual cross-model reviewer, read-only, high effort, over `feat/secure-boot...dev/0.4.x` (desktop only). Seven findings, each marked CONFIRMED by the reviewer and re-checked here against the source before accepting. Not run by the reviewer: Rust tests, Windows hardware.

| # | Sev | Finding | Verdict |
|---|---|---|---|
| 1 | High | A failed recording's cleanup deleted the final filename, which another instance (same second) could own | **Confirmed** (cleanup was `remove_file(final)`). ffmpeg now writes `.<pid>-<name>.part`; cleanup removes only that; the finished file is published by hard link (never replaces; next free name on a clash; rename fallback on filesystems without links) |
| 2 | High | Stopping a recording joined the encoder on the GUI thread (up to 120 s), including with input captured | **Confirmed.** Stop now runs on a worker; the bar shows "Finishing the recording…"; quitting waits at most 20 s |
| 3 | Med | A full queue dropped frames but the clock kept going: shorter, faster clip, and a wrong reported duration | **Confirmed.** Lost slots are owed and repaid as repeats (bounded to 2 s); `Finished.frames`/`duration` count frames actually written |
| 4 | Med | With the capture gone, nothing called `push`, so the duration cap and an encoder's death went unnoticed | **Confirmed.** `Recorder::check` runs when no frame arrives; a source gone for 10 s ends the clip with what exists |
| 5 | Med | A stored device key that matched nothing fell back to the old `/dev/videoN`, which may now be a webcam | **Confirmed.** The path is used only when no key was ever stored; regression test added |
| 6 | Med | Audio lookup (`pactl` / `ffmpeg`) ran on the GUI thread | **Confirmed.** Runs on a worker with a "looking for audio inputs…" state. Not done: `ffmpeg -version` at Start is still synchronous (a local binary; left, noted) |
| 7 | Med | "No signal" (flat fill) blocked recording, but a black boot screen is valid video | **Confirmed** (also a known gap from our own notes). Blank no longer blocks; the popup says it looks blank |

Fixes verified: `scripts/rs.sh test` and `clippy` clean; the ffmpeg end-to-end tests pass against the host's ffmpeg.

**Round 18** (re-review of the round-17 fixes, same reviewer): **NO SIGN-OFF**, eight findings, each confirmed from the control flow and fixed:

| # | Sev | Finding | Fix |
|---|---|---|---|
| 1 | High | Two recorders in one process/second/format chose the same final name, hence the same `.part`; one's abort deleted the other's file | The final name is reserved atomically at start (`create_new`, an empty placeholder only this recording owns); the part file derives from it; cleanup removes only those two |
| 2 | High | The rename fallback (no hard links) could replace another process's recording | Publishing is now a rename over our own reserved placeholder; the hard-link path is gone; a test starts and aborts two recorders in one second |
| 3 | High | Quitting after a 20 s wait abandoned finalisation (orphan ffmpeg, stuck `.part`) | A `Canceller` makes `finish` kill the encoder and remove the files; exit waits 20 s, cancels, waits 5 s more |
| 4 | Med | Owed frames were discarded at stop or cap; repeated failed repayments inflated `dropped` | `finish` repays what is owed (bounded 2 s) before closing; only newly lost slots count as dropped |
| 5 | Med | The no-frame deadline did not cover a recording still awaiting its first frame | Applies from the start; ends with `NoFrames` |
| 6 | Med | Rescan could leave a stale index selecting a different audio device | The list is read-only while a lookup runs, and the selection resets to "none" when a new list arrives |
| 7 | Med | A hung `pactl`/ffmpeg lookup was never killed | Lookups are killed after 5 s |
| 8 | Low | An automatic stop kept showing the REC timer during encoding | The worker flags finalising; the bar shows "Finishing the recording…" |

**Round 19** (re-review of the round-18 fixes): **NO SIGN-OFF**, five findings; four fixed, one accepted:

| # | Sev | Finding | Disposition |
|---|---|---|---|
| 1 | High | A cancel requested before the recorder was registered was lost (a fresh flag replaced it) | **Fixed.** The flag is made before the worker starts and handed to the recorder; a cancel can no longer be lost |
| 2 | Med | The audio lookup's 5 s did not bound the pipe readers if a descendant held them | **Fixed.** Readers get 1 s after the tool is killed, then are abandoned. Not changed: the recorder's own reader joins (ffmpeg spawns no descendants) |
| 3 | Med | Owed-frame flush gave up after 2 s and ignored cancel | **Fixed.** 10 s, cancel-aware. Residual debt after that is tolerated and shows in the dropped count |
| 4 | Med | An empty file under the final name appeared immediately and survived a crash | **Fixed.** The name is reserved by a hidden `.<name>.reserved` marker; nothing is visible under the final name until publication; a crash leaves only hidden files. A cancel now also wins over a clean exit |
| 5 | High (plausible) | A user deleting the placeholder, then another program creating that name, got overwritten | **Fixed by the same change:** cleanup never deletes the final name, and publication checks it is free (next free name otherwise; the check-then-rename gap against a non-cooperating writer remains, accepted) |

Verification after the fixes: `rs.sh test`/`clippy` clean; ffmpeg end-to-end (4 tests) pass. **No sign-off is claimed.** Each round has found issues in the previous round's fixes, with the severity of the remaining ones falling toward exotic races; a fourth round is the gate before release.

**Round 20** (final gate on the round-19 fixes): one Medium, no other finding: publication was check-then-rename, which a program that is not us could race. **Fixed:** publication tries a hard link first (atomic, never replaces; a taken name moves on to the next free one) and uses check-then-rename only on filesystems without hard links. The reviewer's verdict on round 20 was NO SIGN-OFF on that single finding; the fix has had **no further review round**, so **no sign-off is claimed**. The maintainer decides whether a fifth round is worth it before release.

## 2026-10-07 - macOS controller port (`feat/macos`, PR #38): review round 21 (Codex `gpt-6-astra`)

Static review (no Mac hardware exists for this project); every finding traced against the code and Apple's API contracts before accepting. **NO SIGN-OFF**, eight findings, all fixed:

| # | Sev | Finding | Fix |
|---|---|---|---|
| 1 | High | A 59.94/29.97 fps format rounded to 60/30 and `1/60` was set as the minimum frame duration: outside the range, AVFoundation throws (crash) | The chosen format's fastest `AVFrameRateRange` object is kept and its exact `minFrameDuration` applied; rounding only for scoring/display |
| 2 | High | The first camera-permission prompt blocked the GUI up to 120 s; startRunning/stopRunning ran on the GUI thread | Permission never waits (starts macOS's prompt and says "answer it, then open again"); the session starts and stops on worker threads |
| 3 | High | Event-tap start/stop could hang: a stop sent before the run loop ran was lost, and a slow start was joined without cancelling | The tap thread runs its run loop in 0.1 s slices checking a shutdown flag; a failed or slow start sets it before joining |
| 4 | High | A GUI stall let a modifier release pass to macOS unseen, then swallowing resumed with the key held on the target | A stall or macOS disabling the tap now gives up for good: everything passes, and a release is queued so capture ends with release-all when the GUI resumes |
| 5 | Med | Keys held before capture were not known (Ctrl held + Option+Esc did not release; a held key's release could be swallowed) | The tracker starts with the keys macOS's HID state reports as down (`CGEventSourceKeyState`), as on Windows |
| 6 | Med | The device was unlocked before the session started, so macOS could switch to its own format while the chip showed ours | Apple's macOS pattern: lock, set format and duration, startRunning, unlock (on the worker) |
| 7 | High (plausible) | `pair` subscribed to TX, which is notify-only (not encrypted), so it could report success before bonding | `pair` writes a harmless PING *with response* to RX (`WRITE_ENC`), which only succeeds once macOS has paired; 60 s bound |
| 8 | Med | AVFoundation audio was selected by name: duplicate names collide and a name starting with digits is read as an index | The index from ffmpeg's listing is the id; the name is only for display |

Verification after the fixes: `scripts/rs.sh test`, `clippy`, and `scripts/rs.sh macos` (clippy for aarch64-apple-darwin) clean. A re-review of the fixes is the next step.

**Round 22** (re-review of the round-21 fixes): **NO SIGN-OFF**, three Medium, all confirmed and fixed:

| # | Sev | Finding | Fix |
|---|---|---|---|
| 1 | Med | Start and stop workers were unordered: a capture dropped while starting could be stopped before its start ran, leaving it running | The stop worker joins the start worker first |
| 2 | Med | The 5 s stall clock ran during startup, and `failed()` latches, so a slow start discarded the capture for good | The stall clock runs only once the start has finished; startup has its own 20 s limit |
| 3 | Med | Nothing repainted the GUI when an async start finished, so the first frame or a start error could stay invisible while the app was idle | The GUI polls every 100 ms while a capture is open with no frame yet (all backends) |

Verification: `rs.sh test`, `clippy`, `rs.sh macos` clean.
