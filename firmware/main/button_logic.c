#include "button_logic.h"

void btn_init(btn_t *b) { *b = (btn_t){0}; }

btn_event_t btn_update(btn_t *b, bool pressed, uint32_t now)
{
    if (pressed != b->raw_last) { b->raw_last = pressed; b->raw_changed_ms = now; }
    if (pressed != b->stable && (uint32_t)(now - b->raw_changed_ms) >= BTN_DEBOUNCE_MS) {
        b->stable = pressed;
        if (pressed) {
            b->pressed_since_ms = now;
            b->reset_fired = false;
        } else {
            uint32_t held = now - b->pressed_since_ms;
            // A long hold that already fired TRUST_RESET, or one past the short window, is not a short press.
            if (!b->reset_fired && held < BTN_SHORT_MAX_MS) return BTN_EVENT_SHORT_PRESS;
            return BTN_EVENT_NONE;
        }
    }
    if (b->stable && !b->reset_fired && (uint32_t)(now - b->pressed_since_ms) >= BTN_RESET_HOLD_MS) {
        b->reset_fired = true;
        return BTN_EVENT_TRUST_RESET;
    }
    return BTN_EVENT_NONE;
}

uint32_t btn_held_ms(const btn_t *b, uint32_t now) { return b->stable ? now - b->pressed_since_ms : 0; }
