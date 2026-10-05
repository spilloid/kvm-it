//! Flashing the adapter from the controller (0.3.0). Everything that decides *whether* and *what* to flash is plain code
//! with tests; only [`flash`] touches a serial port, through the `espflash` library.
//!
//! Safety rules, all enforced here and not left to the UI (review round 7 drove most of them):
//! - Only the board's UART bridge (the "COM" port) is flashable, and the device is re-identified (name *and* USB identity)
//!   at the moment of flashing, because port names such as `/dev/ttyACM0` are reused when cables are swapped.
//! - The adapter's own USB port is a keyboard and mouse toward the *target*; one plugged into the flashing computer would
//!   type into it. That port is HID only (so not a serial port) and in the chip's ROM download mode looks like a generic
//!   Espressif device, so it is found by enumerating USB devices and *any* Espressif (303a) device blocks flashing.
//! - What answers on the port must be an ESP32-S3 with the image's flash size; the bridge chip alone proves nothing.
//! - Only the bootloader, partition table and app are written. Unless a full erase is requested, the settings partition
//!   (the Bluetooth bond) must lie outside every erased sector *and* be identical in the installed and the new table.
//! - Files named by the manifest must stay inside the firmware folder, be regular files and be of sane size, and the
//!   images must look like ESP32-S3 images with an app that fits its partition.
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

pub use espflash::target::ProgressCallbacks;

const IMAGE_MAGIC: u8 = 0xE9;
const CHIP: &str = "esp32s3";
/// `chip_id` field of an ESP image header for the ESP32-S3.
const ESP32S3_CHIP_ID: u16 = 9;
const SECTOR: u32 = 4096;
/// Where the bootloader reads the partition table, and so where the table part must be written and the installed one read.
const PARTITION_TABLE_OFFSET: u32 = 0x8000;
/// Partition type `data` and subtype `fat`: the only kind of partition an optional data image may be written to.
const DATA_TYPE: u8 = 1;
const DATA_FAT_SUBTYPE: u8 = 0x81;
const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 8;
const MAX_FLASH: u32 = 16 * 1024 * 1024;
const PARTITION_TABLE_BYTES: u32 = 0xC00;

/// The USB identity of the board's UART bridge (CH343) and of the ESP32-S3's own USB peripheral.
const UART_VID_PID: (u16, u16) = (0x1a86, 0x55d3);
const ESPRESSIF_VID: u16 = 0x303a;

#[derive(Debug)]
pub struct FlashError(pub String);
impl std::fmt::Display for FlashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FlashError {}
fn err<T>(m: impl Into<String>) -> Result<T, FlashError> {
    Err(FlashError(m.into()))
}

/// One region of flash and what goes in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub name: String,
    pub offset: u32,
    pub data: Vec<u8>,
}

/// A validated set of parts for one flash operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub parts: Vec<Part>,
    pub flash_bytes: u32,
}

/// A JSON object whose duplicate keys are an error (a map would silently keep the last one).
struct StrictMap(Vec<(String, String)>);
impl<'de> Deserialize<'de> for StrictMap {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = StrictMap;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object of offset: file")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<StrictMap, A::Error> {
                let mut v: Vec<(String, String)> = Vec::new();
                while let Some((k, val)) = m.next_entry::<String, String>()? {
                    if v.len() >= MAX_ENTRIES {
                        return Err(serde::de::Error::custom("too many flash_files entries"));
                    }
                    if v.iter().any(|(k2, _)| *k2 == k) {
                        return Err(serde::de::Error::custom(format!("duplicate flash_files key {k}")));
                    }
                    v.push((k, val));
                }
                Ok(StrictMap(v))
            }
        }
        d.deserialize_map(V)
    }
}

#[derive(Deserialize)]
struct FlasherArgs {
    flash_settings: Settings,
    flash_files: StrictMap,
    #[serde(default)]
    extra_esptool_args: Extra,
}
#[derive(Deserialize)]
struct Settings {
    flash_size: String,
}
#[derive(Deserialize, Default)]
struct Extra {
    chip: Option<String>,
}

