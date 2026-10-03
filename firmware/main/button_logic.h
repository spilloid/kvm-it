// BOOT button (GPIO0) gesture recogniser. Pure C, host-tested; fed raw samples by the GPIO task.
// RESET (EN) is a hardware line that reboots the chip; software cannot observe it, so it has no gesture.
#pragma once
#include <stdbool.h>
#include <stdint.h>

#define BTN_DEBOUNCE_MS 30
#define BTN_SHORT_MAX_MS 3000      // released before this = short press (open pairing window)
#define BTN_RESET_HOLD_MS 10000    // held this long = erase trust and re-enter pairing

typedef enum { BTN_EVENT_NONE = 0, BTN_EVENT_SHORT_PRESS, BTN_EVENT_TRUST_RESET } btn_event_t;

typedef struct {
    bool stable;            // debounced level: true = pressed
    bool raw_last;
    uint32_t raw_changed_ms;
    uint32_t pressed_since_ms;
    bool reset_fired;
} btn_t;

void btn_init(btn_t *b);
// Feed one sample. Returns at most one event.
btn_event_t btn_update(btn_t *b, bool pressed, uint32_t now_ms);
// Milliseconds the button has been held (debounced), 0 if released.
uint32_t btn_held_ms(const btn_t *b, uint32_t now_ms);
