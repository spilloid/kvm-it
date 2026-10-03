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