fn parse_offset(s: &str) -> Result<u32, FlashError> {
    let t = s.trim();
    match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u32::from_str_radix(h, 16),
        None => t.parse(),
    }
    .or_else(|_| err(format!("bad flash offset {s:?}")))
}

fn parse_size(s: &str) -> Result<u32, FlashError> {
    match s.trim().to_ascii_uppercase().strip_suffix("MB").map(str::parse::<u32>) {
        Some(Ok(mb)) if mb.is_power_of_two() && mb <= MAX_FLASH / (1024 * 1024) => Ok(mb * 1024 * 1024),
        _ => err(format!("unsupported flash size {s:?}")),
    }
}

fn part_name(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// A manifest path must be relative and must not climb out (`..`) or start from a root or drive.
fn check_relative(rel: &str) -> Result<(), FlashError> {
    let p = Path::new(rel);
    if rel.is_empty() || p.is_absolute() || p.components().any(|c| !matches!(c, Component::Normal(_) | Component::CurDir)) || rel.contains('\\') && cfg!(unix) {
        return err(format!("the manifest names a file outside the firmware folder: {rel:?}"));
    }
    Ok(())
}

/// Read a regular file of at most `limit` bytes. The type is checked before opening (opening a FIFO would block) and again on the
/// opened handle; the read itself is bounded, so a file that grows after the check cannot lift the limit; too big is an error.
fn read_limited(path: &Path, limit: u64, what: &str) -> Result<Vec<u8>, FlashError> {
    use std::io::Read;
    let before = std::fs::metadata(path).or_else(|e| err(format!("{what}: {e}")))?;
    if !before.is_file() {
        return err(format!("{what} is not a regular file"));
    }
    let file = std::fs::File::open(path).or_else(|e| err(format!("{what}: {e}")))?;
    if !file.metadata().or_else(|e| err(format!("{what}: {e}")))?.is_file() {
        return err(format!("{what} is not a regular file"));
    }
    let mut data = Vec::new();
    file.take(limit + 1).read_to_end(&mut data).or_else(|e| err(format!("{what}: {e}")))?;
    if data.len() as u64 > limit {
        return err(format!("{what} is larger than {limit} bytes"));
    }
    Ok(data)
}

/// Read a file the manifest names: inside `dir` (symlinks resolved), a regular file, no bigger than the flash.
fn read_contained(dir: &Path, rel: &str) -> Result<Vec<u8>, FlashError> {
    check_relative(rel)?;
    let root = dir.canonicalize().or_else(|e| err(format!("{}: {e}", dir.display())))?;
    let path = root.join(rel).canonicalize().or_else(|e| err(format!("{rel}: {e}")))?;
    if !path.starts_with(&root) {
        return err(format!("{rel} resolves outside the firmware folder"));
    }
    read_limited(&path, MAX_FLASH as u64, rel)
}

/// One entry of the partition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub ptype: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
    pub name: String,
    pub flags: u32,
}

