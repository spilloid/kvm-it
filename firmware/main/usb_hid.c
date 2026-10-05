#include "usb_hid.h"

#include <stdio.h>
#include <string.h>

#include "class/hid/hid_device.h"
#include "esp_log.h"
#include "esp_mac.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "hid_state.h"
#include "tinyusb.h"
#include "usb_msc.h"

static const char *TAG = "usb_hid";

/* The mass-storage drive is the LAST interface, so the two boot-protocol HID interfaces keep the numbers 0 and 1 that BIOS/UEFI keyboard
 * handling expects, and a firmware that ignores storage still sees exactly the keyboard and mouse it always did. */
enum { ITF_KEYBOARD = 0, ITF_MOUSE = 1, ITF_MSC = 2, ITF_COUNT = 3 };

/* No report IDs: each interface has exactly one report, which is also what the
 * boot protocol requires. */
static const uint8_t kbd_report_desc[] = {TUD_HID_REPORT_DESC_KEYBOARD()};
static const uint8_t mouse_report_desc[] = {TUD_HID_REPORT_DESC_MOUSE()};

#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + 2 * TUD_HID_DESC_LEN + TUD_MSC_DESC_LEN)
#define EP_KBD_IN 0x81
#define EP_MOUSE_IN 0x82
#define EP_MSC_OUT 0x03
#define EP_MSC_IN 0x83

static char serial_str[13]; /* MAC as hex, stable per board */

static const char *string_desc[] = {
    (char[]){0x09, 0x04},   /* 0: English (US) */
    "kvm-it",               /* 1: manufacturer */
    "kvm-it HID adapter",   /* 2: product */
    serial_str,             /* 3: serial */
    "kvm-it keyboard",      /* 4 */
    "kvm-it mouse",         /* 5 */
    "kvm-it boot drive",    /* 6 */
};

static const uint8_t config_desc[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_COUNT, 0, CONFIG_TOTAL_LEN, TUSB_DESC_CONFIG_ATT_REMOTE_WAKEUP, 100),
    TUD_HID_DESCRIPTOR(ITF_KEYBOARD, 4, HID_ITF_PROTOCOL_KEYBOARD, sizeof(kbd_report_desc), EP_KBD_IN, 8, 8),
    TUD_HID_DESCRIPTOR(ITF_MOUSE, 5, HID_ITF_PROTOCOL_MOUSE, sizeof(mouse_report_desc), EP_MOUSE_IN, 8, 8),
    TUD_MSC_DESCRIPTOR(ITF_MSC, 6, EP_MSC_OUT, EP_MSC_IN, 64), /* full-speed bulk packet size */
};

static void housekeeping_task(void *arg);

static hid_state_t g_state;
static SemaphoreHandle_t g_lock;
static bool g_release_pending;
static volatile bool g_caps_lock; /* from host LED output report */

uint8_t const *tud_hid_descriptor_report_cb(uint8_t instance)
{
    return instance == ITF_KEYBOARD ? kbd_report_desc : mouse_report_desc;
}

uint16_t tud_hid_get_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t report_type,
                               uint8_t *buffer, uint16_t reqlen)
{
    (void)report_id;
    if (report_type != HID_REPORT_TYPE_INPUT) {
        return 0; /* only input reports are supported */
    }
    uint16_t len = 0;
    xSemaphoreTake(g_lock, portMAX_DELAY);
    if (instance == ITF_KEYBOARD && reqlen >= 8) {
        buffer[0] = g_state.modifiers;
        buffer[1] = 0;
        memcpy(&buffer[2], g_state.keys, HID_STATE_MAX_KEYS);
        len = 8;
    } else if (instance == ITF_MOUSE && reqlen >= 5) {
        buffer[0] = g_state.buttons;
        memset(&buffer[1], 0, 4); /* no motion/wheel/pan in a state query */
        len = 5;
    }
    xSemaphoreGive(g_lock);
    return len;
}

/* Keyboard LED output report: byte 0 bit 1 is Caps Lock. */
void tud_hid_set_report_cb(uint8_t instance, uint8_t report_id, hid_report_type_t report_type,
                           uint8_t const *buffer, uint16_t bufsize)
{
    (void)report_id; (void)report_type;
    if (instance == ITF_KEYBOARD && bufsize >= 1) {
        g_caps_lock = (buffer[bufsize - 1] & 0x02) != 0;
    }
}

