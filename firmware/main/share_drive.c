/* A single writable medium. Only the target or one leased UART client can
 * access sectors at a time. The immutable iPXE partition never enters here. */
#include "share_drive.h"
#include "usb_hid.h"
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "driver/uart.h"
#include "esp_partition.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "kvm_config.h"
#include "proto_frame.h"
#include "wear_levelling.h"

#define SECTOR 512u
#define MAX_LINE 1152
static wl_handle_t disk = WL_INVALID_HANDLE;
static const esp_partition_t *partition;
static SemaphoreHandle_t lock;
static bool presented, media_changed;
static char lease[17], board[33];
static int64_t expires;
static char last_request[MAX_LINE], reply[MAX_LINE];

static char digit(unsigned n) { return "0123456789abcdef"[n & 15]; }
static void hex(const uint8_t *p, size_t n, char *out)
{
    for (size_t i = 0; i < n; ++i) { *out++ = digit(p[i] >> 4); *out++ = digit(p[i]); }
    *out = 0;
}
static int nibble(char c)
{
    if (c >= '0' && c <= '9') return c - '0';
    return c >= 'a' && c <= 'f' ? c - 'a' + 10 : -1;
}

uint32_t share_blocks(void) { return disk == WL_INVALID_HANDLE ? 0 : wl_size(disk) / SECTOR; }
bool share_present(void)
{
    if (!lock) return false;
    xSemaphoreTake(lock, portMAX_DELAY);
    bool ready = presented;
    xSemaphoreGive(lock);
    return ready;
}
bool share_media_changed(void)
{
    if (!lock) return false;
    xSemaphoreTake(lock, portMAX_DELAY);
    bool changed = media_changed;
    media_changed = false;
    xSemaphoreGive(lock);
    return changed;
}
void share_eject(void)
{
    if (!lock) return;
    xSemaphoreTake(lock, portMAX_DELAY);
    presented = false;
    media_changed = false;
    xSemaphoreGive(lock);
}
static esp_err_t write_sector(size_t addr, const uint8_t *bytes)
{
    esp_err_t err = wl_erase_range(disk, addr, SECTOR);
    if (err != ESP_OK) return err;
    err = wl_write(disk, addr, bytes, SECTOR);
    if (err != ESP_OK) return err;
    static uint8_t check[SECTOR]; /* Both callers hold lock through verification. */
    err = wl_read(disk, addr, check, sizeof(check));
    if (err != ESP_OK) return err;
    return memcmp(check, bytes, SECTOR) == 0 ? ESP_OK : ESP_ERR_INVALID_CRC;
}
int32_t share_usb_io(bool write, uint32_t block, uint32_t offset, void *data, uint32_t len)
{
    uint64_t pos = (uint64_t)block * SECTOR + offset;
    if (!lock || pos + len > (uint64_t)share_blocks() * SECTOR) return -1;
    xSemaphoreTake(lock, portMAX_DELAY);
    esp_err_t err = presented ? ESP_OK : ESP_ERR_INVALID_STATE;
    uint8_t *bytes = data;
    if (!write && err == ESP_OK) err = wl_read(disk, pos, bytes, len);
    for (size_t left = write ? len : 0; left && err == ESP_OK;) {
        uint8_t sector[SECTOR];
        size_t off = pos % SECTOR, count = SECTOR - off;
        if (count > left) count = left;
        if (count != SECTOR) err = wl_read(disk, pos - off, sector, SECTOR);
        if (err == ESP_OK) {
            memcpy(sector + off, bytes, count);
            err = write_sector(pos - off, sector);
        }
        pos += count; bytes += count; left -= count;
    }
    xSemaphoreGive(lock);
    return err == ESP_OK ? (int32_t)len : -1;
}

