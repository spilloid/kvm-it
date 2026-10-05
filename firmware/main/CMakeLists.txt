idf_component_register(
    SRCS "main.c" "usb_hid.c" "hid_selftest.c" "hid_state.c" "ascii_us.c"
         "proto_frame.c" "proto_dispatch.c" "led_pattern.c" "led.c" "button_logic.c" "buttons.c"
         "kvm_config.c" "ble_link.c" "radio_diag.c" "usb_msc.c"
    INCLUDE_DIRS "."
    PRIV_REQUIRES esp_app_format esp_system nvs_flash bt driver esp_timer esp_hw_support esp_partition
)