esp_err_t usb_hid_init(void)
{
    uint8_t mac[6];
    ESP_ERROR_CHECK(esp_read_mac(mac, ESP_MAC_WIFI_STA));
    snprintf(serial_str, sizeof(serial_str), "%02X%02X%02X%02X%02X%02X",
             mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);

    g_lock = xSemaphoreCreateMutex();
    if (!g_lock) {
        return ESP_ERR_NO_MEM;
    }
    hid_state_init(&g_state);
    usb_msc_init();

    const tinyusb_config_t cfg = {
        .device_descriptor = NULL, /* esp_tinyusb default (Espressif VID/PID) */
        .string_descriptor = string_desc,
        .string_descriptor_count = sizeof(string_desc) / sizeof(string_desc[0]),
        .external_phy = false,
        .configuration_descriptor = config_desc,
    };
    ESP_LOGI(TAG, "installing TinyUSB, serial %s", serial_str);
    esp_err_t err = tinyusb_driver_install(&cfg);
    if (err != ESP_OK) {
        return err;
    }
    if (xTaskCreate(housekeeping_task, "hid_house", 3072, NULL, 4, NULL) != pdPASS) {
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}

bool usb_hid_mounted(void)
{
    return tud_mounted() && !tud_suspended();
}

#define READY_TIMEOUT_MS 100

/* Deadline-based (not iteration-counted) so it does not depend on tick rate.
 * sdkconfig.defaults sets 1 kHz ticks, so 1 tick = 1 ms. */
static esp_err_t wait_ready(uint8_t itf)
{
    TickType_t deadline = xTaskGetTickCount() + pdMS_TO_TICKS(READY_TIMEOUT_MS);
    for (;;) {
        if (!usb_hid_mounted()) {
            return ESP_ERR_INVALID_STATE;
        }
        if (tud_hid_n_ready(itf)) {
            return ESP_OK;
        }
        if ((int32_t)(xTaskGetTickCount() - deadline) >= 0) {
            return ESP_ERR_TIMEOUT;
        }
        vTaskDelay(1);
    }
}

static esp_err_t send_keyboard_locked(void)
{
    esp_err_t err = wait_ready(ITF_KEYBOARD);
    if (err != ESP_OK) {
        return err;
    }
    return tud_hid_n_keyboard_report(ITF_KEYBOARD, 0, g_state.modifiers, g_state.keys)
               ? ESP_OK : ESP_FAIL;
}

static esp_err_t send_mouse_locked(int8_t dx, int8_t dy, int8_t wheel, int8_t pan)
{
    esp_err_t err = wait_ready(ITF_MOUSE);
    if (err != ESP_OK) {
        return err;
    }
    return tud_hid_n_mouse_report(ITF_MOUSE, 0, g_state.buttons, dx, dy, wheel, pan)
               ? ESP_OK : ESP_FAIL;
}

static esp_err_t flush_release_locked(void)
{
    if (!usb_hid_mounted()) {
        return ESP_ERR_INVALID_STATE; /* stays pending */
    }
    esp_err_t k = send_keyboard_locked();
    esp_err_t m = send_mouse_locked(0, 0, 0, 0);
    if (k == ESP_OK && m == ESP_OK) {
        g_release_pending = false;
        return ESP_OK;
    }
    return k != ESP_OK ? k : m;
}

static void housekeeping_task(void *arg)
{
    (void)arg;
    for (;;) {
        vTaskDelay(pdMS_TO_TICKS(50));
        xSemaphoreTake(g_lock, portMAX_DELAY);
        if (g_release_pending) {
            (void)flush_release_locked();
        }
        xSemaphoreGive(g_lock);
    }
}

bool usb_hid_caps_lock(void)
{
    return g_caps_lock;
}

/* If the report cannot be delivered, roll the state back so the caller's retry
 * is seen as a real transition rather than a redundant no-op. */
static esp_err_t apply_key(uint8_t usage, bool down)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    hid_state_t before = g_state;
    hid_state_result_t r = down ? hid_state_key_down(&g_state, usage)
                                : hid_state_key_up(&g_state, usage);
    esp_err_t err = ESP_OK;
    if (r == HID_STATE_CHANGED) {
        err = send_keyboard_locked();
        if (err != ESP_OK) {
            g_state = before;
        }
    } else if (r == HID_STATE_ROLLOVER) {
        err = ESP_ERR_NO_MEM;
    } else if (r == HID_STATE_INVALID) {
        err = ESP_ERR_INVALID_ARG;
    }
    xSemaphoreGive(g_lock);
    return err;
}

esp_err_t usb_hid_key_down(uint8_t usage) { return apply_key(usage, true); }
esp_err_t usb_hid_key_up(uint8_t usage) { return apply_key(usage, false); }

/* Local state is always cleared. If the all-zero reports cannot be delivered
 * (suspended, busy), g_release_pending stays set and the housekeeping task
 * retries until the host has actually been told. */
esp_err_t usb_hid_release_all(void)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    hid_state_release_all(&g_state);
    g_release_pending = true;
    esp_err_t err = flush_release_locked();
    xSemaphoreGive(g_lock);
    return err;
}

esp_err_t usb_hid_mouse_move(int8_t dx, int8_t dy)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    esp_err_t err = send_mouse_locked(dx, dy, 0, 0);
    xSemaphoreGive(g_lock);
    return err;
}

esp_err_t usb_hid_mouse_wheel(int8_t wheel, int8_t pan)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    esp_err_t err = send_mouse_locked(0, 0, wheel, pan);
    xSemaphoreGive(g_lock);
    return err;
}

esp_err_t usb_hid_mouse_button(uint8_t mask, bool down)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    hid_state_t before = g_state;
    hid_state_result_t r = down ? hid_state_button_down(&g_state, mask)
                                : hid_state_button_up(&g_state, mask);
    esp_err_t err = ESP_OK;
    if (r == HID_STATE_CHANGED) {
        err = send_mouse_locked(0, 0, 0, 0);
        if (err != ESP_OK) {
            g_state = before;
        }
    } else if (r == HID_STATE_INVALID) {
        err = ESP_ERR_INVALID_ARG;
    }
    xSemaphoreGive(g_lock);
    return err;
}

bool usb_hid_any_held(void)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    bool held = hid_state_any_held(&g_state);
    xSemaphoreGive(g_lock);
    return held;
}

void usb_hid_counts(uint8_t *keys, uint8_t *buttons)
{
    xSemaphoreTake(g_lock, portMAX_DELAY);
    uint8_t n = 0;
    for (int i = 0; i < HID_STATE_MAX_KEYS; i++) {
        n += g_state.keys[i] != 0;
    }
    *keys = n;
    *buttons = g_state.buttons;
    xSemaphoreGive(g_lock);
}
