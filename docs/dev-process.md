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
