#include "esp_app_desc.h"
#include "esp_log.h"
#include "ble_link.h"
#include "buttons.h"
#include "hid_selftest.h"
#include "kvm_config.h"
#include "led.h"
#include "sdkconfig.h"
#include "usb_hid.h"

static const char *TAG = "kvm-it";

void app_main(void)
{
    const esp_app_desc_t *app = esp_app_get_description();
    ESP_LOGI(TAG, "kvm-it firmware %s (ESP-IDF %s)", app->version, app->idf_ver);

    /* Boot order (docs/architecture.md): LED first so a failure is visible, then USB HID so the target
     * always sees a working keyboard/mouse even if BLE fails, then BLE (reconnect or pairing window). */
    ESP_ERROR_CHECK(kvm_config_init());
    if (led_start() != ESP_OK) ESP_LOGW(TAG, "status LED unavailable");
    ESP_ERROR_CHECK(usb_hid_init());
    ESP_ERROR_CHECK(buttons_start());
    if (ble_link_start() != ESP_OK) ESP_LOGE(TAG, "BLE failed to start; USB HID still works but no remote control");
#if CONFIG_KVMIT_SELFTEST
    hid_selftest_start();
#endif
}
