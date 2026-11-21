use super::*;
use kvmit_flash::{PortInfo, PortKind};
use std::io::{self, Write};
use std::path::PathBuf;

mod serial;
mod volume;

enum Operation {
    Show,
    Send(Vec<PathBuf>),
    Collect,
}
type Job = std_mpsc::Receiver<Result<Vec<PathBuf>, String>>;

#[derive(Default)]
pub(super) struct Files {
    ports: Vec<PortInfo>,
    scan: Option<std_mpsc::Receiver<Vec<PortInfo>>>,
    scanned: bool,
    selected: Option<PortInfo>,
    job: Option<Job>,
    pub(super) active: Arc<AtomicBool>,
    pub(super) preparing: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<String>>,
    received: Vec<PathBuf>,
    // Some desktop environments require the clipboard owner to stay alive.
    owner: Option<arboard::Clipboard>,
}

fn exchange(
    port: &PortInfo,
    uuid: [u8; 16],
    op: Operation,
    status: &Mutex<String>,
    cancel: &AtomicBool,
    preparing: &AtomicBool,
) -> io::Result<Vec<PathBuf>> {
    let _hardware = kvmit_control::ResourceClaim::hardware(true).map_err(io::Error::other)?;
    if !kvmit_flash::list_ports().contains(port) {
        return Err(io::Error::other(
            "The selected COM cable changed. Scan again.",
        ));
    }
    let mut cable = serial::Cable::open(&port.name)?;
    let info = cable.request("INFO")?;
    let fields: Vec<_> = info.split(',').collect();
    let board = serial::hex(&uuid);
    if fields.len() != 3 || fields[0] != board {
        return Err(io::Error::other(
            "The cable and Bluetooth connection belong to different adapters.",
        ));
    }
    if fields[2] == "target" {
        if matches!(op, Operation::Show) {
            return Ok(vec![]);
        }
        return Err(io::Error::other(
            "Eject KVM SHARE on the target, then try again.",
        ));
    }
    let blocks = fields[1]
        .parse::<u32>()
        .ok()
        .filter(|n| (2048..=16384).contains(n))
        .ok_or_else(|| io::Error::other("The adapter reported an invalid drive size."))?;
    cable.request(&format!("TAKE {board}"))?;
    let progress = |done, total| {
        if cancel.load(Ordering::SeqCst) {
            return Err(io::Error::other("Transfer cancelled."));
        }
        *status.lock().unwrap() = format!(
            "Transferring {} of {} KiB",
            u64::div_ceil(done, 1024),
            u64::div_ceil(total, 1024)
        );
        Ok(())
    };
    let show = !matches!(op, Operation::Collect);
    let work = (|| {
        if show {
            cable.request("PREP")?;
        }
        preparing.store(false, Ordering::SeqCst);
        let mut disk = serial::Disk::new(&mut cable, blocks);
        let fs = volume::mount(&mut disk, show)?;
        let mut files = vec![];
        if let Operation::Send(paths) = op {
            let batch = format!(
                "Transfer-{:016x}",
                getrandom::u64().map_err(|_| io::Error::other("Cannot create transfer ID"))?
            );
            volume::send(&fs, &paths, &batch, &progress)?;
        } else if matches!(op, Operation::Collect) {
            let parent = dirs::download_dir()
                .ok_or_else(|| io::Error::other("Cannot locate the Downloads folder."))?
                .join("kvm-it");
            files = volume::collect(&fs, &parent, &progress)?;
        }
        fs.unmount()?;
        disk.flush()?;
        Ok(files)
    })();
    if work.is_err() {
        let _ = cable.request("DONE");
        return work;
    }
    cable.request(if show { "SHOW" } else { "DONE" })?;
    work
}

