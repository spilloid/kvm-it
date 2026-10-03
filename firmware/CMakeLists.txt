cmake_minimum_required(VERSION 3.16)

# Firmware version reported over the serial console now and over BLE later.
# Bump together with CHANGELOG.md and VERSION at the repo root.
set(PROJECT_VER "0.1.0")

# The pinned ESP-IDF must carry patches/esp-idf-v5.5-nimble-conn-handle-index.patch (scripts/fw.sh applies it).
# Without it NimBLE corrupts memory on connect on the ESP32-S3; refuse to build rather than ship that.
file(STRINGS "$ENV{IDF_PATH}/components/bt/host/nimble/nimble/nimble/host/src/ble_hs_conn_priv.h" KVMIT_NIMBLE_PATCHED
     REGEX "bhc_slave_conn_sent")
file(STRINGS "$ENV{IDF_PATH}/components/bt/host/nimble/nimble/nimble/host/src/ble_gap.c" KVMIT_NIMBLE_RAW_ARRAYS
     REGEX "slave_conn\\[|g_max_(tx|rx)_(time|octets)\\[")
if(NOT KVMIT_NIMBLE_PATCHED OR KVMIT_NIMBLE_RAW_ARRAYS)
    message(FATAL_ERROR "ESP-IDF NimBLE is unpatched: build with scripts/fw.sh, or apply firmware/patches/*.patch to $ENV{IDF_PATH}")
endif()

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(kvm-it-firmware)
