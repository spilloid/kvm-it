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
enum Outcome {
    Shown,
    Sent(String),
    Collected(Vec<PathBuf>),
}
type Job = std_mpsc::Receiver<Result<Outcome, String>>;

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

fn transfer_status(direction: &str, done: u64, total: u64, elapsed: Duration) -> String {
    let mut label = format!(
        "{direction} · {} / {} KiB",
        done.div_ceil(1024),
        total.div_ceil(1024)
    );
    if done > 0 && elapsed >= Duration::from_secs(1) {
        let bytes_per_sec = done as f64 / elapsed.as_secs_f64();
        label.push_str(&format!(" · {:.1} KiB/s", bytes_per_sec / 1024.0));
        if done < total {
            let seconds = ((total - done) as f64 / bytes_per_sec).ceil() as u64;
            label.push_str(&format!(
                " · about {}m {:02}s left",
                seconds / 60,
                seconds % 60
            ));
        }
    }
    label
}

fn exchange(
    port: &PortInfo,
    uuid: [u8; 16],
    op: Operation,
    status: &Mutex<String>,
    cancel: &AtomicBool,
    preparing: &AtomicBool,
) -> io::Result<Outcome> {
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
            return Ok(Outcome::Shown);
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
    let show = !matches!(op, Operation::Collect);
    let work = (|| {
        if show {
            cable.request("PREP")?;
        }
        preparing.store(false, Ordering::SeqCst);
        *status.lock().unwrap() = "Preparing KVM SHARE…".into();
        let mut request = |cmd: &str| cable.request(cmd);
        let mut disk = serial::Disk::new(&mut request, blocks);
        let fs = volume::mount(&mut disk, show)?;
        let direction = if show {
            "Sending to target"
        } else {
            "Receiving from target"
        };
        let started = std::cell::OnceCell::new();
        let progress = |done, total| {
            if cancel.load(Ordering::SeqCst) {
                return Err(io::Error::other("Transfer cancelled."));
            }
            let elapsed = started.get_or_init(Instant::now).elapsed();
            *status.lock().unwrap() = transfer_status(direction, done, total, elapsed);
            Ok(())
        };
        let outcome = if let Operation::Send(paths) = op {
            let batch = format!(
                "Transfer-{:016x}",
                getrandom::u64().map_err(|_| io::Error::other("Cannot create transfer ID"))?
            );
            volume::send(&fs, &paths, &batch, &progress)?;
            Outcome::Sent(batch)
        } else if matches!(op, Operation::Collect) {
            let parent = dirs::download_dir()
                .ok_or_else(|| io::Error::other("Cannot locate the Downloads folder."))?
                .join("kvm-it");
            Outcome::Collected(volume::collect(&fs, &parent, &progress)?)
        } else {
            Outcome::Shown
        };
        *status.lock().unwrap() = "Finishing the file exchange…".into();
        fs.unmount()?;
        disk.flush()?;
        Ok(outcome)
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
        ui.label("KVM SHARE appears as a USB drive on the target. This computer reaches it over the adapter’s slower COM cable.");
        ui.small("Just sending plain text? Use Send → Type host clipboard.");
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
                        Ok(Outcome::Shown) => {
                            "On the target, copy files into KVM SHARE → To host, then eject KVM SHARE and collect them here.".into()
                        }
                        Ok(Outcome::Sent(batch)) => format!(
                            "On the target, open KVM SHARE → From host → {batch}. Eject the drive after copying your files."
                        ),
                        Ok(Outcome::Collected(paths)) => {
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
        {
            let message = files.progress.lock().unwrap();
            if !message.is_empty() {
                ui.label(message.as_str());
            }
        }
        if files.active.load(Ordering::SeqCst) && ui.button("Cancel transfer").clicked() {
            files.cancel.store(true, Ordering::SeqCst);
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
            ui.label("No adapter COM cable found.");
            ui.small("Both USB ports can power the board. Before connecting COM to this computer, verify that only one computer supplies power with your board and cables.");
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
        let ready =
            files.job.is_none() && files.selected.is_some() && available && !boot_drive && idle;
        ui.separator();
        ui.strong("This computer → target");
        ui.label("Copy files in this computer’s file manager, then send them to the drive.");
        if ui
            .add_enabled(ready, egui::Button::new("Send copied files"))
            .clicked()
        {
            match arboard::Clipboard::new().and_then(|mut cb| cb.get().file_list()) {
                Ok(paths) if !paths.is_empty() => action = Some(Operation::Send(paths)),
                _ => {
                    *files.progress.lock().unwrap() =
                        "Copy one or more files in your file manager first.".into();
                }
            }
        }
        ui.separator();
        ui.strong("Target → this computer");
        ui.label("Show the drive, put files in “To host” on the target, then eject the drive before collecting.");
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(ready, egui::Button::new("Show drive on target"))
                .clicked()
            {
                action = Some(Operation::Show);
            }
            if ui
                .add_enabled(ready, egui::Button::new("Collect from target"))
                .clicked()
            {
                action = Some(Operation::Collect);
            }
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_rate_and_remaining_time_use_payload_bytes() {
        let status = |done, total, secs| {
            transfer_status("Sending to target", done, total, Duration::from_secs(secs))
        };
        assert_eq!(status(0, 0, 0), "Sending to target · 0 / 0 KiB");
        assert_eq!(
            status(4096, 65536, 4),
            "Sending to target · 4 / 64 KiB · 1.0 KiB/s · about 1m 00s left"
        );
        assert_eq!(
            status(65536, 65536, 32),
            "Sending to target · 64 / 64 KiB · 2.0 KiB/s"
        );
    }
}