/// Parse an ESP-IDF partition table strictly: 32-byte entries (`AA 50`), then (when `require_md5`) the MD5 entry (`EB EB`, ones,
/// then the MD5 of the entries, which is verified), then erased flash, with at least one erased terminator entry inside the table
/// window, as the bootloader requires. A table the board already has may lack the MD5 entry (older build options), so installed
/// tables are parsed with `require_md5 = false`. `flash_bytes` bounds the partitions. Anything else is an error: the bootloader
/// rejects such a table and the board would not boot.
pub fn parse_partition_table(table: &[u8], flash_bytes: u32, require_md5: bool) -> Result<Vec<Partition>, FlashError> {
    use md5::{Digest, Md5};
    if table.len() > PARTITION_TABLE_BYTES as usize {
        return err("the partition table is larger than the 0xC00-byte window the bootloader reads");
    }
    let mut out = Vec::new();
    let mut md5_seen = false;
    let mut terminated = false;
    for (i, e) in table.chunks(32).enumerate() {
        if e.len() < 32 {
            return err("the partition table ends in a partial entry");
        }
        if terminated || md5_seen {
            if e.iter().any(|&b| b != 0xFF) {
                return err("the partition table has data after its end");
            }
            terminated = true;
            continue;
        }
        match e[0..2] {
            [0xAA, 0x50] => {
                let name_bytes = &e[12..28];
                let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(16);
                out.push(Partition {
                    ptype: e[2],
                    subtype: e[3],
                    offset: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
                    size: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
                    name: String::from_utf8_lossy(&name_bytes[..end]).into_owned(),
                    flags: u32::from_le_bytes([e[28], e[29], e[30], e[31]]),
                });
            }
            [0xEB, 0xEB] => {
                if e[2..16].iter().any(|&b| b != 0xFF) || Md5::digest(&table[..i * 32]).as_slice() != &e[16..32] {
                    return err("the partition table's MD5 does not match its entries");
                }
                md5_seen = true;
            }
            _ if e.iter().all(|&b| b == 0xFF) => terminated = true,
            _ => return err(format!("the partition table has a bad entry at index {i}")),
        }
    }
    if require_md5 && !md5_seen {
        return err("the partition table has no MD5 entry");
    }
    // the bootloader needs an erased terminator entry inside the window: a table filled to its last entry (the MD5 record
    // included) is rejected by ESP-IDF
    if !(terminated || table.len() / 32 > out.len() + usize::from(md5_seen)) {
        return err("the partition table has no terminating entry inside its 0xC00 window");
    }
    check_partitions(&out, flash_bytes)?;
    Ok(out)
}

fn check_partitions(t: &[Partition], flash_bytes: u32) -> Result<(), FlashError> {
    if t.is_empty() {
        return err("the partition table has no entries");
    }
    let mut spans: Vec<(u32, u32, &str)> = Vec::new();
    for p in t {
        if p.size == 0 || p.offset % SECTOR != 0 || p.offset as u64 + p.size as u64 > flash_bytes as u64 {
            return err(format!("partition {:?} has a bad range (offset 0x{:x}, size 0x{:x}) for {} MB of flash", p.name, p.offset, p.size, flash_bytes / (1024 * 1024)));
        }
        // ESP-IDF: app partitions sit on 64 KiB boundaries (the flash cache maps whole 64 KiB pages); the OTA selection
        // partition (data/ota) is two sectors
        if p.ptype == 0 && p.offset % 0x1_0000 != 0 {
            return err(format!("app partition {:?} is not on a 64 KiB boundary", p.name));
        }
        if p.ptype == 1 && p.subtype == 0 && p.size != 0x2000 {
            return err(format!("the OTA data partition {:?} must be 0x2000 bytes", p.name));
        }
        spans.push((p.offset, p.offset + p.size, &p.name));
    }
    spans.sort();
    for w in spans.windows(2) {
        if w[0].1 > w[1].0 {
            return err(format!("partitions {:?} and {:?} overlap", w[0].2, w[1].2));
        }
    }
    let nvs = t.iter().filter(|p| p.ptype == 1 && p.subtype == 2).count();
    if nvs != 1 || !t.iter().any(|p| p.ptype == 1 && p.subtype == 2 && p.name == "nvs") {
        return err("the partition table needs exactly one nvs partition, named \"nvs\" (the name the firmware opens)");
    }
    Ok(())
}

/// The settings partition as it must be identical before and after: where, how big, what it is called, its flags.
fn nvs_of(table: &[Partition]) -> Vec<&Partition> {
    table.iter().filter(|p| p.ptype == 1 && p.subtype == 2).collect()
}

