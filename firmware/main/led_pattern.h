// Status LED language (docs/ux.md "LED"). Pure function of device state and time -> RGB, host-tested.
// The board's single RGB LED must tell a technician, at a glance and without the app, what the adapter is doing.
#pragma once
#include <stdbool.h>
#include <stdint.h>

typedef enum {
    LED_LINK_BOOTING,      // before BLE is up
    LED_LINK_IDLE,         // nothing paired and the pairing window is closed: radio is quiet (press BOOT)
    LED_LINK_PAIRING,      // pairing window open: anyone may pair (physical-presence gated)
    LED_LINK_RECONNECT,    // trusted controller known; waiting for it
    LED_LINK_CONNECTED,    // encrypted link + handshake done
    LED_LINK_FAULT,        // BLE stack failed to start
} led_link_t;

typedef struct {
    led_link_t link;
    bool usb_mounted;         // target has enumerated us
    uint32_t last_input_ms;   // time of last accepted input command (0 = never)
    uint32_t reset_hold_ms;   // >0 while BOOT is held for trust reset
    uint32_t reset_flash_until_ms;  // >now: trust was just erased (3 red flashes)
} led_inputs_t;

typedef struct { uint8_t r, g, b; } led_rgb_t;

// Colours are deliberately dim (<= 40/255): the LED sits beside a screen and is very bright at full scale.
led_rgb_t led_pattern(const led_inputs_t *in, uint32_t now_ms);
