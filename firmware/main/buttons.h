/* BOOT button (GPIO0): short press opens the pairing window; 10 s hold erases BLE trust.
 * RESET is the EN line (hardware reboot) and is not observable here. */
#pragma once
#include "esp_err.h"
esp_err_t buttons_start(void);