impl Image {
    /// Load the image set an ESP-IDF build directory describes (`flasher_args.json`, files relative to it).
    pub fn from_build_dir(dir: &Path) -> Result<Image, FlashError> {
        let manifest = dir.join("flasher_args.json");
        if !manifest.exists() {
            return err(format!("{}: not found (is this a firmware folder?)", manifest.display()));
        }
        let json = String::from_utf8(read_limited(&manifest, MAX_MANIFEST, "flasher_args.json")?).or_else(|_| err("flasher_args.json is not UTF-8"))?;
        Self::from_manifest(&json, |rel| read_contained(dir, rel))
    }

    /// The pure part of loading: a manifest and a way to read its files.
    pub fn from_manifest(json: &str, read: impl Fn(&str) -> Result<Vec<u8>, FlashError>) -> Result<Image, FlashError> {
        let m: FlasherArgs = serde_json::from_str(json).or_else(|e| err(format!("flasher_args.json: {e}")))?;
        if m.extra_esptool_args.chip.as_deref() != Some(CHIP) {
            return err(format!("this image is for {:?}, not {CHIP}", m.extra_esptool_args.chip));
        }
        let flash_bytes = parse_size(&m.flash_settings.flash_size)?;
        let mut parts = Vec::new();
        for (off, rel) in &m.flash_files.0 {
            check_relative(rel)?;
            parts.push(Part { name: part_name(rel), offset: parse_offset(off)?, data: read(rel)? });
        }
        parts.sort_by_key(|p| p.offset);
        let image = Image { parts, flash_bytes };
        image.validate()?;
        Ok(image)
    }

    /// Structure checks: three parts at supported, sector-aligned places; ESP32-S3 images that are intact (segments, checksum,
    /// SHA-256); a valid partition table at 0x8000 with one `nvs` partition and an app partition at the app's offset that the
    /// app fits in.
    pub fn validate(&self) -> Result<(), FlashError> {
        if !(3..=4).contains(&self.parts.len()) {
            return err(format!("expected bootloader, partition table and app, plus at most one data image (3 or 4 parts), found {}", self.parts.len()));
        }
        if self.parts[0].offset != 0 {
            return err("the first part must be the bootloader at 0x0");
        }
        if self.parts[1].offset != PARTITION_TABLE_OFFSET {
            return err(format!("the partition table must be at 0x{PARTITION_TABLE_OFFSET:x}, where the bootloader reads it"));
        }
        for p in &self.parts {
            if p.offset % SECTOR != 0 {
                return err(format!("{} is not aligned to an erase sector (4 KiB)", p.name));
            }
            if p.data.is_empty() {
                return err(format!("{} is empty", p.name));
            }
            if p.offset as u64 + p.data.len().next_multiple_of(SECTOR as usize) as u64 > self.flash_bytes as u64 {
                return err(format!("{} does not fit in {} MB of flash", p.name, self.flash_bytes / (1024 * 1024)));
            }
        }
        for w in self.parts.windows(2) {
            // sector footprints, not just bytes: two parts must not share an erase sector
            let end = w[0].offset as u64 + (w[0].data.len() as u64).next_multiple_of(SECTOR as u64);
            if end > w[1].offset as u64 {
                return err(format!("{} overlaps {} (they would share an erase sector)", w[0].name, w[1].name));
            }
        }
        for p in [&self.parts[0], &self.parts[2]] {
            check_esp_image(p)?;
        }
        let table_part = &self.parts[1];
        if table_part.data.len() as u32 > PARTITION_TABLE_BYTES {
            return err("the partition table is larger than 0xC00 bytes");
        }
        let table = parse_partition_table(&table_part.data, self.flash_bytes, true)?;
        let app = &self.parts[2];
        let need = app.data.len() as u32;
        match table.iter().find(|p| p.ptype == 0 && p.offset == app.offset) {
            Some(p) if p.size >= need => {}
            Some(p) => return err(format!("the app ({need} bytes) does not fit its partition ({} bytes)", p.size)),
            None => return err(format!("the partition table has no app partition at 0x{:x}", app.offset)),
        }
        for p in &self.parts[3..] {
            check_data_image(&table, p)?;
        }
        Ok(())
    }

