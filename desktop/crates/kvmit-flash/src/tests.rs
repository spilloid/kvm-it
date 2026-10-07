use super::*;

const MANIFEST: &str = r#"{
  "flash_settings": {"flash_mode":"dio","flash_size":"16MB","flash_freq":"80m"},
  "flash_files": {"0x0":"bootloader/bootloader.bin","0x10000":"kvm-it-firmware.bin","0x8000":"partition_table/partition-table.bin"},
  "extra_esptool_args": {"chip":"esp32s3"}
}"#;

/// A valid ESP image for `chip`, about `len` bytes: header, one segment, the XOR checksum, the appended SHA-256.
fn esp_image(chip: u16, len: usize) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let payload = len.saturating_sub(24 + 8 + 16 + 32).max(4);
    let mut d = vec![0u8; 24];
    d[0] = IMAGE_MAGIC;
    d[1] = 1;
    d[12..14].copy_from_slice(&chip.to_le_bytes());
    d[23] = 1; // SHA-256 appended
    d.extend(0x3C00_0000u32.to_le_bytes());
    d.extend((payload as u32).to_le_bytes());
    let data: Vec<u8> = (0..payload).map(|i| (i % 251) as u8).collect();
    let xor = data.iter().fold(0xEFu8, |a, &b| a ^ b);
    d.extend(data);
    let checksum_at = (d.len() + 1).next_multiple_of(16) - 1;
    d.resize(checksum_at, 0);
    d.push(xor);
    let hash = Sha256::digest(&d);
    d.extend_from_slice(&hash);
    d
}

/// A valid partition table: entries (type, subtype, offset, size, name), the MD5 entry, erased flash up to 0xC00.
fn table(entries: &[(u8, u8, u32, u32, &str)]) -> Vec<u8> {
    use md5::{Digest, Md5};
    let mut v = Vec::new();
    for &(t, st, off, sz, name) in entries {
        let mut e = vec![0xAA, 0x50, t, st];
        e.extend(off.to_le_bytes());
        e.extend(sz.to_le_bytes());
        let mut n = name.as_bytes().to_vec();
        n.resize(16, 0);
        e.extend(n);
        e.extend(0u32.to_le_bytes());
        v.extend(e);
    }
    let md5 = Md5::digest(&v);
    v.extend([0xEB, 0xEB]);
    v.extend([0xFF; 14]);
    v.extend_from_slice(&md5);
    v.resize(PARTITION_TABLE_BYTES as usize, 0xFF);
    v
}

fn parse(t: &[u8]) -> Result<Vec<Partition>, FlashError> {
    parse_partition_table(t, 16 << 20, true)
}

