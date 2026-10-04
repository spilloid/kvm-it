use super::*;

const MANIFEST: &str = r#"{
  "flash_settings": {"flash_mode":"dio","flash_size":"16MB","flash_freq":"80m"},
  "flash_files": {"0x0":"bootloader/bootloader.bin","0x10000":"kvm-it-firmware.bin","0x8000":"partition_table/partition-table.bin"},
  "extra_esptool_args": {"chip":"esp32s3"}
}"#;

/// An ESP image header (24 bytes) for `chip`, padded to `len`.
fn esp_image(chip: u16, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len.max(24)];
    v[0] = IMAGE_MAGIC;
    v[1] = 3;
    v[12..14].copy_from_slice(&chip.to_le_bytes());
    v
}

fn table(entries: &[(u8, u8, u32, u32)]) -> Vec<u8> {
    let mut v = Vec::new();
    for &(t, st, off, sz) in entries {
        let mut e = vec![0xAA, 0x50, t, st];
        e.extend(off.to_le_bytes());
        e.extend(sz.to_le_bytes());
        e.resize(32, 0);
        v.extend(e);
    }
    v.extend([0xEB, 0xEB]);
    v.resize(PARTITION_TABLE_BYTES as usize, 0xFF);
    v
}

fn project_table() -> Vec<u8> {
    table(&[(1, 2, 0x9000, 0x6000), (1, 1, 0xf000, 0x1000), (0, 0, 0x10000, 0x100000)])
}

fn files(rel: &str) -> Result<Vec<u8>, FlashError> {
    Ok(match rel {
        "bootloader/bootloader.bin" => esp_image(ESP32S3_CHIP_ID, 21_088),
        "partition_table/partition-table.bin" => project_table(),
        "kvm-it-firmware.bin" => esp_image(ESP32S3_CHIP_ID, 596_576),
        _ => return err("missing"),
    })
}

fn image() -> Image {
    Image::from_manifest(MANIFEST, files).unwrap()
}

fn image_with(table: Vec<u8>, bootloader_len: usize, app_off: u32) -> Image {
    Image {
        flash_bytes: 16 << 20,
        parts: vec![
            Part { name: "bootloader".into(), offset: 0, data: esp_image(ESP32S3_CHIP_ID, bootloader_len) },
            Part { name: "partition-table".into(), offset: 0x8000, data: table },
            Part { name: "app".into(), offset: app_off, data: esp_image(ESP32S3_CHIP_ID, 600_000) },
        ],
    }
}

#[test]
fn loads_the_three_parts_in_offset_order() {
    let im = image();
    let got: Vec<_> = im.parts.iter().map(|p| (p.name.as_str(), p.offset)).collect();
    assert_eq!(got, [("bootloader", 0), ("partition-table", 0x8000), ("kvm-it-firmware", 0x10000)]);
    assert_eq!(im.flash_bytes, 16 * 1024 * 1024);
    assert_eq!(im.flash_size_code(), 4);
}

#[test]
fn refuses_another_chip_in_the_manifest_or_in_the_image() {
    let m = MANIFEST.replace("esp32s3", "esp32");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("not esp32s3"));
    let m = MANIFEST.replace(r#""extra_esptool_args": {"chip":"esp32s3"}"#, r#""extra_esptool_args": {}"#);
    assert!(Image::from_manifest(&m, files).is_err());
    // an esp32-c3 build (chip id 5) mislabelled as s3 in the manifest
    let r = Image::from_manifest(MANIFEST, |rel| Ok(if rel == "kvm-it-firmware.bin" { esp_image(5, 5000) } else { files(rel)? }));
    assert!(r.unwrap_err().0.contains("another chip"));
}

#[test]
fn refuses_images_that_cannot_boot() {
    // one byte, wrong magic, absurd segment count
    for bad in [vec![IMAGE_MAGIC], vec![0u8; 5000], { let mut v = esp_image(9, 5000); v[1] = 99; v }, { let mut v = esp_image(9, 5000); v[1] = 0; v }] {
        let r = Image::from_manifest(MANIFEST, |rel| Ok(if rel == "kvm-it-firmware.bin" { bad.clone() } else { files(rel)? }));
        assert!(r.is_err(), "{:?}", &bad[..bad.len().min(4)]);
    }
}

#[test]
fn the_partition_table_must_have_entries_an_app_at_the_apps_offset_and_room() {
    let no_entries = vec![0xFFu8; 0xC00];
    assert!(image_with(no_entries, 21_088, 0x10000).validate().unwrap_err().0.contains("no entries"));
    let no_app = table(&[(1, 2, 0x9000, 0x6000)]);
    assert!(image_with(no_app, 21_088, 0x10000).validate().unwrap_err().0.contains("no app partition"));
    let small = table(&[(1, 2, 0x9000, 0x6000), (0, 0, 0x10000, 0x1000)]);
    assert!(image_with(small, 21_088, 0x10000).validate().unwrap_err().0.contains("does not fit its partition"));
}