    /// The new image's partition table (valid, because [`Image::validate`] passed).
    fn table(&self) -> Result<Vec<Partition>, FlashError> {
        parse_partition_table(&self.parts[1].data, self.flash_bytes, true)
    }

    /// The flash regions (start, end) the image's own partition table gives to the settings (data/nvs): where the pairing lives.
    pub fn nvs_regions(&self) -> Vec<(u32, u32)> {
        self.table().map(|t| nvs_of(&t).iter().map(|p| (p.offset, p.offset + p.size)).collect()).unwrap_or_default()
    }

    /// Check "the pairing and settings are kept" against this very image: no part, rounded out to whole erase sectors, may touch
    /// an nvs partition. Needed only when not erasing everything anyway.
    pub fn check_keeps_settings(&self) -> Result<(), FlashError> {
        let nvs = self.nvs_regions();
        if nvs.is_empty() {
            return err("the image's partition table has no nvs partition, so it cannot be shown that the pairing is kept: use a full erase");
        }
        for p in &self.parts {
            let (start, end) = (p.offset, p.offset.saturating_add((p.data.len() as u32).next_multiple_of(SECTOR)));
            for &(ns, ne) in &nvs {
                if start < ne && ns < end {
                    return err(format!("{} would overwrite the settings partition (0x{ns:x}-0x{ne:x}), erasing the pairing: use a full erase to do that on purpose", p.name));
                }
            }
        }
        Ok(())
    }

    /// The installed partition table (read from the chip) must give the settings exactly the same place, size, name and flags as
    /// the new one, or the new firmware would not find the old pairing (or, if shrunk, would erase it on start).
    pub fn check_matches_installed(&self, installed_table: &[u8]) -> Result<(), FlashError> {
        let installed = parse_partition_table(installed_table, self.flash_bytes, false).or_else(|e| {
            err(format!("the board's partition table is not valid ({e}): flash it with a full erase"))
        })?;
        let new = self.table()?;
        if nvs_of(&installed) != nvs_of(&new) {
            return err("the new firmware puts the settings (pairing) somewhere other than where the board has them, or under another name: use a full erase to do that on purpose");
        }
        Ok(())
    }

    pub fn total_bytes(&self) -> usize {
        self.parts.iter().map(|p| p.data.len()).sum()
    }

    /// The flash size as espflash's header code (1 MB = 0, 2 MB = 1, ... 16 MB = 4).
    fn flash_size_code(&self) -> u8 {
        (self.flash_bytes / (1024 * 1024)).trailing_zeros() as u8
    }
}

/// An optional data image (the adapter's read-only boot drive): it must be written exactly into a FAT data partition of the table (never the
/// settings, `phy_init`, an OTA partition or anything else), fit it, and look like a disk or filesystem image (boot signature 55 AA).
fn check_data_image(table: &[Partition], p: &Part) -> Result<(), FlashError> {
    match table.iter().find(|t| t.offset == p.offset) {
        Some(t) if t.ptype == DATA_TYPE && t.subtype == DATA_FAT_SUBTYPE => {
            if p.data.len() as u64 > t.size as u64 {
                return err(format!("{} ({} bytes) does not fit its partition {:?} ({} bytes)", p.name, p.data.len(), t.name, t.size));
            }
        }
        Some(t) => return err(format!("{} would be written to partition {:?}, which is not a FAT data partition: only the app and one FAT data image are ever written", p.name, t.name)),
        None => return err(format!("the partition table has no partition at 0x{:x} for {}", p.offset, p.name)),
    }
    if p.data.len() < 512 || p.data[510] != 0x55 || p.data[511] != 0xAA {
        return err(format!("{} is not a disk or filesystem image (no 55 AA boot signature)", p.name));
    }
    Ok(())
}