fn project_table() -> Vec<u8> {
    table(&[(1, 2, 0x9000, 0x6000, "nvs"), (1, 1, 0xf000, 0x1000, "phy_init"), (0, 0, 0x10000, 0x100000, "factory")])
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

fn nvs_table(nvs: (u32, u32), name: &str) -> Vec<u8> {
    table(&[(1, 2, nvs.0, nvs.1, name), (0, 0, 0x10000, 0x100000, "factory")])
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
fn the_committed_release_images_pass_the_strict_checks() {
    // always runs: firmware/release is what every package ships
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../firmware/release");
    let im = Image::from_build_dir(&dir).expect("firmware/release must be a valid image set");
    assert_eq!(im.parts.len(), 4, "bootloader, partition table, app and the boot drive");
    assert_eq!(im.parts[3].name, "ipxe");
    im.check_keeps_settings().unwrap();
    assert_eq!(im.nvs_regions(), vec![(0x9000, 0xf000)]);
    // and its own table is its own installed table
    im.check_matches_installed(&im.parts[1].data).unwrap();
}

#[test]
fn refuses_another_chip_in_the_manifest_or_in_the_image() {
    let m = MANIFEST.replace("esp32s3", "esp32");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("not esp32s3"));
    let m = MANIFEST.replace(r#""extra_esptool_args": {"chip":"esp32s3"}"#, r#""extra_esptool_args": {}"#);
    assert!(Image::from_manifest(&m, files).is_err());
    let r = Image::from_manifest(MANIFEST, |rel| Ok(if rel == "kvm-it-firmware.bin" { esp_image(5, 5000) } else { files(rel)? }));
    assert!(r.unwrap_err().0.contains("another chip"));
}

fn corrupt(f: impl Fn(&mut Vec<u8>)) -> Result<Image, FlashError> {
    Image::from_manifest(MANIFEST, |rel| {
        let mut d = files(rel)?;
        if rel == "kvm-it-firmware.bin" {
            f(&mut d);
        }
        Ok(d)
    })
}

#[test]
fn images_must_be_intact_not_just_shaped_like_images() {
    // a bare header (round 8 F5), a flipped payload byte (checksum + sha), a truncated file, a segment running past the end,
    // a wrong appended hash, bad magic and segment counts
    assert!(corrupt(|d| d.truncate(24)).is_err());
    assert!(corrupt(|d| d[100] ^= 1).unwrap_err().0.contains("checksum mismatch"));
    assert!(corrupt(|d| { let n = d.len(); d.truncate(n - 40) }).is_err());
    assert!(corrupt(|d| d[28..32].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes())).unwrap_err().0.contains("past the end"));
    assert!(corrupt(|d| { let n = d.len(); d[n - 1] ^= 1 }).unwrap_err().0.contains("SHA-256"));
    assert!(corrupt(|d| d[0] = 0).is_err());
    assert!(corrupt(|d| d[1] = 0).is_err());
    assert!(corrupt(|d| d[1] = 99).is_err());
    assert!(corrupt(|d| *d = vec![IMAGE_MAGIC]).is_err());
}

#[test]
fn partition_tables_must_be_valid_the_way_the_bootloader_demands() {
    let good = project_table();
    assert_eq!(parse(&good).unwrap().len(), 3);
    // flipped MD5 (round 8 F5), changed entry, junk after the MD5, bad magic, no MD5 at all, partial entry
    let mut v = good.clone();
    v[16 * 6 + 3] ^= 1; // inside the md5 entry's digest (entry 3 starts at 96; its md5 at +16)
    let mut md5_flip = good.clone();
    md5_flip[96 + 16] ^= 1;
    assert!(parse(&md5_flip).unwrap_err().0.contains("MD5"));
    let mut entry_changed = good.clone();
    entry_changed[8] ^= 0x10;
    assert!(parse(&entry_changed).is_err());
    let mut trailing = good.clone();
    let n = trailing.len();
    trailing[n - 1] = 0;
    assert!(parse(&trailing).unwrap_err().0.contains("after its end"));
    let mut bad_magic = good.clone();
    bad_magic[32] = 0x12;
    assert!(parse(&bad_magic).unwrap_err().0.contains("bad entry"));
    assert!(parse(&[0xFF; 0xC00]).is_err());
    assert!(parse(&good[..100]).is_err());
    let _ = v;
}

#[test]
fn partition_tables_with_bad_ranges_overlaps_or_nvs_names_are_refused() {
    assert!(parse(&table(&[(1, 2, 0x9000, 0, "nvs"), (0, 0, 0x10000, 0x1000, "app")])).is_err(), "zero size");
    assert!(parse(&table(&[(1, 2, 0x9100, 0x1000, "nvs"), (0, 0, 0x10000, 0x1000, "app")])).is_err(), "unaligned");
    assert!(parse(&table(&[(1, 2, 0x9000, 0x7000, "nvs"), (1, 1, 0xf000, 0x1000, "phy")])).unwrap_err().0.contains("overlap"));
    assert!(parse(&table(&[(1, 2, 0x9000, 0x6000, "storage"), (0, 0, 0x10000, 0x1000, "app")])).unwrap_err().0.contains("named"));
    assert!(parse(&table(&[(1, 2, 0x9000, 0x3000, "nvs"), (1, 2, 0xc000, 0x3000, "nvs"), (0, 0, 0x10000, 0x1000, "app")])).is_err(), "two nvs");
    assert!(parse(&table(&[(0, 0, 0x10000, 0x1000, "app")])).is_err(), "no nvs");
}

#[test]
fn the_app_needs_a_partition_at_its_offset_with_room() {
    assert!(image_with(table(&[(1, 2, 0x9000, 0x6000, "nvs")]), 21_088, 0x10000).validate().unwrap_err().0.contains("no app partition"));
    let small = table(&[(1, 2, 0x9000, 0x6000, "nvs"), (0, 0, 0x10000, 0x1000, "factory")]);
    assert!(image_with(small, 21_088, 0x10000).validate().unwrap_err().0.contains("does not fit its partition"));
}

#[test]
fn parts_must_be_aligned_in_range_disjoint_in_sectors_and_the_table_at_0x8000() {
    let m = MANIFEST.replace("16MB", "1MB").replace("0x10000", "0xFFFF0");
    assert!(Image::from_manifest(&m, files).is_err());
    // the table anywhere but 0x8000 (round 8 F7)
    let m = MANIFEST.replace("0x8000", "0x7000");
    assert!(Image::from_manifest(&m, files).unwrap_err().0.contains("0x8000"));
    // unaligned offsets (round 8 F8): a table 0xC00 long at 0x8FFC would share sector 0x9000 with an app at 0x9FFC
    let mut im = image();
    im.parts[1].offset = 0x8FFC;
    im.parts[2].offset = 0x9FFC;
    assert!(im.validate().is_err());
    // sectors really shared: a bootloader longer than 0x8000 runs into the table
    assert!(image_with(project_table(), 0x8100, 0x10000).validate().is_err());
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
    assert!(read_contained(&dir, "sub").is_err());
    assert!(read_contained(&dir, "../outside.bin").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, dir.join("link.bin")).unwrap();
        assert!(read_contained(&dir, "link.bin").unwrap_err().0.contains("outside the firmware folder"));
    }
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_scratch_folder_is_private_unique_and_removed() {
    let (a, b) = (Scratch::new().unwrap(), Scratch::new().unwrap());
    assert_ne!(a.path(), b.path());
    let path = a.path().to_path_buf();
    assert!(path.is_dir());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o700);
    }
    std::fs::write(path.join("x"), [1]).unwrap();
    drop(a);
    assert!(!path.exists());
    assert!(read_limited(&b.path().join("missing"), 1024, "x").is_err());
    assert!(read_limited(b.path(), 1024, "x").is_err(), "a directory is not a file");
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
fn any_espressif_usb_device_blocks_the_adapters_hid_port_and_its_rom_mode_alike() {
    // running firmware: 303a:4008, HID only, invisible to serial-port enumeration (the real board's descriptor)
    assert!(check_no_native_adapter(&[usb(0x303a, 0x4008, Some("kvm-it"), Some("kvm-it HID adapter"))]).unwrap_err().0.contains("would type into this machine"));
    // an adapter held in download mode shows the generic debug unit (round 8 F2): it must block too
    assert!(check_no_native_adapter(&[usb(0x303a, 0x1001, Some("Espressif"), Some("USB JTAG/serial debug unit"))]).is_err());
    // partially readable or unreadable descriptors cannot be ruled out either (round 8 F3)
    assert!(check_no_native_adapter(&[usb(0x303a, 0x1234, Some("Espressif"), None)]).is_err());
    assert!(check_no_native_adapter(&[usb(0x303a, 0x1234, None, None)]).is_err());
}

