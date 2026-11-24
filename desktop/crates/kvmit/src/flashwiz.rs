//! The "Flash adapter" wizard (0.3.0): a window that walks through the safety checks and writes the adapter's firmware
//! through its UART (COM) port. All the rules are in [`blockers`] (tested) and in `kvmit-flash`; the flashing runs on its
//! own thread and the window only reads its progress.
use kvmit_flash::{check_no_native_adapter, check_port, flash, flash_boot_drive, list_ports, usb_devices, BootUrl, FlashStage, Image, Options, PortInfo, PortKind, ProgressCallbacks, Report};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};


#[derive(Clone, Default)]
struct Progress {
    part: usize,
    parts: usize,
    done: usize,
    total: usize,
    verifying: bool,
    phase: String,
    log: Vec<String>,
    started: Option<Instant>,
    changed: Option<Instant>,
    completed_bytes: usize,
    part_bytes: usize,
    total_bytes: usize,
    mac: Option<String>,
    part_name: String,
}

type Outcome = Option<Result<Report, String>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FlashMode {
    Firmware,
    BootUrl,
    RestoreBootDrive,
}

pub struct Wizard {
    firmware: String,
    image: Result<Image, String>,
    ports: Vec<PortInfo>,
    /// Set when an adapter's own USB port (a keyboard and mouse) is plugged into this computer.
    native: Option<String>,
    port: Option<String>,
    erase_all: bool,
    erase_ack: bool,
    mode: FlashMode,
    boot_url: String,
    append_mac: bool,
    progress: Arc<Mutex<Progress>>,
    outcome: Arc<Mutex<Outcome>>,
    running: bool,
    baud: u32,
    history: Vec<String>,
    recorded: bool,
    attempt: String,
}

/// Where the firmware to flash is looked for: `KVMIT_FIRMWARE`, then the `firmware` folder that ships beside the program
/// (installer, zip and AppImage all carry one). Only a development (debug) build also tries `firmware/release` in the working
/// directory; a release build never picks firmware out of whatever folder it was started from. Empty means "not found".
pub fn default_firmware_dir() -> String {
    if let Some(p) = std::env::var_os("KVMIT_FIRMWARE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p).to_string_lossy().into_owned();
    }
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("firmware")));
    match beside {
        Some(d) if d.join("flasher_args.json").exists() => d.to_string_lossy().into_owned(),
        _ if cfg!(debug_assertions) => "firmware/release".into(),
        _ => String::new(),
    }
}

/// Why flashing is not allowed right now; empty means it is. `selected` is the chosen port.
pub fn blockers(image_ok: bool, native: Option<&str>, external: &[String], selected: Option<&PortInfo>, erase_all: bool, erase_ack: bool, busy: bool) -> Vec<String> {
    let mut v = Vec::new();
    if busy {
        v.push("Flashing is in progress.".to_string());
        return v;
    }
    if !image_ok {
        v.push("No valid firmware image: choose the firmware folder.".into());
    }
    v.extend(external.iter().cloned());
    if let Some(n) = native {
        v.push(n.to_string());
    }
    match selected {
        None => v.push("Plug the board's COM USB port into this computer and choose it.".into()),
        Some(p) => {
            if let Err(e) = check_port(p, false) {
                v.push(e.0);
            }
        }
    }
    if erase_all && !erase_ack {
        v.push("Tick the box to confirm erasing the pairing and settings.".into());
    }
    v
}