static const char *command(char *line, char *value)
{
    char *save = NULL;
    char *key = strtok_r(line, " ", &save);
    char *op = strtok_r(NULL, " ", &save);
    char *arg = strtok_r(NULL, " ", &save);
    if (!key || strlen(key) != 16 || !op) return "INVALID";
    if (!strcmp(op, "INFO")) {
        /* First-time wear-levelling setup belongs to the first file request.
         * A regular keyboard connection can finish booting immediately. */
        if (disk == WL_INVALID_HANDLE) {
            if (wl_mount(partition, &disk) != ESP_OK) return "STORAGE_UNAVAILABLE";
            if (wl_sector_size(disk) != SECTOR) {
                wl_unmount(disk);
                disk = WL_INVALID_HANDLE;
                return "STORAGE_UNAVAILABLE";
            }
        }
        snprintf(value, MAX_LINE - 64, "%s,%lu,%s", board, (unsigned long)share_blocks(), presented ? "target" : "local");
        return "OK";
    }
    if (disk == WL_INVALID_HANDLE) return "STORAGE_UNAVAILABLE";
    if (presented) return "EJECT_FIRST";
    int64_t now = esp_timer_get_time();
    if (!strcmp(op, "TAKE")) {
        if (!arg || strcmp(arg, board)) return "WRONG_BOARD";
        if (*lease && now < expires && strcmp(lease, key)) return "BUSY";
        memcpy(lease, key, 17);
    }
    if (strcmp(lease, key) || (strcmp(op,"TAKE") && now >= expires)) return "LEASE_EXPIRED";
    expires = now + 30000000;
    if (!strcmp(op, "TAKE")) return "OK";
    if (!strcmp(op, "PREP")) return usb_hid_enable_exchange() ? "OK" : "INPUT_ACTIVE";
    if (!strcmp(op, "SHOW") || !strcmp(op, "DONE")) {
        if (!strcmp(op, "SHOW") && !usb_hid_enable_exchange()) return "INPUT_ACTIVE";
        presented = !strcmp(op, "SHOW");
        media_changed = presented;
        lease[0] = 0;
        return "OK";
    }
    if (!arg || !*arg) return "INVALID";
    char *end;
    unsigned long block = strtoul(arg, &end, 10);
    if (*end || block >= share_blocks()) return "INVALID";
    size_t address = block * SECTOR;
    uint8_t sector[SECTOR];
    if (!strcmp(op, "R")) {
        if (wl_read(disk, address, sector, SECTOR) != ESP_OK) return "READ_FAILED";
        hex(sector, SECTOR, value);
        return "OK";
    }
    char *data = strtok_r(NULL, " ", &save);
    if (strcmp(op,"W") || !data || strlen(data) != 2 * SECTOR) return "INVALID";
    for (size_t i = 0; i < SECTOR; ++i) {
        int a = nibble(data[2*i]), b = nibble(data[2*i+1]);
        if (a < 0 || b < 0) return "INVALID";
        sector[i] = (a << 4) | b;
    }
    return write_sector(address, sector) == ESP_OK ? "OK" : "WRITE_FAILED";
}

static void dispatch(char *line)
{
    char *crc = strrchr(line, ' ');
    if (!crc || strlen(crc + 1) != 4 || strncmp(line, "K8 ", 3)) return;
    char *end;
    unsigned long expected = strtoul(crc + 1, &end, 16);
    if (*end || proto_crc16((const uint8_t *)line, crc - line) != expected) return;
    *crc = 0;
    if (strcmp(last_request, line)) {
        char *id = line + 3, *body = strchr(id, ' ');
        if (!body || body-id != 16) return;
        strcpy(last_request, line);
        *body++ = 0;
        char value[MAX_LINE - 64] = "-";
        xSemaphoreTake(lock, portMAX_DELAY);
        const char *status = command(body, value);
        xSemaphoreGive(lock);
        int n = snprintf(reply, sizeof(reply), "K8R %s %s %s", id, status, value);
        if (n < 0 || (size_t)n + 7 >= sizeof(reply)) { reply[0] = 0; return; }
        snprintf(reply+n, sizeof(reply)-n, " %04x\n", proto_crc16((uint8_t *)reply, n));
    }
    uart_write_bytes(UART_NUM_0, "\n", 1);
    uart_write_bytes(UART_NUM_0, reply, strlen(reply));
}

static void serial_task(void *unused)
{
    (void)unused;
    static char line[MAX_LINE];
    size_t len = 0;
    bool overflow = false;
    for (;;) {
        uint8_t buf[128];
        int n = uart_read_bytes(UART_NUM_0, buf, sizeof(buf), pdMS_TO_TICKS(50));
        for (int i = 0; i < n; ++i) {
            if (buf[i] == '\n') {
                if (!overflow && len) { line[len] = 0; dispatch(line); }
                len = 0; overflow = false;
            } else if (len < sizeof(line)-1 && buf[i] >= 32 && buf[i] <= 126) {
                line[len++] = buf[i];
            } else { overflow = true; }
        }
    }
}
esp_err_t share_drive_init(void)
{
    partition = esp_partition_find_first(ESP_PARTITION_TYPE_DATA, 0x40, "exchange");
    if (!partition) return ESP_ERR_NOT_FOUND;
    uint8_t uuid[16]; kvm_config_uuid(uuid); hex(uuid, sizeof(uuid), board);
    lock = xSemaphoreCreateMutex();
    if (!lock) return ESP_ERR_NO_MEM;
    const uart_config_t cfg = {
        .baud_rate = 115200,
        .data_bits = UART_DATA_8_BITS,
        .parity = UART_PARITY_DISABLE,
        .stop_bits = UART_STOP_BITS_1,
        .flow_ctrl = UART_HW_FLOWCTRL_DISABLE,
        .source_clk = UART_SCLK_DEFAULT,
    };
    esp_err_t err = uart_param_config(UART_NUM_0, &cfg);
    if (err != ESP_OK) return err;
    err = uart_driver_install(UART_NUM_0, 4096, 2048, 0, NULL, 0);
    if (err != ESP_OK) return err;
    return xTaskCreate(serial_task, "file_share", 6144, NULL, 3, NULL) == pdPASS ? ESP_OK : ESP_ERR_NO_MEM;
}