#[test]
fn refuses_parts_out_of_range_overlapping_or_sharing_an_erase_sector() {
    let m = MANIFEST.replace("16MB", "1MB").replace("0x10000", "0xFFFF0");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("does not fit"));
    let m = MANIFEST.replace("0x8000", "0x2");
    assert!(Image::from_manifest(&m, files).is_err());
    // bytes disjoint but one erase sector shared: a 0x8100-byte bootloader ends at 0x8100, the table starts at 0x8000
    assert!(image_with(project_table(), 0x8100, 0x10000).validate().is_err());
    // 0x7F00-byte bootloader rounds up to 0x8000: adjacent, fine
    image_with(project_table(), 0x7F00, 0x10000).validate().unwrap();
}

#[test]
fn refuses_a_bootloader_that_is_not_at_zero_wrong_counts_empty_and_missing_files() {
    let m = MANIFEST.replace(r#""0x0""#, r#""0x1000""#);
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("bootloader at 0x0"));
    assert!(Image::from_manifest(MANIFEST, |rel| Ok(if rel.ends_with("bootloader.bin") { vec![] } else { files(rel)? })).is_err());
    assert!(Image::from_manifest(MANIFEST, |_| err("missing")).is_err());
    let two = MANIFEST.replace(r#","0x8000":"partition_table/partition-table.bin""#, "");
    assert!(Image::from_manifest(&two, files).is_err());
}

#[test]
fn a_manifest_cannot_name_files_outside_the_firmware_folder() {
    for evil in ["../secret.bin", "/etc/shadow", "a/../../b.bin", "", "C:\\x.bin"] {
        assert!(check_relative(evil).is_err() || cfg!(windows) && evil == "C:\\x.bin", "{evil}");
    }
    check_relative("bootloader/bootloader.bin").unwrap();
    let m = MANIFEST.replace("kvm-it-firmware.bin", "../kvm-it-firmware.bin");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("outside the firmware folder"));
}

