//! Flashing the adapter from the controller (0.3.0). Everything that decides *whether* and *what* to flash is plain code
//! with tests; only [`flash`] touches a serial port, through the `espflash` library.
//!
//! Safety rules, all enforced here and not left to the UI (review round 7 drove most of them):
//! - Only the board's UART bridge (the "COM" port) is flashable, and the device is re-identified (name *and* USB identity)
//!   at the moment of flashing, because port names such as `/dev/ttyACM0` are reused when cables are swapped.
//! - The adapter's own USB port is a keyboard and mouse toward the *target*; one plugged into the flashing computer would
//!   type into it. That port is HID only, so it is not a serial port: it is found by enumerating USB devices.
//! - What answers on the port must be an ESP32-S3 with the image's flash size; the bridge chip alone proves nothing.
//! - Only the bootloader, partition table and app are written. Unless a full erase is requested, the settings partition
//!   (the Bluetooth bond) must lie outside every erased sector *and* be identical in the installed and the new table.
//! - Files named by the manifest must stay inside the firmware folder, be regular files and be of sane size, and the
//!   images must look like ESP32-S3 images with an app that fits its partition.
use serde::Deserialize;
use std::path::{Component, Path};

pub use espflash::target::ProgressCallbacks;

const IMAGE_MAGIC: u8 = 0xE9;
const CHIP: &str = "esp32s3";
/// `chip_id` field of an ESP image header for the ESP32-S3.
const ESP32S3_CHIP_ID: u16 = 9;
const SECTOR: u32 = 4096;
const MAX_MANIFEST: u64 = 64 * 1024;
const MAX_ENTRIES: usize = 8;
const MAX_FLASH: u32 = 16 * 1024 * 1024;
const PARTITION_TABLE_BYTES: u32 = 0xC00;

/// The USB identity of the board's UART bridge (CH343) and of the ESP32-S3's own USB peripheral.
const UART_VID_PID: (u16, u16) = (0x1a86, 0x55d3);
const ESPRESSIF_VID: u16 = 0x303a;
/// The adapter firmware's own USB product id (a keyboard and mouse), and its descriptor strings.
const ADAPTER_PID: u16 = 0x4008;
const ADAPTER_NAME: &str = "kvm-it";

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

/// Read a file the manifest names: inside `dir` (symlinks resolved), a regular file, no bigger than the flash.
fn read_contained(dir: &Path, rel: &str) -> Result<Vec<u8>, FlashError> {
    check_relative(rel)?;
    let root = dir.canonicalize().or_else(|e| err(format!("{}: {e}", dir.display())))?;
    let path = root.join(rel).canonicalize().or_else(|e| err(format!("{rel}: {e}")))?;
    if !path.starts_with(&root) {
        return err(format!("{rel} resolves outside the firmware folder"));
    }
    let meta = std::fs::metadata(&path).or_else(|e| err(format!("{rel}: {e}")))?;
    if !meta.is_file() {
        return err(format!("{rel} is not a regular file"));
    }
    if meta.len() > MAX_FLASH as u64 {
        return err(format!("{rel} is larger than the flash"));
    }
    std::fs::read(&path).or_else(|e| err(format!("{rel}: {e}")))
}

/// One entry of the partition table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub ptype: u8,
    pub subtype: u8,
    pub offset: u32,
    pub size: u32,
}

/// Parse an ESP-IDF partition table (32-byte entries starting `AA 50`, ended by the MD5 entry or erased flash).
pub fn parse_partition_table(table: &[u8]) -> Vec<Partition> {
    let mut out = Vec::new();
    for e in table.as_chunks::<32>().0 {
        if e[0..2] != [0xAA, 0x50] {
            break;
        }
        out.push(Partition {
            ptype: e[2],
            subtype: e[3],
            offset: u32::from_le_bytes([e[4], e[5], e[6], e[7]]),
            size: u32::from_le_bytes([e[8], e[9], e[10], e[11]]),
        });
    }
    out
}

fn nvs_of(table: &[Partition]) -> Vec<(u32, u32)> {
    table.iter().filter(|p| p.ptype == 1 && p.subtype == 2).map(|p| (p.offset, p.offset.saturating_add(p.size))).collect()
}

