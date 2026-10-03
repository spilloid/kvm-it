#include "hid_state.h"

#include <string.h>

#define USAGE_FIRST_KEY 0x04
#define USAGE_LAST_KEY 0xDD
#define USAGE_FIRST_MOD 0xE0
#define USAGE_LAST_MOD 0xE7
#define BUTTON_MASK_ALL 0x07

void hid_state_init(hid_state_t *s)
{
    memset(s, 0, sizeof(*s));
}

static bool is_modifier(uint8_t usage)
{
    return usage >= USAGE_FIRST_MOD && usage <= USAGE_LAST_MOD;
}

static bool is_plain_key(uint8_t usage)
{
    return usage >= USAGE_FIRST_KEY && usage <= USAGE_LAST_KEY;
}

hid_state_result_t hid_state_key_down(hid_state_t *s, uint8_t usage)
{
    if (is_modifier(usage)) {
        uint8_t bit = (uint8_t)(1u << (usage - USAGE_FIRST_MOD));
        if (s->modifiers & bit) {
            return HID_STATE_UNCHANGED;
        }
        s->modifiers |= bit;
        return HID_STATE_CHANGED;
    }
    if (!is_plain_key(usage)) {
        return HID_STATE_INVALID;
    }
    int free_slot = -1;
    for (int i = 0; i < HID_STATE_MAX_KEYS; i++) {
        if (s->keys[i] == usage) {
            return HID_STATE_UNCHANGED;
        }
        if (s->keys[i] == 0 && free_slot < 0) {
            free_slot = i;
        }
    }
    if (free_slot < 0) {
        return HID_STATE_ROLLOVER;
    }
    s->keys[free_slot] = usage;
    return HID_STATE_CHANGED;
}

hid_state_result_t hid_state_key_up(hid_state_t *s, uint8_t usage)
{
    if (is_modifier(usage)) {
        uint8_t bit = (uint8_t)(1u << (usage - USAGE_FIRST_MOD));
        if (!(s->modifiers & bit)) {
            return HID_STATE_UNCHANGED;
        }
        s->modifiers &= (uint8_t)~bit;
        return HID_STATE_CHANGED;
    }
    if (!is_plain_key(usage)) {
        return HID_STATE_INVALID;
    }
    for (int i = 0; i < HID_STATE_MAX_KEYS; i++) {
        if (s->keys[i] == usage) {
            s->keys[i] = 0;
            return HID_STATE_CHANGED;
        }
    }
    return HID_STATE_UNCHANGED;
}

hid_state_result_t hid_state_button_down(hid_state_t *s, uint8_t mask)
{
    if (mask == 0 || (mask & ~BUTTON_MASK_ALL)) {
        return HID_STATE_INVALID;
    }
    if ((s->buttons & mask) == mask) {
        return HID_STATE_UNCHANGED;
    }
    s->buttons |= mask;
    return HID_STATE_CHANGED;
}

hid_state_result_t hid_state_button_up(hid_state_t *s, uint8_t mask)
{
    if (mask == 0 || (mask & ~BUTTON_MASK_ALL)) {
        return HID_STATE_INVALID;
    }
    if (!(s->buttons & mask)) {
        return HID_STATE_UNCHANGED;
    }
    s->buttons &= (uint8_t)~mask;
    return HID_STATE_CHANGED;
}

bool hid_state_any_held(const hid_state_t *s)
{
    if (s->modifiers || s->buttons) {
        return true;
    }
    for (int i = 0; i < HID_STATE_MAX_KEYS; i++) {
        if (s->keys[i]) {
            return true;
        }
    }
    return false;
}

bool hid_state_release_all(hid_state_t *s)
{
    bool held = hid_state_any_held(s);
    hid_state_init(s);
    return held;
}
