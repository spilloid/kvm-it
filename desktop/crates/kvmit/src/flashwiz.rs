//! The "Flash adapter" wizard (0.3.0): a window that walks through the safety checks and writes the adapter's firmware
//! through its UART (COM) port. All the rules are in [`blockers`] (tested) and in `kvmit-flash`; the flashing runs on its
//! own thread and the window only reads its progress.
use kvmit_flash::{check_no_native_adapter, check_port, flash, list_ports, usb_devices, Image, Options, PortInfo, PortKind, ProgressCallbacks, Report};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};


#[derive(Clone, Default)]
struct Progress {
    part: usize,
    parts: usize,
    done: usize,
    total: usize,
    verifying: bool,
}

type Outcome = Option<Result<Report, String>>;

pub struct Wizard {
    firmware: String,
    image: Result<Image, String>,
    ports: Vec<PortInfo>,
    /// Set when an adapter's own USB port (a keyboard and mouse) is plugged into this computer.
    native: Option<String>,
    port: Option<String>,
    erase_all: bool,
    erase_ack: bool,
    progress: Arc<Mutex<Progress>>,
    outcome: Arc<Mutex<Outcome>>,
    running: bool,
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
    fn init(&mut self, _addr: u32, total: usize) {
        let mut p = self.p.lock().unwrap();
        p.part += 1;
        p.done = 0;
        p.total = total;
        p.verifying = false;
        self.ctx.request_repaint();
    }
    fn update(&mut self, current: usize) {
        self.p.lock().unwrap().done = current;
        self.ctx.request_repaint();
    }
    fn verifying(&mut self) {
        self.p.lock().unwrap().verifying = true;
        self.ctx.request_repaint();
    }
    fn finish(&mut self, _skipped: bool) {
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
            progress: Arc::default(),
            outcome: Arc::default(),
            running: false,
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

    fn start(&mut self, ctx: &egui::Context) {
        let (Ok(image), Some(port)) = (self.image.clone(), self.selected().cloned()) else { return };
        *self.progress.lock().unwrap() = Progress { parts: image.parts.len(), ..Progress::default() };
        *self.outcome.lock().unwrap() = None;
        self.running = true;
        let (opts, p, o, ctx2) = (Options { erase_all: self.erase_all, allow_other_port: false, baud: None }, self.progress.clone(), self.outcome.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut bar = Bar { p, ctx: ctx2.clone() };
            let r = flash(&port, &image, opts, &mut bar).map_err(|e| e.0);
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
        }
        let window = egui::Window::new("Flash adapter").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]);
        // no close button while a flash is being written: an interrupted write can leave the board unbootable
        let window = if self.running { window } else { window.open(&mut open) };
        window.show(ctx, |ui| {
            ui.set_max_width(520.0);
            if self.running {
                let p = self.progress.lock().unwrap().clone();
                ui.strong("Flashing… do not unplug the board");
                let frac = if p.total == 0 { 0.0 } else { p.done as f32 / p.total as f32 };
                ui.add(egui::ProgressBar::new(frac).text(format!(
                    "part {} of {}{}",
                    p.part.max(1),
                    p.parts,
                    if p.verifying { ", verifying" } else { "" }
                )));
                return;
            }
            match finished {
                Some(Ok(report)) => {
                    ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).good_text, format!("Flashed and verified{}.", report.mac.map(|m| format!(" ({m})")).unwrap_or_default()));
                    ui.label("The adapter restarts by itself. Unplug the COM cable and plug the board into the target when you are ready, then connect from the Adapter chip. If it was paired before, it keeps its pairing; if you erased everything, pair it again.");
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                }
                Some(Err(e)) => {
                    ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).bad_text, "Flashing failed.");
                    ui.label(&e);
                    ui.weak("The board can be recovered: flash again. If it is not found, hold BOOT while plugging in the COM cable, then flash again.");
                    if ui.button("Try again").clicked() {
                        *self.outcome.lock().unwrap() = None;
                    }
                }
                None => self.form(ui, ctx, external),
            }
        });
        open && !close
    }

    fn form(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, external: &[String]) {
        ui.label("Writes the firmware onto an adapter through the board's COM USB port.");
        ui.horizontal(|ui| {
            ui.label("Firmware folder");
            if ui.text_edit_singleline(&mut self.firmware).lost_focus() {
                self.refresh();
            }
        });
        match &self.image {
            Ok(im) => {
                ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).good_text, format!("Firmware OK: {} parts, {} bytes", im.parts.len(), im.total_bytes()));
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
            if ui.selectable_label(self.port.as_deref() == Some(p.name.as_str()), format!("{}  {}  ({tag})", p.name, p.description)).clicked() {
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
        ui.checkbox(&mut self.erase_all, "Erase everything first (also erases the pairing and settings)");
        if self.erase_all {
            ui.checkbox(&mut self.erase_ack, "I understand the adapter must be paired again afterwards");
        } else {
            self.erase_ack = false;
            ui.weak("By default the pairing and settings are kept.");
        }
        ui.add_space(4.0);
        let reasons = blockers(self.image.is_ok(), self.native.as_deref(), external, self.selected(), self.erase_all, self.erase_ack, false);
        for r in &reasons {
            ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).bad_text, r);
        }
        ui.weak("Keep the board's native USB port unplugged from this computer: it acts as a keyboard and mouse.");
        if ui.add_enabled(reasons.is_empty(), egui::Button::new("Flash adapter")).clicked() {
            self.start(ctx);
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
