/* Radio diagnostics (CONFIG_KVMIT_RADIO_DIAG, off by default; build with scripts/fw.sh build-diag).
 *
 * Runs once at BLE sync, before advertising:
 *  - logs every stored bond (peer identity address) so we know who the board trusts;
 *  - passively scans for CONFIG_KVMIT_RADIO_DIAG_SCAN_S seconds and prints one line per advertiser with
 *    report count and min/avg/max RSSI. Any advert whose data contains the marker "KVMITDIAG" is flagged,
 *    so a host advertising that marker (see docs/dev-process.md, "BLE radio diagnostics") gives the
 *    host->board path loss between exactly these two radios.
 * Diagnostic builds only: the observer role is not part of the shipped firmware. */
#pragma once

typedef void (*radio_diag_done_fn)(void);

/* Call from the NimBLE sync callback. Invokes done() on the host task when the scan ends (or fails). */
void radio_diag_run(radio_diag_done_fn done);