impl App {
    pub(super) fn files_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("USB file exchange");
        ui.label("On the target, open KVM SHARE in the file manager. Take files from “From host” or put files in “To host”, then eject the drive.");
        let fw = self.device().map(|d| (d.info().fw, d.info().uuid));
        let idle = self.input_idle();
        let boot_drive = self
            .status
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|s| s.boot_drive)
            == Some(true);
        let files = &mut self.files;
        if let Some(rx) = &files.job {
            match rx.try_recv() {
                Ok(result) => {
                    *files.progress.lock().unwrap() = match result {
                        Ok(paths) if paths.is_empty() => {
                            "Drive ready on the adapter. Open KVM SHARE on the target.".into()
                        }
                        Ok(paths) => {
                            files.received = paths;
                            format!(
                                "{} file(s) saved in {}.",
                                files.received.len(),
                                files.received[0].parent().unwrap().display()
                            )
                        }
                        Err(e) => e,
                    };
                    files.job = None;
                }
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    *files.progress.lock().unwrap() =
                        "Transfer interrupted. Check the drive before retrying.".into();
                    files.job = None;
                }
                Err(_) => ctx.request_repaint_after(Duration::from_millis(150)),
            }
        }
        if let Some(rx) = &files.scan {
            if let Ok(ports) = rx.try_recv() {
                if ports.len() == 1 {
                    files.selected = ports.first().cloned();
                } else if !files.selected.as_ref().is_some_and(|p| ports.contains(p)) {
                    files.selected = None;
                }
                files.ports = ports;
                files.scan = None;
            }
        }
        let scan = ui
            .add_enabled(
                files.job.is_none() && files.scan.is_none(),
                egui::Button::new("Find COM cable"),
            )
            .clicked();
        if (scan || !files.scanned) && files.scan.is_none() {
            let (tx, rx) = std_mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(
                    kvmit_flash::list_ports()
                        .into_iter()
                        .filter(|p| p.kind == PortKind::Uart)
                        .collect(),
                );
                ctx.request_repaint();
            });
            files.scan = Some(rx);
            files.scanned = true;
        }
        if files.scan.is_some() {
            ui.spinner();
        } else if files.ports.is_empty() {
            ui.small("Connect the board’s COM port to this computer using the supported single-power setup.");
        } else {
            for port in &files.ports {
                ui.add_enabled_ui(files.job.is_none(), |ui| {
                    ui.selectable_value(
                        &mut files.selected,
                        Some(port.clone()),
                        format!("{}  ·  adapter cable", port.name),
                    );
                });
            }
        }
        let available = fw.is_some_and(|(fw, _)| fw >= [0, 3, 0]);
        if !available {
            ui.small("Connect an adapter running firmware 0.3.0 or later. Text paste works with earlier firmware.");
        }
        if boot_drive {
            ui.small("Turn off the boot drive under Devices to use file exchange.");
        }
        if !idle && files.job.is_none() {
            ui.small("Finish sending input before opening the drive.");
        }
        let mut action = None;
        ui.add_enabled_ui(
            files.job.is_none() && files.selected.is_some() && available && !boot_drive && idle,
            |ui| {
                if ui.button("Share copied files").clicked() {
                    match arboard::Clipboard::new().and_then(|mut cb| cb.get().file_list()) {
                        Ok(paths) if !paths.is_empty() => action = Some(Operation::Send(paths)),
                        _ => {
                            *files.progress.lock().unwrap() =
                                "Copy one or more files in your file manager first.".into()
                        }
                    }
                }
                if ui.button("Show drive on target").clicked() {
                    action = Some(Operation::Show);
                }
                if ui.button("Collect from target").clicked() {
                    action = Some(Operation::Collect);
                }
            },
        );
        if let (Some(op), Some(port), Some((_, uuid))) = (action, files.selected.clone(), fw) {
            *files.progress.lock().unwrap() = "Opening the adapter’s exchange drive…".into();
            let progress = files.progress.clone();
            files.active.store(true, Ordering::SeqCst);
            let active = files.active.clone();
            files.preparing.store(true, Ordering::SeqCst);
            let preparing = files.preparing.clone();
            files.cancel.store(false, Ordering::SeqCst);
            let cancel = files.cancel.clone();
            let (tx, rx) = std_mpsc::channel();
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                struct Finished(Arc<AtomicBool>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::SeqCst);
                    }
                }
                let _finish = Finished(active);
                let _ready = Finished(preparing.clone());
                let _ = tx.send(
                    exchange(&port, uuid, op, &progress, &cancel, &preparing)
                        .map_err(|e| e.to_string()),
                );
                ctx.request_repaint();
            });
            files.job = Some(rx);
        }
        ui.label(files.progress.lock().unwrap().as_str());
        if files.active.load(Ordering::SeqCst) && ui.button("Cancel transfer").clicked() {
            files.cancel.store(true, Ordering::SeqCst);
        }
        if !files.received.is_empty() && ui.button("Copy received files to clipboard").clicked() {
            let result = arboard::Clipboard::new().and_then(|mut cb| {
                cb.set().file_list(&files.received)?;
                files.owner = Some(cb);
                Ok(())
            });
            *files.progress.lock().unwrap() = if result.is_ok() {
                "Ready to paste in your file manager."
            } else {
                "Clipboard busy. Your files are saved in Downloads/kvm-it."
            }
            .into();
        }
    }
}
