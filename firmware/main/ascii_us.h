/*
 * Minimal US-ANSI ASCII -> HID usage mapping, used ONLY by the firmware
 * self-test. Real text injection (Milestone 6) is done on the controller by
 * the desktop keyboard-layout abstraction, so the target-side firmware never
 * needs to know about layouts.
 */
#pragma once

#include <stdbool.h>
#include <stdint.h>

typedef struct {
    uint8_t usage; /* HID usage page 0x07 code */
    bool shift;    /* needs Left Shift held */
} ascii_us_key_t;

/* Returns false for characters with no key on a US ANSI keyboard. */
bool ascii_us_lookup(char c, ascii_us_key_t *out);
