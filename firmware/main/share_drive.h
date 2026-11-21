#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "esp_err.h"

esp_err_t share_drive_init(void);
bool share_reserve_restart(void);
void share_cancel_restart(void);
bool share_present(void);
bool share_media_changed(void);
uint32_t share_blocks(void);
void share_eject(void);
int32_t share_usb_io(bool write, uint32_t block, uint32_t offset, void *data, uint32_t len);
