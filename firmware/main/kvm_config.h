/* Persistent device configuration (NVS). Only the user-visible name is stored; bonds live in NimBLE's own
 * NVS store. Typed text and secrets are never stored. */
#pragma once
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#define KVM_NAME_MAX 32

esp_err_t kvm_config_init(void);
/* NUL-terminated name; defaults to "kvm-it-XXXX" (last MAC bytes) until SET_NAME. */
void kvm_config_name(char out[KVM_NAME_MAX + 1]);
esp_err_t kvm_config_set_name(const char *name, size_t len);
/* 16-byte device id derived from the base MAC. */
void kvm_config_uuid(uint8_t out[16]);