#[test]
fn files_are_read_only_from_inside_the_folder_as_regular_files() {
    let base = std::env::temp_dir().join(format!("kvmit-flash-contain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (dir, outside) = (base.join("fw"), base.join("outside.bin"));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/ok.bin"), [1u8, 2, 3]).unwrap();
    std::fs::write(&outside, [9u8]).unwrap();
    assert_eq!(read_contained(&dir, "sub/ok.bin").unwrap(), vec![1, 2, 3]);
    assert!(read_contained(&dir, "sub").unwrap_err().0.contains("not a regular file"));
    assert!(read_contained(&dir, "../outside.bin").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, dir.join("link.bin")).unwrap();
        assert!(read_contained(&dir, "link.bin").unwrap_err().0.contains("outside the firmware folder"));
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn duplicate_offsets_and_too_many_entries_in_the_manifest_are_errors() {
    let dup = MANIFEST.replace(r#""0x0":"bootloader/bootloader.bin""#, r#""0x0":"bootloader/bootloader.bin","0x0":"kvm-it-firmware.bin""#);
    assert!(Image::from_manifest(&dup, files).unwrap_err().0.contains("duplicate"));
    let many: String = (0..9).map(|i| format!(r#""0x{:x}":"f{i}.bin""#, i * 0x1000)).collect::<Vec<_>>().join(",");
    let m = MANIFEST.replace(r#""0x0":"bootloader/bootloader.bin","0x10000":"kvm-it-firmware.bin","0x8000":"partition_table/partition-table.bin""#, &many);
    assert!(Image::from_manifest(&m, files).is_err());
}

#[test]
fn offsets_and_sizes_parse_strictly() {
    assert_eq!(parse_offset("0x10000").unwrap(), 0x10000);
    assert_eq!(parse_offset("4096").unwrap(), 4096);
    assert!(parse_offset("0xZZ").is_err());
    assert_eq!(parse_size("16MB").unwrap(), 16 << 20);
    assert!(parse_size("0MB").is_err() && parse_size("big").is_err() && parse_size("3MB").is_err() && parse_size("128MB").is_err());
}

#[test]
fn classifies_ports_by_usb_identity() {
    assert_eq!(classify(0x1a86, 0x55d3), PortKind::Uart);
    assert_eq!(classify(0x303a, 0x1001), PortKind::NativeUsb);
    assert_eq!(classify(0x0403, 0x6001), PortKind::Other);
}

fn port(kind: PortKind) -> PortInfo {
    PortInfo { name: "/dev/ttyACM0".into(), kind, description: "x".into(), vid: 0x1a86, pid: 0x55d3, serial: Some("A1".into()) }
}

#[test]
fn a_native_serial_port_is_never_flashable_even_with_any_port() {
    for allow in [false, true] {
        let e = check_port(&port(PortKind::NativeUsb), allow).unwrap_err().0;
        assert!(e.contains("would type into this machine"), "{e}");
    }
}

#[test]
fn the_uart_bridge_is_fine_and_unknown_ports_need_an_explicit_override() {
    assert_eq!(check_port(&port(PortKind::Uart), false).unwrap(), None);
    assert!(check_port(&port(PortKind::Other), false).is_err());
    assert!(check_port(&port(PortKind::Other), true).unwrap().is_some());
}

#[test]
fn the_same_name_with_another_identity_is_a_different_device() {
    let a = port(PortKind::Uart);
    let mut b = a.clone();
    assert!(a.same_device(&b));
    b.serial = Some("B2".into());
    assert!(!a.same_device(&b), "another board under the same port name");
    let mut c = a.clone();
    c.vid = 0x303a;
    assert!(!a.same_device(&c), "a native device under the reused name");
    let mut d = a.clone();
    d.name = "/dev/ttyACM1".into();
    assert!(!a.same_device(&d));
}

fn usb(vid: u16, pid: u16, manufacturer: Option<&str>, product: Option<&str>) -> UsbDevice {
    UsbDevice { vid, pid, manufacturer: manufacturer.map(Into::into), product: product.map(Into::into) }
}

#[test]
fn the_adapters_hid_only_native_port_is_found_by_usb_enumeration() {
    // exactly what the real board shows: 303a:4008, no serial interface, so serial-port enumeration cannot see it
    let adapter = usb(0x303a, 0x4008, Some("kvm-it"), Some("kvm-it HID adapter"));
    assert!(check_no_native_adapter(std::slice::from_ref(&adapter)).unwrap_err().0.contains("would type into this machine"));
    // by name even with another pid, by pid even with no readable name, and unreadable names cannot be ruled out
    assert!(check_no_native_adapter(&[usb(0x303a, 0x1234, None, Some("KVM-IT keyboard"))]).is_err());
    assert!(check_no_native_adapter(&[usb(0x303a, 0x4008, None, None)]).is_err());
    assert!(check_no_native_adapter(&[usb(0x303a, 0x1234, None, None)]).is_err());
}

#[test]
fn unrelated_devices_and_other_named_espressif_boards_do_not_block() {
    let devs = [usb(0x1a86, 0x55d3, None, Some("USB Single Serial")), usb(0x046d, 0xc52b, Some("Logitech"), Some("Receiver")), usb(0x303a, 0x1001, Some("Espressif"), Some("USB JTAG/serial debug unit"))];
    check_no_native_adapter(&devs).unwrap();
    assert!(adapter_native_ports(&devs).is_empty());
}

#[test]
fn this_projects_layout_keeps_the_settings_and_the_installed_table_must_match() {
    let im = image();
    assert_eq!(im.nvs_regions(), vec![(0x9000, 0xf000)]);
    im.check_keeps_settings().unwrap();
    im.check_matches_installed(&project_table()).unwrap();
}

#[test]
fn a_part_that_reaches_into_nvs_is_refused() {
    let t = table(&[(1, 2, 0x9000, 0x6000), (0, 0, 0x9000, 0x100000)]);
    assert!(image_with(t.clone(), 21_088, 0x9000).check_keeps_settings().unwrap_err().0.contains("settings partition"));
    assert!(image_with(table(&[(1, 2, 0x9000, 0x6000), (0, 0, 0x10000, 0x100000)]), 0x9100, 0x10000).check_keeps_settings().is_err());
}

#[test]
fn no_nvs_partition_means_it_cannot_be_shown_and_is_refused() {
    let t = table(&[(0, 0, 0x10000, 0x100000)]);
    assert!(image_with(t, 21_088, 0x10000).check_keeps_settings().unwrap_err().0.contains("no nvs partition"));
}

#[test]
fn a_new_table_that_moves_or_shrinks_nvs_is_refused_against_the_installed_one() {
    let im = image();
    let moved = table(&[(1, 2, 0xa000, 0x6000), (0, 0, 0x10000, 0x100000)]);
    assert!(im.check_matches_installed(&moved).unwrap_err().0.contains("somewhere other"));
    let shrunk = table(&[(1, 2, 0x9000, 0x3000), (0, 0, 0x10000, 0x100000)]);
    assert!(im.check_matches_installed(&shrunk).is_err());
    assert!(im.check_matches_installed(&[0xFF; 0xC00]).unwrap_err().0.contains("no readable partition table"));
}

#[test]
fn the_real_firmware_build_is_valid_and_keeps_settings_when_present() {
    // host-tested against this repo's own build output when a build exists (skipped otherwise)
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/build");
    if dir.join("flasher_args.json").exists() {
        let im = Image::from_build_dir(&dir).unwrap();
        assert_eq!(im.parts.len(), 3);
        im.check_keeps_settings().unwrap();
        assert_eq!(im.nvs_regions(), vec![(0x9000, 0xf000)]);
    }
}