#[test]
fn unrelated_usb_devices_do_not_block() {
    let devs = [usb(0x1a86, 0x55d3, None, Some("USB Single Serial")), usb(0x046d, 0xc52b, Some("Logitech"), Some("Receiver"))];
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
    let t = table(&[(1, 2, 0x9000, 0x6000, "nvs"), (0, 0, 0x10000, 0x100000, "factory")]);
    assert!(image_with(t.clone(), 21_088, 0x9000).validate().is_err(), "app on top of nvs");
    let mut im = image_with(t, 21_088, 0x10000);
    im.parts[0].data = esp_image(ESP32S3_CHIP_ID, 0x9100); // a bootloader grown past 0x9000
    assert!(im.check_keeps_settings().unwrap_err().0.contains("settings partition"));
}

#[test]
fn a_new_table_that_moves_shrinks_renames_or_changes_nvs_is_refused_against_the_installed_one() {
    let im = image();
    let msg = |t: Vec<u8>| im.check_matches_installed(&t).unwrap_err().0;
    assert!(msg(nvs_table((0xa000, 0x6000), "nvs")).contains("somewhere other"));
    assert!(msg(nvs_table((0x9000, 0x3000), "nvs")).contains("somewhere other"));
    assert!(msg(vec![0xFF; 0xC00]).contains("not valid"));
    assert!(msg(vec![0xAA; 0xC00]).contains("not valid"));
    // swapped labels (round 8 F6): the installed table has nvs at 0x9000 and "storage" at 0x10000... the new one the reverse
    let installed = table(&[(1, 2, 0x9000, 0x6000, "nvs"), (1, 2, 0x20000, 0x6000, "storage"), (0, 0, 0x30000, 0x100000, "factory")]);
    let new = table(&[(1, 2, 0x9000, 0x6000, "storage"), (1, 2, 0x20000, 0x6000, "nvs"), (0, 0, 0x30000, 0x100000, "factory")]);
    assert!(parse(&installed).is_err(), "two nvs partitions are refused outright by the table rules");
    assert!(parse(&new).is_err());
}