/// An ESP image, walked the way the ROM bootloader reads it: header (magic, segment count, the ESP32-S3 chip id), each segment
/// inside the file, the XOR checksum byte, and the appended SHA-256 when the header says there is one.
fn check_esp_image(p: &Part) -> Result<(), FlashError> {
    use sha2::{Digest, Sha256};
    let d = &p.data;
    let bad = |why: &str| err(format!("{} is not an intact ESP32-S3 image: {why}", p.name));
    if d.len() < 24 || d[0] != IMAGE_MAGIC {
        return bad("bad magic byte or too short");
    }
    let segments = d[1] as usize;
    if segments == 0 || segments > 16 {
        return bad(&format!("segment count {segments}"));
    }
    let chip = u16::from_le_bytes([d[12], d[13]]);
    if chip != ESP32S3_CHIP_ID {
        return err(format!("{} is built for another chip (id {chip}), not the ESP32-S3", p.name));
    }
    let mut pos = 24usize;
    let mut xor = 0xEFu8;
    for _ in 0..segments {
        let Some(h) = d.get(pos..pos.saturating_add(8)).filter(|h| h.len() == 8) else { return bad("truncated segment header") };
        let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as usize;
        if !len.is_multiple_of(4) {
            return bad("a segment length is not a multiple of 4");
        }
        let Some(end) = (pos + 8).checked_add(len) else { return bad("a segment runs past the end of the file") };
        let Some(data) = d.get(pos + 8..end) else { return bad("a segment runs past the end of the file") };
        xor = data.iter().fold(xor, |a, &b| a ^ b);
        pos = end;
    }
    let checksum_at = (pos + 1).next_multiple_of(16) - 1;
    match d.get(checksum_at) {
        Some(&c) if c == xor => {}
        Some(_) => return bad("checksum mismatch"),
        None => return bad("truncated before the checksum"),
    }
    let end = checksum_at + 1;
    if d[23] != 0 {
        let Some(hash) = d.get(end..end + 32) else { return bad("truncated before the appended SHA-256") };
        if Sha256::digest(&d[..end]).as_slice() != hash {
            return bad("appended SHA-256 mismatch");
        }
    }
    Ok(())
}

/// What a serial port is, as far as flashing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    /// The board's UART bridge: the one to flash through.
    Uart,
    /// An Espressif native-USB serial port (not flashable from here; see [`adapter_native_ports`]).
    NativeUsb,
    /// Anything else (could be another board, a modem, a serial console).
    Other,
}

pub fn classify(vid: u16, pid: u16) -> PortKind {
    if (vid, pid) == UART_VID_PID {
        PortKind::Uart
    } else if vid == ESPRESSIF_VID {
        PortKind::NativeUsb
    } else {
        PortKind::Other
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub name: String,
    pub kind: PortKind,
    pub description: String,
    pub vid: u16,
    pub pid: u16,
    /// The USB serial number, when the bridge reports one: with the ids it pins down one physical device.
    pub serial: Option<String>,
}

impl PortInfo {
    fn same_device(&self, other: &PortInfo) -> bool {
        self.name == other.name && self.vid == other.vid && self.pid == other.pid && self.serial == other.serial
    }
}

/// USB serial ports on this computer, with the board's UART bridge first.
pub fn list_ports() -> Vec<PortInfo> {
    let mut v: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => Some(PortInfo {
                kind: classify(u.vid, u.pid),
                description: format!("{:04x}:{:04x} {}", u.vid, u.pid, u.product.clone().unwrap_or_default()).trim().to_string(),
                name: p.port_name,
                vid: u.vid,
                pid: u.pid,
                serial: u.serial_number,
            }),
            _ => None,
        })
        .collect();
    v.sort_by_key(|p| p.kind != PortKind::Uart);
    v
}

/// A USB device on the bus, whatever it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbDevice {
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

