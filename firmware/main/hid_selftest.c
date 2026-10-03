#include "hid_selftest.h"

#include "ascii_us.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "usb_hid.h"

static const char *TAG = "selftest";

#define HID_KEY_ENTER 0x28
#define HID_KEY_LSHIFT 0xE1
#define KEY_GAP_MS 20

static bool is_letter(uint8_t usage)
{
    return usage >= 0x04 && usage <= 0x1D;
}

static esp_err_t tap(uint8_t usage, bool shift)
{
    esp_err_t err = ESP_OK;
    if (shift && (err = usb_hid_key_down(HID_KEY_LSHIFT)) != ESP_OK) {
        return err;
    }
    err = usb_hid_key_down(usage);
    vTaskDelay(pdMS_TO_TICKS(KEY_GAP_MS));
    esp_err_t up = usb_hid_key_up(usage);
    if (shift) {
        esp_err_t su = usb_hid_key_up(HID_KEY_LSHIFT);
        if (up == ESP_OK) {
            up = su;
        }
    }
    vTaskDelay(pdMS_TO_TICKS(KEY_GAP_MS));
    return err != ESP_OK ? err : up;
}

static esp_err_t run_sequence(void)
{
    for (const char *p = HID_SELFTEST_TEXT; *p; p++) {
        ascii_us_key_t k;
        if (!ascii_us_lookup(*p, &k)) {
            return ESP_ERR_NOT_SUPPORTED;
        }
        /* With Caps Lock on, letters need Shift to be *lower*case, so invert. */
        bool shift = (is_letter(k.usage) && usb_hid_caps_lock()) ? !k.shift : k.shift;
        esp_err_t err = tap(k.usage, shift);
        if (err != ESP_OK) {
            return err;
        }
    }
    esp_err_t err = tap(HID_KEY_ENTER, false);
    if (err != ESP_OK) {
        return err;
    }
    /* Small, symmetric nudge: 20 px right in four steps, then 20 px back. The
     * pointer ends where it started (modulo host acceleration). */
    for (int i = 0; i < 4; i++) {
        if ((err = usb_hid_mouse_move(5, 0)) != ESP_OK) return err;
        vTaskDelay(pdMS_TO_TICKS(20));
    }
    for (int i = 0; i < 4; i++) {
        if ((err = usb_hid_mouse_move(-5, 0)) != ESP_OK) return err;
        vTaskDelay(pdMS_TO_TICKS(20));
    }
    return ESP_OK;
}

static void selftest_task(void *arg)
{
    (void)arg;
    ESP_LOGI(TAG, "waiting %d ms before test (focus a text field on the target now)", HID_SELFTEST_DELAY_MS);
    vTaskDelay(pdMS_TO_TICKS(HID_SELFTEST_DELAY_MS));

    while (!usb_hid_mounted()) {
        ESP_LOGW(TAG, "target has not configured the USB device yet; waiting");
        vTaskDelay(pdMS_TO_TICKS(1000));
    }

    ESP_LOGI(TAG, "sending \"%s\" + Enter + mouse nudge", HID_SELFTEST_TEXT);
    esp_err_t err = run_sequence();
    /* Whatever happened, never leave a key or button held on the target. */
    esp_err_t rel = usb_hid_release_all();
    if (err == ESP_OK && rel == ESP_OK) {
        ESP_LOGI(TAG, "self-test sent; all keys released. Done (runs once per boot).");
    } else {
        ESP_LOGE(TAG, "self-test failed: sequence=%s release=%s",
                 esp_err_to_name(err), esp_err_to_name(rel));
    }
    vTaskDelete(NULL);
}

void hid_selftest_start(void)
{
    if (xTaskCreate(selftest_task, "hid_selftest", 4096, NULL, 5, NULL) != pdPASS) {
        ESP_LOGE(TAG, "could not start self-test task (out of memory); USB HID is still up");
    }
}
