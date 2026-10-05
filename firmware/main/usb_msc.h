/*
 * The adapter's read-only boot drive: a USB mass-storage interface (third interface of the composite device, after the keyboard
 * and mouse) serving the `ipxe` flash partition, so a target can boot iPXE from the adapter's own USB port and reach the network.
 *
 * Read-only by construction: the drive reports itself write-protected and every write is refused. Nothing the target does to the
 * drive can change the image.
 */
#pragma once

#include <stdint.h>

/* Locate the `ipxe` partition. Without it the drive reports "no medium" and the HID interfaces are unaffected. */
void usb_msc_init(void);

/* Read counters for the serial log and tests: SCSI READ(10) commands served, bytes served, writes refused. */
void usb_msc_stats(uint32_t *reads, uint32_t *bytes, uint32_t *refused_writes);