struct Bar {
    p: Arc<Mutex<Progress>>,
    ctx: egui::Context,
}
impl ProgressCallbacks for Bar {
    fn stage(&mut self, stage: &FlashStage) {
        let mut p = self.p.lock().unwrap();
        p.phase = stage.to_string();
        p.changed = Some(Instant::now());
        let seconds = p.started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        let line = format!("{seconds:>4}s  {stage}");
        p.log.push(line);
        if let FlashStage::Preparing { name, index, count, bytes, .. } = stage {
            p.part_name = name.clone();
            p.part = *index;
            p.parts = *count;
            p.part_bytes = *bytes;
            p.done = 0;
            p.total = 0;
            p.verifying = false;
        }
        if let FlashStage::Identified { mac, .. } = stage { p.mac = mac.clone(); }
        self.ctx.request_repaint();
    }
    fn init(&mut self, _addr: u32, total: usize) {
        let mut p = self.p.lock().unwrap();
        p.done = 0;
        p.total = total;
        p.verifying = false;
        p.phase = format!("Writing {} (region {} of {})", p.part_name, p.part, p.parts);
        p.changed = Some(Instant::now());
        self.ctx.request_repaint();
    }
    fn update(&mut self, current: usize) {
        let mut p = self.p.lock().unwrap();
        p.done = current;
        p.changed = Some(Instant::now());
        self.ctx.request_repaint();
    }
    fn verifying(&mut self) {
        let mut p = self.p.lock().unwrap();
        p.verifying = true;
        p.phase = format!("Verifying {} (region {} of {})", p.part_name, p.part, p.parts);
        p.changed = Some(Instant::now());
        self.ctx.request_repaint();
    }
    fn finish(&mut self, skipped: bool) {
        let mut p = self.p.lock().unwrap();
        p.completed_bytes += p.part_bytes;
        p.part_bytes = 0;
        let line = format!("Region {}: {}", p.part, if skipped { "already matches; verified without rewriting" } else { "written and verified" });
        p.log.push(line);
        self.ctx.request_repaint();
    }
}

impl Default for Wizard {
    fn default() -> Self {
        Self::new()
    }
}

impl Wizard {
    pub fn new() -> Wizard {
        let firmware = default_firmware_dir();
        let mut w = Wizard {
            image: Err(String::new()),
            firmware,
            ports: Vec::new(),
            native: None,
            port: None,
            erase_all: false,
            erase_ack: false,
            mode: FlashMode::Firmware,
            boot_url: String::new(),
            append_mac: false,
            progress: Arc::default(),
            outcome: Arc::default(),
            running: false,
            baud: 460_800,
            history: Vec::new(),
            recorded: false,
            attempt: String::new(),
        };
        w.refresh();
        w
    }

    fn refresh(&mut self) {
        self.image = Image::from_build_dir(std::path::Path::new(&self.firmware)).map_err(|e| e.0);
        self.ports = list_ports();
        self.native = usb_devices().and_then(|d| check_no_native_adapter(&d)).err().map(|e| e.0);
        let still = self.port.as_ref().is_some_and(|n| self.ports.iter().any(|p| &p.name == n));
        if !still {
            // preselect the one adapter UART if there is exactly one; never preselect anything else
            let mut uarts = self.ports.iter().filter(|p| p.kind == PortKind::Uart);
            self.port = match (uarts.next(), uarts.next()) {
                (Some(p), None) => Some(p.name.clone()),
                _ => None,
            };
        }
    }

    fn selected(&self) -> Option<&PortInfo> {
        self.port.as_ref().and_then(|n| self.ports.iter().find(|p| &p.name == n))
    }

    /// A flash is being written: the window cannot be closed and the app must not exit.
    pub fn busy(&self) -> bool {
        self.running
    }

