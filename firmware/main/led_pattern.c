#include "led_pattern.h"

#define DIM 32

static const led_rgb_t OFF = {0, 0, 0};

static led_rgb_t on(uint8_t r, uint8_t g, uint8_t b) { return (led_rgb_t){r, g, b}; }

// duty-cycle blink: on for on_ms of every period_ms
static bool blink(uint32_t now, uint32_t period, uint32_t on_ms) { return (now % period) < on_ms; }

led_rgb_t led_pattern(const led_inputs_t *in, uint32_t now)
{
    // 1. Trust reset feedback outranks everything: the user is holding BOOT and must know how far along they are.
    if (in->reset_flash_until_ms > now)  // erased: three red flashes
        return blink(now, 300, 150) ? on(DIM, 0, 0) : OFF;
    if (in->reset_hold_ms >= 3000) {
        // Past the "short press" zone: yellow, blinking faster as the threshold (10 s) approaches; solid at 10 s.
        if (in->reset_hold_ms >= 10000) return on(DIM, DIM / 2, 0);
        uint32_t period = in->reset_hold_ms < 6000 ? 600 : in->reset_hold_ms < 9000 ? 300 : 120;
        return blink(now, period, period / 2) ? on(DIM, DIM / 2, 0) : OFF;
    }

    // 2. Faults are loud.
    if (in->link == LED_LINK_FAULT) return blink(now, 250, 125) ? on(DIM, 0, 0) : OFF;

    // 3. Base colour: the controller link.
    led_rgb_t base;
    switch (in->link) {
    case LED_LINK_BOOTING:   base = on(DIM, DIM, DIM); break;                                   // white
    case LED_LINK_IDLE:      base = blink(now, 3000, 100) ? on(DIM, 0, DIM) : OFF; break;           // rare magenta tick
    case LED_LINK_PAIRING:   base = blink(now, 250, 125) ? on(0, 0, DIM) : OFF; break;           // fast blue
    case LED_LINK_RECONNECT: base = blink(now, 1500, 200) ? on(0, 0, DIM) : OFF; break;          // slow blue tick
    case LED_LINK_CONNECTED: base = on(0, DIM / 2, 0); break;                                    // steady green
    default:                 base = OFF; break;
    }

    // 4. Input activity: brief white-ish flash on the connected steady colour.
    if (in->link == LED_LINK_CONNECTED && in->last_input_ms && (uint32_t)(now - in->last_input_ms) < 60)
        base = on(DIM, DIM, DIM);

    // 5. Target-USB not enumerated: amber blip every 2 s on top of whatever else, so "BLE is fine but the
    // target can't see me" never looks like "all good".
    if (!in->usb_mounted && in->link != LED_LINK_BOOTING && blink(now, 2000, 150)) return on(DIM, DIM / 3, 0);
    return base;
}
