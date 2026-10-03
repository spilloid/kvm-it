#include "buttons.h"
#include "ble_link.h"
#include "button_logic.h"
#include "driver/gpio.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "led.h"
#include "sdkconfig.h"

static const char *TAG = "buttons";

static void button_task(void *arg)
{
    (void)arg;
    btn_t b;
    btn_init(&b);
    for (;;) {
        uint32_t now = (uint32_t)(esp_timer_get_time() / 1000);
        btn_event_t e = btn_update(&b, gpio_get_level(CONFIG_KVMIT_BOOT_GPIO) == 0, now);
        led_set_reset_hold(btn_held_ms(&b, now));
        if (e == BTN_EVENT_SHORT_PRESS) {
            ESP_LOGI(TAG, "BOOT short press: opening pairing window");
            ble_link_open_pairing(CONFIG_KVMIT_PAIRING_WINDOW_S);
        } else if (e == BTN_EVENT_TRUST_RESET) {
            ESP_LOGW(TAG, "BOOT held 10 s: erasing BLE trust");
            ble_link_trust_reset();
            led_flash_trust_erased();
        }
        vTaskDelay(pdMS_TO_TICKS(10));
    }
}

esp_err_t buttons_start(void)
{
    gpio_config_t io = {.pin_bit_mask = 1ULL << CONFIG_KVMIT_BOOT_GPIO, .mode = GPIO_MODE_INPUT,
                        .pull_up_en = GPIO_PULLUP_ENABLE};
    esp_err_t err = gpio_config(&io);
    if (err != ESP_OK) return err;
    return xTaskCreate(button_task, "buttons", 3072, NULL, 3, NULL) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM;
}