impl Image {
    /// Load the image set an ESP-IDF build directory describes (`flasher_args.json`, files relative to it).
    pub fn from_build_dir(dir: &Path) -> Result<Image, FlashError> {
        let manifest = dir.join("flasher_args.json");
        let meta = std::fs::metadata(&manifest).or_else(|e| err(format!("{}: {e} (is this a firmware build directory?)", manifest.display())))?;
        if !meta.is_file() || meta.len() > MAX_MANIFEST {
            return err("flasher_args.json is not a regular file of sane size");
        }
        let json = std::fs::read_to_string(&manifest).or_else(|e| err(format!("{}: {e}", manifest.display())))?;
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

    /// Structure checks: three parts, in range and disjoint; ESP32-S3 image headers; a partition table that has an nvs
    /// partition and an app partition at the app's offset that the app fits in.
    pub fn validate(&self) -> Result<(), FlashError> {
        if self.parts.len() != 3 {
            return err(format!("expected bootloader, partition table and app (3 parts), found {}", self.parts.len()));
        }
        if self.parts[0].offset != 0 {
            return err("the first part must be the bootloader at 0x0");
        }
        for p in &self.parts {
            if p.data.is_empty() {
                return err(format!("{} is empty", p.name));
            }
            if p.offset as u64 + p.data.len().next_multiple_of(4) as u64 > self.flash_bytes as u64 {
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
            check_esp_header(p)?;
        }
        let table_part = &self.parts[1];
        if table_part.data.len() as u32 > PARTITION_TABLE_BYTES {
            return err("the partition table is larger than 0xC00 bytes");
        }
        let table = parse_partition_table(&table_part.data);
        if table.is_empty() {
            return err("the partition table has no entries");
        }
        let app = &self.parts[2];
        let need = app.data.len() as u32;
        match table.iter().find(|p| p.ptype == 0 && p.offset == app.offset) {
            Some(p) if p.size >= need => {}
            Some(p) => return err(format!("the app ({need} bytes) does not fit its partition ({} bytes)", p.size)),
            None => return err(format!("the partition table has no app partition at 0x{:x}", app.offset)),
        }
        Ok(())
    }

    /// The flash regions (start, end) the image's own partition table gives to settings storage (data/nvs): where the
    /// Bluetooth bond lives.
    pub fn nvs_regions(&self) -> Vec<(u32, u32)> {
        nvs_of(&parse_partition_table(&self.parts[1].data))
    }

    /// Check "the pairing and settings are kept" against this very image: no part, rounded up to whole erase sectors, may
    /// touch an nvs partition. Needed only when not erasing everything anyway.
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

    /// The installed partition table (read from the chip) must give the settings the same place as the new one, or the
    /// new firmware would not find the old pairing (or, if shrunk, would erase it on start).
    pub fn check_matches_installed(&self, installed_table: &[u8]) -> Result<(), FlashError> {
        let installed = nvs_of(&parse_partition_table(installed_table));
        if installed.is_empty() {
            return err("the board has no readable partition table (blank or foreign): flash it with a full erase");
        }
        if installed != self.nvs_regions() {
            return err("the new firmware puts the settings (pairing) somewhere other than where the board has them: use a full erase to do that on purpose");
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

/// ESP image header: magic, a plausible segment count, and the chip id of the ESP32-S3.
fn check_esp_header(p: &Part) -> Result<(), FlashError> {
    let d = &p.data;
    if d.len() < 24 || d[0] != IMAGE_MAGIC {
        return err(format!("{} is not an ESP32 image (bad magic byte or too short)", p.name));
    }
    if d[1] == 0 || d[1] > 16 {
        return err(format!("{} is not an ESP32 image (segment count {})", p.name, d[1]));
    }
    let chip = u16::from_le_bytes([d[12], d[13]]);
    if chip != ESP32S3_CHIP_ID {
        return err(format!("{} is built for another chip (id {chip}), not the ESP32-S3", p.name));
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

/// Devices that are, or may be, the adapter's own USB port (its keyboard and mouse). An Espressif device is one when it has
/// the adapter's product id, names itself kvm-it, or its name could not be read (it cannot be ruled out); a different
/// named Espressif board is not.
pub fn adapter_native_ports(devices: &[UsbDevice]) -> Vec<&UsbDevice> {
    devices
        .iter()
        .filter(|d| {
            d.vid == ESPRESSIF_VID
                && (d.pid == ADAPTER_PID
                    || (d.product.is_none() && d.manufacturer.is_none())
                    || [&d.product, &d.manufacturer].iter().any(|s| s.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains(ADAPTER_NAME))))
        })
        .collect()
}

/// Refuse while the adapter's native USB port is plugged into this computer.
pub fn check_no_native_adapter(devices: &[UsbDevice]) -> Result<(), FlashError> {
    match adapter_native_ports(devices).first() {
        None => Ok(()),
        Some(d) => err(format!(
            "an adapter's native USB port ({:04x}:{:04x} {}) is plugged into this computer. It presents a keyboard and mouse and would type into this machine. \
             Unplug that cable and keep only the board's COM port connected.",
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
        let tmp = std::env::temp_dir().join(format!("kvmit-flash-pt-{}.bin", std::process::id()));
        let read = flasher.read_flash(0x8000, PARTITION_TABLE_BYTES, 0x1000, 64, tmp.clone());
        let table = std::fs::read(&tmp);
        let _ = std::fs::remove_file(&tmp);
        read.map_err(|e| fail("could not read the board's partition table", &e))?;
        image.check_matches_installed(&table.map_err(|e| fail("could not read the board's partition table", &e))?)?;
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

#[cfg(test)]
mod tests;
