#include "led.h"
#include "ble_link.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "led_pattern.h"
#include "led_strip.h"
#include "sdkconfig.h"
#include "usb_hid.h"

static volatile uint32_t g_hold_ms;
static volatile uint32_t g_flash_until_ms;

static uint32_t now_ms(void) { return (uint32_t)(esp_timer_get_time() / 1000); }

void led_set_reset_hold(uint32_t held_ms) { g_hold_ms = held_ms; }
void led_flash_trust_erased(void) { g_flash_until_ms = now_ms() + 1000; }

static void led_task(void *arg)
{
    led_strip_handle_t strip = arg;
    uint32_t last_activity = 0, last_input_ms = 0;
    for (;;) {
        uint32_t now = now_ms();
        uint32_t act = ble_link_activity();
        if (act != last_activity) { last_activity = act; last_input_ms = now ? now : 1; }
        led_inputs_t in = {
            .link = ble_link_led_state(),
            .usb_mounted = usb_hid_mounted(),
            .last_input_ms = last_input_ms,
            .reset_hold_ms = g_hold_ms,
            .reset_flash_until_ms = g_flash_until_ms,
        };
        led_rgb_t c = led_pattern(&in, now);
        led_strip_set_pixel(strip, 0, c.r, c.g, c.b);
        led_strip_refresh(strip);
        vTaskDelay(pdMS_TO_TICKS(20));
    }
}

esp_err_t led_start(void)
{
#if CONFIG_KVMIT_LED_GPIO < 0
    ESP_LOGW(TAG, "status LED disabled");
    return ESP_OK;
#else
    led_strip_config_t sc = {.strip_gpio_num = CONFIG_KVMIT_LED_GPIO, .max_leds = 1,
                             .led_model = LED_MODEL_WS2812, .flags.invert_out = false};
    led_strip_rmt_config_t rc = {.clk_src = RMT_CLK_SRC_DEFAULT, .resolution_hz = 10 * 1000 * 1000};
    led_strip_handle_t strip;
    esp_err_t err = led_strip_new_rmt_device(&sc, &rc, &strip);
    if (err != ESP_OK) return err;
    led_strip_clear(strip);
    return xTaskCreate(led_task, "led", 3072, strip, 2, NULL) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM;
#endif
}
