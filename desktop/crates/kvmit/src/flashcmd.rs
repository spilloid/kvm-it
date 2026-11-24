//! `kvmit flash`: write the adapter's firmware through its UART (COM) port. The decisions live in `kvmit-flash`; this is
//! the talking-to-a-person part.
use kvmit_flash::{check_no_native_adapter, check_port, flash, flash_boot_drive, list_ports, usb_devices, BootScript, FlashStage, Image, Options, PortKind, ProgressCallbacks};
use std::io::{BufRead, Write};
use std::path::PathBuf;

pub struct Args {
    pub firmware: Option<PathBuf>,
    pub port: Option<String>,
    pub any_port: bool,
    pub erase_all: bool,
    pub yes: bool,
    pub list: bool,
    pub baud: Option<u32>,
    pub boot_url: Option<String>,
    pub boot_script: Option<PathBuf>,
    pub append_mac: bool,
    pub restore_boot_drive: bool,
}

type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Bar {
    total: usize,
    done: usize,
    name: &'static str,
}

impl ProgressCallbacks for Bar {
    fn stage(&mut self, stage: &FlashStage) { eprintln!("{stage}"); }
    fn init(&mut self, addr: u32, total: usize) {
        self.total = total;
        self.done = 0;
        self.name = "";
        eprint!("  0x{addr:06x}  {total} blocks ");
    }
    fn update(&mut self, current: usize) {
        let before = self.done * 20 / self.total.max(1);
        self.done = current;
        for _ in before..(current * 20 / self.total.max(1)) {
            eprint!(".");
        }
    }
    fn verifying(&mut self) {
        eprint!(" verifying");
    }
    fn finish(&mut self, skipped: bool) {
        eprintln!("{}", if skipped { " unchanged, skipped" } else { " done" });
        let _ = self.name;
    }
}

pub fn run(a: Args) -> R<()> {
    let ports = list_ports();
    if a.list {
        if ports.is_empty() {
            println!("no USB serial ports");
        }
        for p in &ports {
            let tag = match p.kind {
                PortKind::Uart => "adapter UART (flash through this)",
                PortKind::NativeUsb => "adapter native USB (never flash through this)",
                PortKind::Other => "other",
            };
            println!("{}  {}  {}", p.name, p.description, tag);
        }
        return Ok(());
    }
    // fail early and loudly; flash() checks again at the moment of writing
    check_no_native_adapter(&usb_devices()?)?;
    let firmware = a.firmware.clone().unwrap_or_else(|| PathBuf::from(crate::flashwiz::default_firmware_dir()));
    if firmware.as_os_str().is_empty() {
        return Err("no firmware folder found next to the program (and none given): pass --firmware <dir> or set KVMIT_FIRMWARE".into());
    }
    let mut image = Image::from_build_dir(&firmware)?;
    let boot_drive_only = a.restore_boot_drive || a.boot_url.is_some() || a.boot_script.is_some();
    if let Some(url) = &a.boot_url {
        image.set_boot_script(&BootScript::for_url(url, a.append_mac)?)?;
    } else if let Some(file) = &a.boot_script {
        image.set_boot_script(&BootScript::from_file(file)?)?;
    }
    let port = match &a.port {
        Some(name) => ports.iter().find(|p| &p.name == name).cloned().ok_or_else(|| format!("{name} is not a USB serial port here (see `kvmit flash --list`)"))?,
        None => {
            let uarts: Vec<_> = ports.iter().filter(|p| p.kind == PortKind::Uart).collect();
            match uarts.as_slice() {
                [one] => (*one).clone(),
                [] => return Err("no adapter UART (COM) port found. Plug the board's COM USB port into this computer (see `kvmit flash --list`).".into()),
                _ => return Err("more than one adapter UART port; choose one with --port (see `kvmit flash --list`)".into()),
            }
        }
    };
    if let Some(w) = check_port(&port, a.any_port)? {
        eprintln!("warning: {w}");
    }
    println!("firmware: {} ({} images, {} bytes), from {}", image.firmware_version().unwrap_or_else(|| "version unavailable".into()), image.parts.len(), image.total_bytes(), firmware.display());
    println!("port:     {} ({})", port.name, port.description);
    if port.serial.as_deref().is_none_or(str::is_empty) {
        println!("note:     this USB bridge reports no serial number, so identical boards cannot be told apart: do not swap cables now.");
    }
    println!(
        "{}",
        if boot_drive_only {
            "mode:     boot drive only. The adapter restarts after verification."
        } else if a.erase_all {
            "mode:     FULL ERASE first: the adapter's pairing (bond) and settings are erased; re-pair afterwards."
        } else {
            "mode:     all bundled images (including the boot drive): pairing and settings are kept."
        }
    );
    println!("The board's native USB port must NOT be plugged into this computer (it would type into it).");
    if !a.yes {
        print!("Flash now? type yes: ");
        std::io::stdout().flush()?;
        let mut s = String::new();
        std::io::stdin().lock().read_line(&mut s)?;
        if s.trim() != "yes" {
            return Err("cancelled".into());
        }
    }
    let mut bar = Bar { total: 0, done: 0, name: "" };
    let report = if boot_drive_only {
        flash_boot_drive(&port, &image, a.baud, &mut bar)?
    } else {
        flash(&port, &image, Options { erase_all: a.erase_all, allow_other_port: a.any_port, baud: a.baud }, &mut bar)?
    };
    println!("flashed and verified (ESP32-S3{}). The adapter restarts by itself; check it with `kvmit status`.", report.mac.map(|m| format!(", {m}")).unwrap_or_default());
    Ok(())
}