#[test]
fn the_hash_flag_is_any_nonzero_and_segments_are_word_sized() {
    // round 9: byte 23 = 2 must still be hash-checked (ESP-IDF treats any nonzero as "hash appended"); an odd segment length is refused
    // the flag byte is inside the hashed header, so changing it 1 -> 2 without recomputing the digest must fail the SHA check
    assert!(corrupt(|d| d[23] = 2).unwrap_err().0.contains("SHA-256"));
    assert!(corrupt(|d| d[23] = 0).is_ok(), "no appended hash: the XOR checksum alone (the trailing digest is then just extra bytes)");
    let mut odd = esp_image(ESP32S3_CHIP_ID, 5000);
    let len = u32::from_le_bytes([odd[28], odd[29], odd[30], odd[31]]);
    odd[28..32].copy_from_slice(&(len - 1).to_le_bytes());
    assert!(check_esp_image(&Part { name: "odd".into(), offset: 0, data: odd }).unwrap_err().0.contains("multiple of 4"));
    // a segment length that would overflow pointer arithmetic is an error, not a panic
    let mut huge = esp_image(ESP32S3_CHIP_ID, 5000);
    huge[28..32].copy_from_slice(&0xFFFF_FFFCu32.to_le_bytes());
    assert!(check_esp_image(&Part { name: "huge".into(), offset: 0, data: huge }).is_err());
}

#[test]
fn a_table_filled_to_its_last_entry_has_no_terminator_and_is_refused() {
    let mut entries: Vec<(u8, u8, u32, u32, String)> = vec![(1, 2, 0x9000, 0x1000, "nvs".into()), (0, 0, 0x10000, 0x10000, "app".into())];
    for i in 0..93u32 {
        entries.push((1, 0x80, 0x20000 + i * 0x1000, 0x1000, format!("d{i}")));
    }
    let refs: Vec<(u8, u8, u32, u32, &str)> = entries.iter().map(|(a, b, c, d, n)| (*a, *b, *c, *d, n.as_str())).collect();
    // 95 entries + the MD5 record fill all 96 slots of 0xC00 bytes
    assert_eq!(refs.len(), 95);
    assert!(parse(&table(&refs)).unwrap_err().0.contains("terminating"));
    assert!(parse(&table(&refs[..94])).is_ok(), "one fewer leaves the erased terminator");
}

