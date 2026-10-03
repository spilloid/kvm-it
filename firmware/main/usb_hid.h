/*
 * Target-facing USB HID (native ESP32-S3 USB via TinyUSB).
 * Two separate boot-protocol interfaces - keyboard and mouse - rather than one
 * report-ID composite: BIOS/UEFI firmware commonly only drives boot-protocol
 * keyboards reliably when they own their interface.
 *
 * All senders are serialized by an internal mutex and update one hid_state_t,
 * so duplicate key-down/key-up never reaches the wire.
 */
#pragma once

#include <stdbool.h>
#include <stdint.h>

#include "esp_err.h"

esp_err_t usb_hid_init(void);

/* True once the target host has configured the device. False when unplugged
 * or suspended; senders then return ESP_ERR_INVALID_STATE. */
bool usb_hid_mounted(void);

/* Caps Lock state as last reported by the target (LED output report). */
bool usb_hid_caps_lock(void);

esp_err_t usb_hid_key_down(uint8_t usage);
esp_err_t usb_hid_key_up(uint8_t usage);
esp_err_t usb_hid_release_all(void);

/* Relative motion, clamped to -127..127 per call by the caller's contract. */
esp_err_t usb_hid_mouse_move(int8_t dx, int8_t dy);
esp_err_t usb_hid_mouse_button(uint8_t mask, bool down);
esp_err_t usb_hid_mouse_wheel(int8_t wheel, int8_t pan);

/* Any key or button currently held according to the HID state. */
bool usb_hid_any_held(void);
/* Number of pressed non-modifier keys and the button mask, for STATUS. */
void usb_hid_counts(uint8_t *keys, uint8_t *buttons);
