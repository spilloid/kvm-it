/* Persistent device configuration (NVS). The user-visible name and the boot-drive on/off setting are stored; bonds live in NimBLE's own
 * NVS store. Typed text and secrets are never stored. */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#define KVM_NAME_MAX 32

esp_err_t kvm_config_init(void);
/* NUL-terminated name; defaults to "kvm-it-XXXX" (last MAC bytes) until SET_NAME. */
void kvm_config_name(char out[KVM_NAME_MAX + 1]);
esp_err_t kvm_config_set_name(const char *name, size_t len);
/* Whether the read-only boot drive (iPXE) is presented to the target. OFF unless the user turned it on (SET_BOOT_DRIVE); read at boot,
 * because the USB descriptor is fixed for a session, so a change takes effect after a restart. */
bool kvm_config_boot_drive(void);
esp_err_t kvm_config_set_boot_drive(bool enabled);
/* 16-byte device id derived from the base MAC. */
void kvm_config_uuid(uint8_t out[16]);
