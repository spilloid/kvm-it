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