#[test]
fn ota_geometry_app_alignment_and_the_flash_size_are_checked() {
    assert!(parse(&table(&[(1, 2, 0x9000, 0x6000, "nvs"), (1, 0, 0xf000, 0x1000, "otadata"), (0, 0x10, 0x10000, 0x1000, "ota_0")])).unwrap_err().0.contains("0x2000"));
    parse(&table(&[(1, 2, 0x9000, 0x4000, "nvs"), (1, 0, 0xd000, 0x2000, "otadata"), (0, 0x10, 0x10000, 0x1000, "ota_0")])).unwrap();
    assert!(parse(&table(&[(1, 2, 0x9000, 0x6000, "nvs"), (0, 0, 0x11000, 0x1000, "factory")])).unwrap_err().0.contains("64 KiB"));
    // bounds come from the image's flash size, not a global maximum
    let t = table(&[(1, 2, 0x9000, 0x6000, "nvs"), (0, 0, 0x10000, 0x100000, "factory")]);
    assert!(parse_partition_table(&t, 1 << 20, true).is_err());
    assert!(parse_partition_table(&t, 2 << 20, true).is_ok());
    // and the image-level check: an app moved to 0x11000 with a matching table entry is refused
    let moved = table(&[(1, 2, 0x9000, 0x6000, "nvs"), (0, 0, 0x11000, 0x100000, "factory")]);
    assert!(image_with(moved, 21_088, 0x11000).validate().is_err());
}

#[test]
fn an_installed_table_without_an_md5_entry_is_accepted_for_the_settings_comparison() {
    // older build options leave out the MD5 record; the settings partition can still be proven identical
    let mut v = Vec::new();
    for e in [(1u8, 2u8, 0x9000u32, 0x6000u32, "nvs"), (0, 0, 0x10000, 0x100000, "factory")] {
        let mut x = vec![0xAA, 0x50, e.0, e.1];
        x.extend(e.2.to_le_bytes());
        x.extend(e.3.to_le_bytes());
        let mut n = e.4.as_bytes().to_vec();
        n.resize(16, 0);
        x.extend(n);
        x.extend(0u32.to_le_bytes());
        v.extend(x);
    }
    v.resize(PARTITION_TABLE_BYTES as usize, 0xFF);
    assert!(parse(&v).is_err(), "a NEW table must carry the MD5");
    image().check_matches_installed(&v).unwrap();
}

