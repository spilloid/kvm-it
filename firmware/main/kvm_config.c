#include "kvm_config.h"
#include <stdio.h>
#include <string.h>
#include "esp_mac.h"
#include "nvs.h"
#include "nvs_flash.h"

#define NS "kvmit"

esp_err_t kvm_config_init(void)
{
    esp_err_t err = nvs_flash_init();
    if (err == ESP_ERR_NVS_NO_FREE_PAGES || err == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        err = nvs_flash_init();
    }
    return err;
}

void kvm_config_name(char out[KVM_NAME_MAX + 1])
{
    nvs_handle_t h;
    size_t n = KVM_NAME_MAX + 1;
    if (nvs_open(NS, NVS_READONLY, &h) == ESP_OK) {
        esp_err_t e = nvs_get_str(h, "name", out, &n);
        nvs_close(h);
        if (e == ESP_OK && out[0]) return;
    }
    uint8_t mac[6];
    esp_read_mac(mac, ESP_MAC_BT);
    snprintf(out, KVM_NAME_MAX + 1, "kvm-it-%02X%02X", mac[4], mac[5]);
}

esp_err_t kvm_config_set_name(const char *name, size_t len)
{
    if (len == 0 || len > KVM_NAME_MAX) return ESP_ERR_INVALID_ARG;
    char buf[KVM_NAME_MAX + 1];
    memcpy(buf, name, len);
    buf[len] = 0;
    nvs_handle_t h;
    esp_err_t err = nvs_open(NS, NVS_READWRITE, &h);
    if (err != ESP_OK) return err;
    err = nvs_set_str(h, "name", buf);
    if (err == ESP_OK) err = nvs_commit(h);
    nvs_close(h);
    return err;
}

void kvm_config_uuid(uint8_t out[16])
{
    uint8_t mac[6];
    esp_read_mac(mac, ESP_MAC_BT);
    memcpy(out, "KVMIT\x01", 6);
    memcpy(out + 6, mac, 6);
    memset(out + 12, 0, 4);
}
