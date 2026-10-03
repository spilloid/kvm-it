cmake_minimum_required(VERSION 3.16)

# Firmware version reported over the serial console now and over BLE later.
# Bump together with CHANGELOG.md and VERSION at the repo root.
set(PROJECT_VER "0.1.0")

include($ENV{IDF_PATH}/tools/cmake/project.cmake)
project(kvm-it-firmware)
