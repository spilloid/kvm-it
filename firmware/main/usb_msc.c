#include "usb_msc.h"

#include <string.h>

#include "class/msc/msc_device.h"
#include "esp_log.h"
#include "esp_partition.h"
#include "tusb.h"

static const char *TAG = "usb_msc";

#define BLOCK_SIZE 512u

static const esp_partition_t *s_part;
static uint32_t s_blocks;
static uint32_t s_reads, s_bytes, s_refused;

void usb_msc_init(void)
{
    s_part = esp_partition_find_first(ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_DATA_FAT, "ipxe");
    if (!s_part) {
        ESP_LOGW(TAG, "no `ipxe` partition: the boot drive will report no medium");
        return;
    }
    s_blocks = s_part->size / BLOCK_SIZE;
    ESP_LOGI(TAG, "boot drive: %u blocks of %u bytes from partition `ipxe` at 0x%x", (unsigned)s_blocks, BLOCK_SIZE,
             (unsigned)s_part->address);
}

void usb_msc_stats(uint32_t *reads, uint32_t *bytes, uint32_t *refused_writes)
{
    *reads = s_reads;
    *bytes = s_bytes;
    *refused_writes = s_refused;
}

/* What the target did, for the serial log: enumeration and power events, and the first time it asks the drive about itself. */
void tud_mount_cb(void) { ESP_LOGI(TAG, "USB: configured by the target"); }
void tud_umount_cb(void) { ESP_LOGI(TAG, "USB: unconfigured (cable out or target reset)"); }
void tud_suspend_cb(bool remote_wakeup_en) { (void)remote_wakeup_en; ESP_LOGI(TAG, "USB: suspended"); }
void tud_resume_cb(void) { ESP_LOGI(TAG, "USB: resumed"); }

/* ---- TinyUSB MSC callbacks (strong definitions here; esp_tinyusb's own storage module is not linked) ---- */

void tud_msc_inquiry_cb(uint8_t lun, uint8_t vendor_id[8], uint8_t product_id[16], uint8_t product_rev[4])
{
    (void)lun;
    static bool once;
    if (!once) {
        once = true;
        ESP_LOGI(TAG, "target sent SCSI INQUIRY to the boot drive");
    }
    memcpy(vendor_id, "kvm-it  ", 8);
    memcpy(product_id, "iPXE boot drive ", 16);
    memcpy(product_rev, "1.0 ", 4);
}

bool tud_msc_test_unit_ready_cb(uint8_t lun)
{
    if (!s_part) {
        tud_msc_set_sense(lun, SCSI_SENSE_NOT_READY, 0x3A, 0x00); /* medium not present */
        return false;
    }
    return true;
}

void tud_msc_capacity_cb(uint8_t lun, uint32_t *block_count, uint16_t *block_size)
{
    (void)lun;
    static bool once;
    if (!once) {
        once = true;
        ESP_LOGI(TAG, "target asked the boot drive for its capacity");
    }
    *block_count = s_blocks;
    *block_size = BLOCK_SIZE;
}

bool tud_msc_start_stop_cb(uint8_t lun, uint8_t power_condition, bool start, bool load_eject)
{
    (void)lun; (void)power_condition; (void)start; (void)load_eject;
    return true; /* a fixed medium: eject requests are accepted and ignored */
}

bool tud_msc_is_writable_cb(uint8_t lun)
{
    (void)lun;
    return false; /* the host sees a write-protected drive */
}

int32_t tud_msc_read10_cb(uint8_t lun, uint32_t lba, uint32_t offset, void *buffer, uint32_t bufsize)
{
    (void)lun;
    if (!s_part) {
        return -1;
    }
    uint64_t addr = (uint64_t)lba * BLOCK_SIZE + offset;
    if (addr + bufsize > s_part->size) {
        tud_msc_set_sense(lun, SCSI_SENSE_ILLEGAL_REQUEST, 0x21, 0x00); /* logical block address out of range */
        return -1;
    }
    if (esp_partition_read(s_part, (size_t)addr, buffer, bufsize) != ESP_OK) {
        tud_msc_set_sense(lun, SCSI_SENSE_MEDIUM_ERROR, 0x11, 0x00); /* unrecovered read error */
        return -1;
    }
    if (s_reads == 0) {
        ESP_LOGI(TAG, "target is reading the boot drive (first read: lba %u)", (unsigned)lba);
    }
    s_reads++;
    s_bytes += bufsize;
    return (int32_t)bufsize;
}

int32_t tud_msc_write10_cb(uint8_t lun, uint32_t lba, uint32_t offset, uint8_t *buffer, uint32_t bufsize)
{
    (void)lba; (void)offset; (void)buffer; (void)bufsize;
    s_refused++;
    tud_msc_set_sense(lun, SCSI_SENSE_DATA_PROTECT, 0x27, 0x00); /* write protected */
    return -1;
}

int32_t tud_msc_scsi_cb(uint8_t lun, uint8_t const scsi_cmd[16], void *buffer, uint16_t bufsize)
{
    (void)scsi_cmd; (void)buffer; (void)bufsize;
    tud_msc_set_sense(lun, SCSI_SENSE_ILLEGAL_REQUEST, 0x20, 0x00); /* invalid command operation code */
    return -1;
}