pub fn usb_devices() -> Result<Vec<UsbDevice>, FlashError> {
    use nusb::MaybeFuture;
    let it = nusb::list_devices().wait().or_else(|e| err(format!("could not list USB devices: {e}")))?;
    Ok(it
        .map(|d| UsbDevice {
            vid: d.vendor_id(),
            pid: d.product_id(),
            manufacturer: d.manufacturer_string().map(str::to_string),
            product: d.product_string().map(str::to_string),
        })
        .collect())
}

/// Espressif USB devices (vendor 303a) on the bus. Any of them may be an adapter's own USB port: running firmware shows as a
/// keyboard and mouse (`303a:4008 (303a:400a since firmware 0.2.0)`, "kvm-it"), a chip held in download mode as a generic "USB JTAG/serial debug unit"
/// (`303a:1001`) that cannot be told from a developer board by its descriptor. Flashing the chip through its COM port restarts
/// it into the keyboard and mouse, so every one of them blocks flashing. (Limits: a device the operating system will not let
/// the enumerator describe can be missing from the list, and a cable plugged in after the check is not seen.)
pub fn adapter_native_ports(devices: &[UsbDevice]) -> Vec<&UsbDevice> {
    devices.iter().filter(|d| d.vid == ESPRESSIF_VID).collect()
}

/// Refuse while an Espressif USB device (possibly an adapter's own USB port) is plugged into this computer.
pub fn check_no_native_adapter(devices: &[UsbDevice]) -> Result<(), FlashError> {
    match adapter_native_ports(devices).first() {
        None => Ok(()),
        Some(d) => err(format!(
            "an Espressif USB device ({:04x}:{:04x} {}) is plugged into this computer. If it is an adapter's own USB port (the one that is not COM) it presents a keyboard and mouse \
             and would type into this machine. Unplug it, and keep only the board's COM port connected.",
            d.vid,
            d.pid,
            d.product.clone().unwrap_or_default()
        )),
    }
}