#[cfg(unix)]
#[test]
fn a_fifo_is_refused_without_hanging_and_oversize_files_are_errors_not_truncations() {
    let base = std::env::temp_dir().join(format!("kvmit-flash-fifo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let fifo = base.join("pipe.bin");
    assert!(std::process::Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    // would block forever in open() if the type were not checked first
    assert!(read_limited(&fifo, 1024, "pipe").unwrap_err().0.contains("not a regular file"));
    let big = base.join("big.bin");
    std::fs::write(&big, vec![0u8; 2000]).unwrap();
    assert!(read_limited(&big, 1024, "big").unwrap_err().0.contains("larger than"));
    assert_eq!(read_limited(&big, 2000, "big").unwrap().len(), 2000);
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn the_parser_enforces_the_bootloaders_0xc00_window() {
    // round 10: an erased terminator entry *outside* the window does not count; the table itself may not be longer than the window
    let mut t = project_table();
    t.extend([0xFF; 32]);
    assert!(parse(&t).unwrap_err().0.contains("0xC00"));
    assert!(parse(&project_table()).is_ok());
}

fn data_table() -> Vec<u8> {
    table(&[(1, 2, 0x9000, 0x6000, "nvs"), (1, 1, 0xf000, 0x1000, "phy_init"), (0, 0, 0x10000, 0x100000, "factory"), (1, 0x81, 0x110000, 0x400000, "ipxe")])
}

fn boot_image(len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    v[510] = 0x55;
    v[511] = 0xAA;
    v
}

fn four_parts(data_off: u32, data: Vec<u8>) -> Image {
    let mut im = image_with(data_table(), 21_088, 0x10000);
    im.parts.push(Part { name: "ipxe".into(), offset: data_off, data });
    im
}

#[test]
fn a_fat_data_image_in_its_own_partition_is_accepted_and_keeps_the_settings() {
    let im = four_parts(0x110000, boot_image(4 << 20));
    im.validate().unwrap();
    im.check_keeps_settings().unwrap();
    // an installed table that already has the data partition matches, and so does the old three-partition one (the settings are the same)
    im.check_matches_installed(&data_table()).unwrap();
    im.check_matches_installed(&project_table()).unwrap();
}

#[test]
fn a_data_image_is_refused_unless_it_lands_exactly_in_a_fat_data_partition_that_fits() {
    // the rule itself, against a table with every kind of partition, so no other check can be what refuses it
    let t = parse_partition_table(&table(&[
        (1, 2, 0x9000, 0x6000, "nvs"), (1, 1, 0xf000, 0x1000, "phy_init"), (0, 0, 0x10000, 0x100000, "factory"),
        (1, 0, 0x110000, 0x2000, "otadata"), (1, 0x81, 0x120000, 0x400000, "ipxe"), (1, 0x82, 0x520000, 0x10000, "spiffs"),
    ]), 16 << 20, true).unwrap();
    let image = |off: u32, data: Vec<u8>| Part { name: "ipxe".into(), offset: off, data };
    check_data_image(&t, &image(0x120000, boot_image(4 << 20))).unwrap();
    assert!(check_data_image(&t, &image(0x120000, boot_image((4 << 20) + 1))).unwrap_err().0.contains("does not fit"));
    assert!(check_data_image(&t, &image(0x130000, boot_image(4096))).unwrap_err().0.contains("no partition at"), "inside a partition, not at its start");
    assert!(check_data_image(&t, &image(0x120000, vec![0u8; 4096])).unwrap_err().0.contains("55 AA"));
    assert!(check_data_image(&t, &image(0x120000, vec![0x55; 100])).is_err(), "too short to hold a boot sector");
    for (off, why) in [(0x9000, "the settings (nvs: the pairing)"), (0xf000, "phy_init"), (0x10000, "the app partition"), (0x110000, "otadata"), (0x520000, "a non-FAT data partition")] {
        let e = check_data_image(&t, &image(off, boot_image(4096))).unwrap_err().0;
        assert!(e.contains("not a FAT data partition"), "{why}: {e}");
    }
}

#[test]
fn a_four_part_manifest_loads_and_a_misplaced_data_image_is_refused_end_to_end() {
    let manifest = |off: &str| MANIFEST.replace(r#""0x8000":"partition_table/partition-table.bin""#, &format!(r#""0x8000":"partition_table/partition-table.bin","{off}":"ipxe.img""#));
    let read = |rel: &str| -> Result<Vec<u8>, FlashError> {
        match rel {
            "partition_table/partition-table.bin" => Ok(data_table()),
            "ipxe.img" => Ok(boot_image(4 << 20)),
            _ => files(rel),
        }
    };
    let im = Image::from_manifest(&manifest("0x110000"), read).unwrap();
    assert_eq!(im.parts.len(), 4);
    assert_eq!(im.parts[3].name, "ipxe");
    assert!(Image::from_manifest(&manifest("0x120000"), read).is_err());
}

#[test]
fn more_than_one_data_image_or_a_fifth_part_is_refused() {
    let mut im = four_parts(0x110000, boot_image(4096));
    im.parts.push(Part { name: "extra".into(), offset: 0x510000, data: boot_image(4096) });
    assert!(im.validate().unwrap_err().0.contains("3 or 4 parts"));
}

#[test]
fn macos_twin_ports_collapse_to_the_callout_device() {
    let port = |name: &str| PortInfo { kind: PortKind::Uart, description: String::new(), name: name.into(), vid: 0x1a86, pid: 0x55d3, serial: None };
    let got = prefer_callout_ports(vec![port("/dev/tty.usbmodem1101"), port("/dev/cu.usbmodem1101"), port("/dev/tty.other")]);
    let names: Vec<_> = got.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["/dev/cu.usbmodem1101", "/dev/tty.other"], "a tty. without a cu. twin stays");
    let linux = vec![port("/dev/ttyACM0")];
    assert_eq!(prefer_callout_ports(linux.clone()).len(), 1);
}
