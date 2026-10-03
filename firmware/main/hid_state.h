/*
 * HID state machine: the single source of truth for what the target believes
 * is pressed. Pure C, no ESP-IDF dependencies, unit-tested on the host
 * (firmware/test/host). Every mutator returns whether the state changed, so a
 * duplicate key-down or key-up never produces a redundant USB report.
 */
#pragma once

#include <stdbool.h>
#include <stdint.h>

#define HID_STATE_MAX_KEYS 6 /* boot-protocol keyboard report limit */

typedef enum {
    HID_STATE_UNCHANGED = 0, /* duplicate / no-op: do not send a report */
    HID_STATE_CHANGED,       /* state changed: send a report */
    HID_STATE_ROLLOVER,      /* 7th key refused; state unchanged, no phantom keys */
    HID_STATE_INVALID,       /* usage code not accepted */
} hid_state_result_t;

typedef struct {
    uint8_t modifiers;                  /* bit n = usage 0xE0+n */
    uint8_t keys[HID_STATE_MAX_KEYS];   /* pressed non-modifier usages, 0 = empty slot */
    uint8_t buttons;                    /* mouse buttons, bit 0 left, 1 right, 2 middle */
} hid_state_t;

void hid_state_init(hid_state_t *s);

/* Usage page 0x07 codes. 0xE0-0xE7 are modifiers; 0x04-0xDD are ordinary keys. */
hid_state_result_t hid_state_key_down(hid_state_t *s, uint8_t usage);
hid_state_result_t hid_state_key_up(hid_state_t *s, uint8_t usage);

hid_state_result_t hid_state_button_down(hid_state_t *s, uint8_t mask);
hid_state_result_t hid_state_button_up(hid_state_t *s, uint8_t mask);

/* Clears keyboard and mouse state. Returns true if anything was held. */
bool hid_state_release_all(hid_state_t *s);

bool hid_state_any_held(const hid_state_t *s);
