use super::*;

const MANIFEST: &str = r#"{
  "flash_settings": {"flash_mode":"dio","flash_size":"16MB","flash_freq":"80m"},
  "flash_files": {"0x0":"bootloader/bootloader.bin","0x10000":"kvm-it-firmware.bin","0x8000":"partition_table/partition-table.bin"},
  "extra_esptool_args": {"chip":"esp32s3"}
}"#;

fn files(rel: &str) -> Result<Vec<u8>, FlashError> {
    Ok(match rel {
        "bootloader/bootloader.bin" => vec![IMAGE_MAGIC, 1, 2, 3],
        "partition_table/partition-table.bin" => vec![0xAA, 0x50, 0, 0],
        "kvm-it-firmware.bin" => vec![IMAGE_MAGIC; 100],
        _ => return err("missing"),
    })
}

#[test]
fn loads_the_three_parts_in_offset_order() {
    let im = Image::from_manifest(MANIFEST, files).unwrap();
    let got: Vec<_> = im.parts.iter().map(|p| (p.name.as_str(), p.offset)).collect();
    assert_eq!(got, [("bootloader", 0), ("partition-table", 0x8000), ("kvm-it-firmware", 0x10000)]);
    assert_eq!(im.flash_bytes, 16 * 1024 * 1024);
    assert_eq!(im.total_bytes(), 108);
}

#[test]
fn refuses_another_chip() {
    let m = MANIFEST.replace("esp32s3", "esp32");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("not esp32s3"));
}

#[test]
fn refuses_a_missing_chip_field_rather_than_assuming() {
    let m = MANIFEST.replace(r#""extra_esptool_args": {"chip":"esp32s3"}"#, r#""extra_esptool_args": {}"#);
    assert!(Image::from_manifest(&m, files).is_err());
}

#[test]
fn refuses_a_part_that_does_not_fit() {
    let m = MANIFEST.replace("16MB", "1MB").replace("0x10000", "0xFFFF0");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("does not fit"));
}

#[test]
fn refuses_overlapping_parts() {
    let m = MANIFEST.replace("0x8000", "0x2");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("overlaps"));
}

#[test]
fn refuses_a_bootloader_that_is_not_at_zero() {
    let m = MANIFEST.replace(r#""0x0""#, r#""0x1000""#);
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("bootloader at 0x0"));
}

#[test]
fn refuses_files_that_are_not_esp_images() {
    let r = Image::from_manifest(MANIFEST, |rel| Ok(if rel == "kvm-it-firmware.bin" { vec![0u8; 50] } else { files(rel)? }));
    assert!(r.unwrap_err().0.contains("bad magic"));
}

#[test]
fn refuses_empty_and_missing_files_and_wrong_part_counts() {
    assert!(Image::from_manifest(MANIFEST, |rel| Ok(if rel.ends_with("bootloader.bin") { vec![] } else { files(rel)? })).is_err());
    assert!(Image::from_manifest(MANIFEST, |_| err("missing")).is_err());
    let two = MANIFEST.replace(r#","0x8000":"partition_table/partition-table.bin""#, "");
    assert!(Image::from_manifest(&two, files).is_err());
}

#[test]
fn offsets_and_sizes_parse_strictly() {
    assert_eq!(parse_offset("0x10000").unwrap(), 0x10000);
    assert_eq!(parse_offset("4096").unwrap(), 4096);
    assert!(parse_offset("0xZZ").is_err());
    assert_eq!(parse_size("16MB").unwrap(), 16 << 20);
    assert!(parse_size("0MB").is_err() && parse_size("big").is_err());
}

#[test]
fn classifies_ports_by_usb_identity() {
    assert_eq!(classify(0x1a86, 0x55d3), PortKind::Uart);
    assert_eq!(classify(0x303a, 0x1001), PortKind::NativeUsb);
    assert_eq!(classify(0x303a, 0x4001), PortKind::NativeUsb);
    assert_eq!(classify(0x0403, 0x6001), PortKind::Other);
}

fn port(kind: PortKind) -> PortInfo {
    PortInfo { name: "/dev/ttyACM0".into(), kind, description: "x".into() }
}

#[test]
fn the_native_usb_port_is_never_flashable_even_with_any_port() {
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
fn the_real_firmware_build_loads_when_present() {
    // host-tested against this repo's own build output when a build exists (skipped otherwise)
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/build");
    if dir.join("flasher_args.json").exists() {
        let im = Image::from_build_dir(&dir).unwrap();
        assert_eq!(im.parts.len(), 3);
    }
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
    v.resize(v.len().next_multiple_of(32).max(3072), 0xFF);
    v
}

fn image_with(table: Vec<u8>, bootloader_len: usize, app_off: u32) -> Image {
    Image {
        flash_bytes: 16 << 20,
        parts: vec![
            Part { name: "bootloader".into(), offset: 0, data: [vec![IMAGE_MAGIC], vec![0; bootloader_len - 1]].concat() },
            Part { name: "partition-table".into(), offset: 0x8000, data: table },
            Part { name: "app".into(), offset: app_off, data: vec![IMAGE_MAGIC; 600_000] },
        ],
    }
}

#[test]
fn this_projects_layout_keeps_the_settings() {
    let t = table(&[(1, 2, 0x9000, 0x6000), (1, 1, 0xf000, 0x1000), (0, 0, 0x10000, 0x100000)]);
    let im = image_with(t, 21_088, 0x10000);
    assert_eq!(im.nvs_regions().unwrap(), vec![(0x9000, 0xf000)]);
    im.check_keeps_settings().unwrap();
}

#[test]
fn a_part_that_reaches_into_nvs_is_refused() {
    let t = table(&[(1, 2, 0x9000, 0x6000)]);
    // app placed on top of nvs
    assert!(image_with(t.clone(), 21_088, 0x9000).check_keeps_settings().unwrap_err().0.contains("settings partition"));
    // a bootloader grown past 0x9000
    assert!(image_with(t, 0x9100, 0x10000).check_keeps_settings().is_err());
}

#[test]
fn the_partition_table_sector_ends_before_nvs_even_when_rounded_up() {
    let t = table(&[(1, 2, 0x9000, 0x6000)]);
    image_with(t, 21_088, 0x10000).check_keeps_settings().unwrap(); // 0x8000 + 4096 == 0x9000: adjacent, not overlapping
}

#[test]
fn no_nvs_partition_means_it_cannot_be_shown_and_is_refused() {
    let t = table(&[(0, 0, 0x10000, 0x100000)]);
    assert!(image_with(t, 21_088, 0x10000).check_keeps_settings().unwrap_err().0.contains("no nvs partition"));
}

#[test]
fn the_real_build_keeps_the_settings_when_present() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/build");
    if dir.join("flasher_args.json").exists() {
        Image::from_build_dir(&dir).unwrap().check_keeps_settings().unwrap();
    }
}
