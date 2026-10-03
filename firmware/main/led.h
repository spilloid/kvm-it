/* Status LED driver: samples device state every 20 ms and renders led_pattern(). */
#pragma once
#include "esp_err.h"
esp_err_t led_start(void);
/* BOOT-hold progress and trust-erased feedback come from the button task. */
void led_set_reset_hold(uint32_t held_ms);
void led_flash_trust_erased(void);
