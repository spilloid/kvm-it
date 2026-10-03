/* BLE peripheral link (controller <-> adapter only; target-facing input is USB HID).
 *
 * Trust model (docs/security.md): LE Secure Connections bonding, Just Works, gated by physical presence.
 *  - No bond stored: a pairing window opens at boot (plug-in = presence) and stays open CONFIG_KVMIT_PAIRING_WINDOW_S.
 *  - Bond stored: only the bonded controller may connect; others are disconnected at once.
 *  - BOOT short press reopens the window (new pairing replaces the old bond); BOOT 10 s hold erases the bond.
 * Both GATT characteristics require an encrypted link, so an unbonded peer can never reach the protocol. */
#pragma once
#include <stdint.h>
#include "esp_err.h"
#include "led_pattern.h"

esp_err_t ble_link_start(void);
void ble_link_open_pairing(uint32_t seconds);
void ble_link_trust_reset(void);
led_link_t ble_link_led_state(void);
/* Increments for every accepted input command (drives the LED activity flash). */
uint32_t ble_link_activity(void);
