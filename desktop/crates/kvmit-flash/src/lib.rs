//! Flashing the adapter from the controller (0.3.0). Everything that decides *whether* and *what* to flash is plain code
//! with tests; only [`flash`] touches a serial port, through the `espflash` library.
//!
//! Safety rules, all enforced here and not left to the UI:
//! - Only the board's UART bridge (the "COM" port) is flashable. The native USB port is the adapter's HID keyboard/mouse
//!   toward the *target*; a board plugged into the flashing computer through it would type into that computer.
//! - Only the three images the firmware build produces are written (bootloader, partition table, app). The settings
//!   partition that holds the Bluetooth bond is never touched unless a full erase is asked for explicitly.
//! - The image is validated before the port is opened.
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use espflash::target::ProgressCallbacks;

/// Espressif's ROM bootloader writes this byte first in every valid image.
const IMAGE_MAGIC: u8 = 0xE9;
const CHIP: &str = "esp32s3";

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

#[derive(Deserialize)]
struct FlasherArgs {
    flash_settings: Settings,
    flash_files: BTreeMap<String, String>,
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
        Some(Ok(mb)) if (1..=64).contains(&mb) => Ok(mb * 1024 * 1024),
        _ => err(format!("unsupported flash size {s:?}")),
    }
}

fn part_name(path: &str) -> String {
    Path::new(path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

impl Image {
    /// Load the image set an ESP-IDF build directory describes (`flasher_args.json`, files relative to it).
    pub fn from_build_dir(dir: &Path) -> Result<Image, FlashError> {
        let json = std::fs::read_to_string(dir.join("flasher_args.json"))
            .or_else(|e| err(format!("{}: {e} (is this a firmware build directory?)", dir.join("flasher_args.json").display())))?;
        Self::from_manifest(&json, |rel| {
            let p: PathBuf = dir.join(rel);
            std::fs::read(&p).or_else(|e| err(format!("{}: {e}", p.display())))
        })
    }

    /// The pure part of loading: a manifest and a way to read its files.
    pub fn from_manifest(json: &str, read: impl Fn(&str) -> Result<Vec<u8>, FlashError>) -> Result<Image, FlashError> {
        let m: FlasherArgs = serde_json::from_str(json).or_else(|e| err(format!("flasher_args.json: {e}")))?;
        if m.extra_esptool_args.chip.as_deref() != Some(CHIP) {
            return err(format!("this image is for {:?}, not {CHIP}", m.extra_esptool_args.chip));
        }
        let flash_bytes = parse_size(&m.flash_settings.flash_size)?;
        let mut parts = Vec::new();
        for (off, rel) in &m.flash_files {
            parts.push(Part { name: part_name(rel), offset: parse_offset(off)?, data: read(rel)? });
        }
        parts.sort_by_key(|p| p.offset);
        let image = Image { parts, flash_bytes };
        image.validate()?;
        Ok(image)
    }

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
            if p.offset as u64 + p.data.len() as u64 > self.flash_bytes as u64 {
                return err(format!("{} does not fit in {} MB of flash", p.name, self.flash_bytes / (1024 * 1024)));
            }
        }
        for w in self.parts.windows(2) {
            if w[0].offset as u64 + w[0].data.len() as u64 > w[1].offset as u64 {
                return err(format!("{} overlaps {}", w[0].name, w[1].name));
            }
        }
        // bootloader and app are ESP images; the partition table is its own format
        for p in [&self.parts[0], &self.parts[2]] {
            if p.data[0] != IMAGE_MAGIC {
                return err(format!("{} is not an ESP32 image (bad magic byte)", p.name));
            }
        }
        Ok(())
    }

    pub fn total_bytes(&self) -> usize {
        self.parts.iter().map(|p| p.data.len()).sum()
    }
}

/// What a serial port is, as far as flashing is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortKind {
    /// The board's UART bridge: the one to flash through.
    Uart,
    /// The ESP32-S3's own USB: the adapter's keyboard/mouse toward a target. Never flash through it from here.
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
}

/// USB serial ports on this computer, with the board's UART bridge first.
pub fn list_ports() -> Vec<PortInfo> {
    let mut v: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => Some(PortInfo {
                kind: classify(u.vid, u.pid),
                description: format!("{:04x}:{:04x} {}", u.vid, u.pid, u.product.unwrap_or_default()).trim().to_string(),
                name: p.port_name,
            }),
            _ => None,
        })
        .collect();
    v.sort_by_key(|p| p.kind != PortKind::Uart);
    v
}

/// Refuse a port that must not be flashed through; `Ok` carries a warning to show when it is merely unrecognised.
pub fn check_port(info: &PortInfo, allow_other: bool) -> Result<Option<String>, FlashError> {
    match info.kind {
        PortKind::Uart => Ok(None),
        PortKind::NativeUsb => err(format!(
            "{} is the adapter's native USB port ({}). Do not flash through it, and do not leave it plugged into this computer: \
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
    /// Flash at this baud rate once connected (default 460800).
    pub baud: Option<u32>,
}

/// Write `image` through `port`. The caller has already passed [`check_port`] and confirmed with the person.
pub fn flash(port: &str, image: &Image, opts: Options, progress: &mut dyn ProgressCallbacks) -> Result<(), FlashError> {
    use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
    use espflash::flasher::Flasher;
    image.validate()?;
    let fail = |what: &str, e: &dyn std::fmt::Display| FlashError(format!("{what}: {e}"));
    let serial = serialport::new(port, 115_200)
        .flow_control(serialport::FlowControl::None)
        .open_native()
        .map_err(|e| fail(&format!("could not open {port}"), &e))?;
    let usb = serialport::available_ports()
        .ok()
        .and_then(|v| v.into_iter().find(|p| p.port_name == port))
        .and_then(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => Some(u),
            _ => None,
        })
        .unwrap_or(serialport::UsbPortInfo { vid: 0, pid: 0, serial_number: None, manufacturer: None, product: None });
    let baud = opts.baud.unwrap_or(460_800);
    let conn = Connection::new(serial, usb, ResetAfterOperation::HardReset, ResetBeforeOperation::DefaultReset, 115_200);
    let mut flasher = Flasher::connect(conn, true, true, false, None, Some(baud)).map_err(|e| fail("could not connect to the chip (is it in the COM port's auto-reset? hold BOOT while plugging in if not)", &e))?;
    if opts.erase_all {
        flasher.erase_flash().map_err(|e| fail("erase failed", &e))?;
    }
    for p in &image.parts {
        flasher.write_bin_to_flash(p.offset, &p.data, progress).map_err(|e| fail(&format!("writing {} failed", p.name), &e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