/// Refuse a port that must not be flashed through; `Ok` carries a warning to show when it is merely unrecognised.
pub fn check_port(info: &PortInfo, allow_other: bool) -> Result<Option<String>, FlashError> {
    match info.kind {
        PortKind::Uart => Ok(None),
        PortKind::NativeUsb => err(format!(
            "{} is an Espressif native USB port ({}). Do not flash through it, and do not leave an adapter's USB port plugged into this computer: \
             it presents a keyboard and mouse and would type into this machine. Use the board's other USB port, the one labelled COM.",
            info.name, info.description
        )),
        PortKind::Other if allow_other => Ok(Some(format!("{} ({}) is not a known kvm-it UART bridge; flashing anyway as asked", info.name, info.description))),
        PortKind::Other => err(format!("{} ({}) is not the adapter's UART bridge (1a86:55d3); pass --any-port to flash it anyway", info.name, info.description)),
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Erase the whole chip first. This also erases the Bluetooth bond and settings; the default keeps them.
    pub erase_all: bool,
    /// Accept a USB serial port that is not a known adapter UART (a native port is still refused).
    pub allow_other_port: bool,
    /// Flash at this baud rate once connected (default 460800).
    pub baud: Option<u32>,
}

/// What was flashed.
#[derive(Debug, Clone)]
pub struct Report {
    pub mac: Option<String>,
}

/// Write `image` through `expected` (the port the person chose and confirmed). Everything is re-checked first: the port is
/// looked up again and must be the same device, no adapter native port may be attached, and the chip that answers must be
/// an ESP32-S3 of the image's flash size, not in secure download mode.
pub fn flash(expected: &PortInfo, image: &Image, opts: Options, progress: &mut dyn ProgressCallbacks) -> Result<Report, FlashError> {
    use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
    use espflash::flasher::Flasher;
    use espflash::image_format::Segment;
    use espflash::target::Chip;
    image.validate()?;
    if !opts.erase_all {
        image.check_keeps_settings()?;
    }
    check_no_native_adapter(&usb_devices()?)?;
    check_port(expected, opts.allow_other_port)?;
    let current = list_ports().into_iter().find(|p| p.name == expected.name);
    match &current {
        Some(c) if c.same_device(expected) => {}
        Some(_) => return err(format!("{} is now a different device than the one you chose: choose it again", expected.name)),
        None => return err(format!("{} is gone: plug the board in and choose it again", expected.name)),
    }
    let fail = |what: &str, e: &dyn std::fmt::Display| FlashError(format!("{what}: {e}"));
    let serial = serialport::new(&expected.name, 115_200)
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(|e| fail(&format!("could not open {}", expected.name), &e))?;
    let usb = serialport::UsbPortInfo { vid: expected.vid, pid: expected.pid, serial_number: expected.serial.clone(), manufacturer: None, product: None };
    let baud = opts.baud.unwrap_or(460_800);
    let conn = Connection::new(serial, usb, ResetAfterOperation::HardReset, ResetBeforeOperation::DefaultReset, 115_200);
    let mut flasher = Flasher::connect(conn, true, true, false, None, Some(baud)).map_err(|e| fail("could not connect to the chip (hold BOOT while plugging in the COM cable if it does not enter the bootloader by itself)", &e))?;
    if flasher.secure_download_mode() {
        return err("the chip is in secure download mode, where writes cannot be verified: refusing");
    }
    let info = flasher.device_info().map_err(|e| fail("could not read the chip's identity", &e))?;
    if info.chip != Chip::Esp32s3 {
        return err(format!("the chip on {} is an {}, not an ESP32-S3: not an adapter, nothing was written", expected.name, info.chip));
    }
    if info.flash_size.encode_flash_size().ok() != Some(image.flash_size_code()) {
        return err(format!("the board's flash size does not match the firmware's ({} MB): not the expected board, nothing was written", image.flash_bytes / (1024 * 1024)));
    }
    if !opts.erase_all {
        // read the installed partition table and require the settings to stay where they are
        let scratch = Scratch::new().map_err(|e| fail("could not make a private scratch folder", &e))?;
        let file = scratch.path().join("partition-table.bin");
        flasher.read_flash(PARTITION_TABLE_OFFSET, PARTITION_TABLE_BYTES, 0x1000, 64, file.clone()).map_err(|e| fail("could not read the board's partition table", &e))?;
        let table = read_limited(&file, 64 * 1024, "the board's partition table").map_err(|e| fail("could not read the board's partition table", &e))?;
        drop(scratch);
        image.check_matches_installed(&table)?;
    } else {
        flasher.erase_flash().map_err(|e| fail("erase failed", &e))?;
    }
    // One session for all parts: `write_bin_to_flash` resets the chip after every call, so a second part would go to a
    // restarting chip. Data is padded to a multiple of 4 with 0xFF (erased flash) as the library requires.
    let segments: Vec<Segment<'_>> = image
        .parts
        .iter()
        .map(|p| {
            let mut data = p.data.clone();
            data.resize(data.len().next_multiple_of(4), 0xFF);
            Segment { addr: p.offset, data: std::borrow::Cow::Owned(data) }
        })
        .collect();
    flasher.write_bins_to_flash(&segments, progress).map_err(|e| fail("writing the firmware failed", &e))?;
    Ok(Report { mac: info.mac_address })
}

/// A private, freshly created folder (owner-only on Unix, created exclusively so nothing can be pre-planted in it), removed on
/// drop. espflash writes the bytes it reads to a path, so that path must not be one another user can choose or swap.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> std::io::Result<Scratch> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = std::env::temp_dir().join(format!("kvmit-flash-{}-{nanos}", std::process::id()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new().mode(0o700).create(&path)?; // fails if it exists
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&path)?; // fails if it exists; the per-user temp folder is private on Windows
        Ok(Scratch(path))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests;