    fn start(&mut self, ctx: &egui::Context, boot_url: Option<BootUrl>) {
        let (Ok(mut image), Some(port)) = (self.image.clone(), self.selected().cloned()) else { return };
        let boot_drive_only = self.mode != FlashMode::Firmware;
        let erase_all = !boot_drive_only && self.erase_all;
        let (parts, total_bytes) = if boot_drive_only {
            (1, image.parts.get(3).map_or(0, |part| part.data.len()))
        } else {
            (image.parts.len(), image.total_bytes())
        };
        *self.progress.lock().unwrap() = Progress { parts, total_bytes, started: Some(Instant::now()), ..Progress::default() };
        *self.outcome.lock().unwrap() = None;
        self.running = true;
        self.recorded = false;
        let mode = if boot_drive_only { "boot drive only" } else if erase_all { "full erase" } else { "keep pairing/settings" };
        self.attempt = format!("{} · USB serial {} · firmware {} · {} baud · {mode}", port.name, port.serial.as_deref().unwrap_or("unavailable"), image.firmware_version().unwrap_or_else(|| "unknown".into()), self.baud);
        let (opts, p, o, ctx2) = (Options { erase_all, allow_other_port: false, baud: Some(self.baud) }, self.progress.clone(), self.outcome.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut bar = Bar { p, ctx: ctx2.clone() };
            let r = (|| {
                if let Some(url) = &boot_url { image.set_boot_url(url)?; }
                if boot_drive_only {
                    flash_boot_drive(&port, &image, opts.baud, &mut bar)
                } else {
                    flash(&port, &image, opts, &mut bar)
                }
            })().map_err(|e| e.0);
            *o.lock().unwrap() = Some(r);
            ctx2.request_repaint();
        });
    }

    /// Draw the window; returns false when it should be closed.
    pub fn show(&mut self, ctx: &egui::Context, external: &[String]) -> bool {
        let mut open = true;
        let mut close = false;
        let finished = self.outcome.lock().unwrap().clone();
        if finished.is_some() {
            self.running = false;
            if !self.recorded {
                let result = match &finished { Some(Ok(_)) => "verified".to_string(), Some(Err(e)) => format!("failed: {e}"), _ => unreachable!() };
                let mac = self.progress.lock().unwrap().mac.clone().unwrap_or_else(|| "identity not read".into());
                self.history.push(format!("{} · {mac} · {result}", self.attempt));
                self.recorded = true;
            }
        }
        let width = (ctx.screen_rect().width() - 48.0).clamp(240.0, 660.0);
        let height = (ctx.screen_rect().height() - 48.0).max(180.0);
        let window = egui::Window::new("Flash adapters").collapsible(false).resizable(true).default_width(width).max_width(width).max_height(height).vscroll(true).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
        // no close button while a flash is being written: an interrupted write can leave the board unbootable
        let window = if self.running { window } else { window.open(&mut open) };
        window.show(ctx, |ui| {
            ui.set_max_width(width);
            if self.running {
                let p = self.progress.lock().unwrap().clone();
                ui.strong(&self.attempt);
                ui.horizontal(|ui| { ui.spinner(); ui.label(&p.phase); });
                let current = if p.total == 0 { 0.0 } else { p.part_bytes as f32 * p.done as f32 / p.total as f32 };
                let frac = (p.completed_bytes as f32 + current) / p.total_bytes.max(1) as f32;
                ui.add(egui::ProgressBar::new(frac.min(0.99)).text(format!("Region {} of {} · {} blocks acknowledged", p.part, p.parts, p.done)));
                ui.label(format!("Elapsed: {} s · Keep the COM cable connected", p.started.map(|t| t.elapsed().as_secs()).unwrap_or(0)));
                if p.changed.is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
                    ui.weak("Waiting for the board. Erasing and checksumming can take time; the flash worker is still running.");
                }
                self.details(ui, &p, None);
                ctx.request_repaint_after(Duration::from_millis(250));
                return;
            }
            match finished {
                Some(Ok(report)) => {
                    ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).good_text, format!("Flashed and verified{}.", report.mac.map(|m| format!(" ({m})")).unwrap_or_default()));
                    ui.label("All image checksums match. A restart command was sent; connect over Bluetooth to confirm the running firmware version and target USB status.");
                    if ui.button("Flash another board").clicked() { *self.outcome.lock().unwrap() = None; self.port = None; self.refresh(); }
                    self.details(ui, &self.progress.lock().unwrap().clone(), None);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).bad_text, "Flashing failed.");
                    ui.label(&e);
                    ui.strong("This attempt did not complete. Retry and verify before using the board.");
                    ui.weak("The board can be recovered: flash again. If it is not found, hold BOOT while plugging in the COM cable, then flash again.");
                    if ui.button("Try again").clicked() {
                        *self.outcome.lock().unwrap() = None;
                        self.refresh();
                    }
                    if ui.button("Use slower speed (115200 baud)").clicked() { self.baud = 115_200; *self.outcome.lock().unwrap() = None; self.refresh(); }
                    self.details(ui, &self.progress.lock().unwrap().clone(), Some(&e));
                }
                None => self.form(ui, ctx, external),
            }
        });
        open && !close
    }

    fn details(&self, ui: &mut egui::Ui, p: &Progress, error: Option<&str>) {
        let mut text = format!("kvm-it {}\n{}\n{}\n{}", env!("CARGO_PKG_VERSION"), self.attempt, p.log.join("\n"), error.unwrap_or(""));
        if ui.button("Copy flash diagnostics").clicked() { ui.ctx().copy_text(text.clone()); }
        ui.collapsing("Flash details", |ui| { egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| { ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY).interactive(false)); }); });
    }

    fn form(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, external: &[String]) {
        ui.label("Connect the adapter's COM USB port to this computer.");
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.mode, FlashMode::Firmware, "Firmware");
            ui.selectable_value(&mut self.mode, FlashMode::BootUrl, "Set boot URL");
            ui.selectable_value(&mut self.mode, FlashMode::RestoreBootDrive, "Restore boot drive");
        });
        let boot_drive_only = self.mode != FlashMode::Firmware;
        let boot_url = (self.mode == FlashMode::BootUrl).then(|| {
            ui.label("Boot URL");
            ui.add(egui::TextEdit::singleline(&mut self.boot_url).hint_text("https://server.example/install.ipxe"));
            ui.checkbox(&mut self.append_mac, "Append the Ethernet MAC to the URL");
            ui.weak("The target can read this URL. Use a URL without credentials.");
            BootUrl::new(self.boot_url.trim(), self.append_mac)
        });
        ui.collapsing("Firmware location (advanced)", |ui| {
            ui.label("Firmware folder");
            if ui.text_edit_singleline(&mut self.firmware).lost_focus() {
                self.refresh();
            }
        });
        match &self.image {
            Ok(im) => {
                ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).good_text, format!("Firmware {} · ESP32-S3 · {} MiB flash · {} images", im.firmware_version().unwrap_or_else(|| "version unavailable".into()), im.flash_bytes / (1024 * 1024), im.parts.len()));
                if !boot_drive_only {
                    ui.small(format!("{:.1} MiB to check/write, including the boot drive", im.total_bytes() as f32 / (1024.0 * 1024.0)));
                }
            }
            Err(e) if e.is_empty() => {}
            Err(e) => {
                ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).bad_text, e);
            }
        }
        if self.firmware.trim().is_empty() {
            ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).bad_text, "No firmware folder found next to the program: type the folder that holds flasher_args.json.");
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Board");
            if ui.button("Rescan ports").clicked() {
                self.refresh();
            }
        });
        if self.ports.is_empty() {
            ui.label("No USB serial ports found. Plug the board's COM port in, then Rescan ports.");
        }
        let mut pick = None;
        for p in &self.ports {
            let tag = match p.kind {
                PortKind::Uart => "adapter COM port",
                PortKind::NativeUsb => "native USB: never flash through this",
                PortKind::Other => "not an adapter port",
            };
            if ui.selectable_label(self.port.as_deref() == Some(p.name.as_str()), format!("{} · {tag} · USB serial {}", p.name, p.serial.as_deref().unwrap_or("unavailable"))).on_hover_text(&p.description).clicked() {
                pick = Some(p.name.clone());
            }
        }
        if pick.is_some() {
            self.port = pick;
        }
        if self.selected().is_some_and(|p| p.serial.as_deref().is_none_or(str::is_empty)) {
            ui.weak("This USB bridge reports no serial number, so two identical boards cannot be told apart: do not swap cables after choosing.");
        }
        ui.separator();
        egui::ComboBox::from_label("Transfer speed").selected_text(format!("{} baud", self.baud)).show_ui(ui, |ui| {
            ui.selectable_value(&mut self.baud, 460_800, "460800 baud (fast)");
            ui.selectable_value(&mut self.baud, 115_200, "115200 baud (slower, for unreliable links)");
        });
        if !boot_drive_only {
            ui.checkbox(&mut self.erase_all, "Erase everything first (also erases the pairing and settings)");
        }
        let erase_all = !boot_drive_only && self.erase_all;
        if erase_all {
            ui.checkbox(&mut self.erase_ack, "I understand the adapter must be paired again afterwards");
        } else {
            self.erase_ack = false;
            ui.weak("Pairing and settings are kept.");
        }
        ui.add_space(4.0);
        let mut reasons = blockers(self.image.is_ok(), self.native.as_deref(), external, self.selected(), erase_all, self.erase_ack, false);
        if let Some(Err(error)) = &boot_url { reasons.push(error.0.clone()); }
        if boot_drive_only && self.image.as_ref().is_ok_and(|image| image.parts.len() < 4) {
            reasons.push("This firmware bundle has no boot-drive image.".into());
        }
        for r in &reasons {
            ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).bad_text, r);
        }
        ui.weak("Keep the board's native USB port unplugged from this computer: it acts as a keyboard and mouse.");
        let label = if boot_drive_only { "Write boot drive" } else { "Flash adapter" };
        if ui.add_enabled(reasons.is_empty(), egui::Button::new(label)).clicked() {
            self.start(ctx, boot_url.and_then(Result::ok));
        }
        if !self.history.is_empty() {
            ui.collapsing("Boards tested in this window", |ui| { for line in &self.history { ui.label(line); } });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uart() -> PortInfo {
        PortInfo { name: "/dev/ttyACM0".into(), kind: PortKind::Uart, description: "d".into(), vid: 0x1a86, pid: 0x55d3, serial: None }
    }

    #[test]
    fn a_good_setup_has_no_blockers() {
        assert!(blockers(true, None, &[], Some(&uart()), false, false, false).is_empty());
    }

    #[test]
    fn each_missing_piece_blocks() {
        assert_eq!(blockers(false, None, &[], Some(&uart()), false, false, false).len(), 1);
        assert_eq!(blockers(true, None, &[], None, false, false, false).len(), 1);
    }

    #[test]
    fn an_adapter_native_port_blocks_even_with_a_good_uart_selected() {
        let b = blockers(true, Some("would type into this machine"), &[], Some(&uart()), false, false, false);
        assert!(b.iter().any(|m| m.contains("type into this machine")), "{b:?}");
    }

    #[test]
    fn an_unknown_port_cannot_be_chosen() {
        let mut other = uart();
        other.kind = PortKind::Other;
        assert!(!blockers(true, None, &[], Some(&other), false, false, false).is_empty());
    }

    #[test]
    fn erasing_everything_needs_the_second_confirmation() {
        assert!(!blockers(true, None, &[], Some(&uart()), true, false, false).is_empty());
        assert!(blockers(true, None, &[], Some(&uart()), true, true, false).is_empty());
    }

    #[test]
    fn other_work_in_the_app_blocks_flashing() {
        let ext = vec!["A script is running.".to_string()];
        assert_eq!(blockers(true, None, &ext, Some(&uart()), false, false, false), ext);
    }

    #[test]
    fn nothing_else_is_allowed_while_flashing() {
        assert_eq!(blockers(true, None, &[], Some(&uart()), false, false, true).len(), 1);
    }
}
