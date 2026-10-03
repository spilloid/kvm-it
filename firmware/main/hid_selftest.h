/*
 * Milestone 2 deterministic HID self-test. Runs once per boot, in its own task,
 * and only ever sends the fixed, non-secret string below.
 */
#pragma once

#define HID_SELFTEST_DELAY_MS 8000
#define HID_SELFTEST_TEXT "HELLO FROM KVM"

void hid_selftest_start(void);
