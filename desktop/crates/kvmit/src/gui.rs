//! egui front end. UX rules from docs/ux.md: state always visible, capture unmistakable, release-all on
//! every exit path, secrets masked and never persisted, scripts interruptible.
use crate::config::Config;
use crate::host::ScriptHost;
use crate::keymap;
use crate::library::{self, Entry};
use crate::session;
use kvmit_ble::backend::{self, Found};
use kvmit_ble::Device;
use kvmit_hid::Key;
use kvmit_layout::UsAnsi;
use kvmit_protocol::message::StatusInfo;
use kvmit_script::{compile, preview, run, Preview, RunEvent, RunOptions, Script, Step, Vars};
use kvmit_video::{Capture, DeviceInfo};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc as std_mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Clone)]
enum Link {
    Disconnected,
    Connecting(String),
    Connected { dev: Device, name: String, id: String },
    Failed(String),
}

enum Scan {
    Idle,
    Scanning,
    Done(Vec<Found>),
    Error(String),
}

enum Input {
    Down(Key),
    Up(Key),
    Button(u8, bool),
    Scroll(i8),
    ReleaseAll,
}

type ConfirmSlot = Arc<Mutex<Option<(String, std_mpsc::Sender<bool>)>>>;

struct RunHandle {
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    log: Arc<Mutex<Vec<String>>>,
    confirm: ConfirmSlot,
}

pub struct App {
    rt: Arc<tokio::runtime::Runtime>,
    cfg: Config,
    link: Arc<Mutex<Link>>,
    link_gen: Arc<AtomicU64>,
    scan: Arc<Mutex<Scan>>,
    status: Arc<Mutex<Option<StatusInfo>>>,
    notice: Arc<Mutex<String>>,
    input_tx: Option<mpsc::UnboundedSender<Input>>,
    input_for: Option<String>,
    capturing: bool,
    /// A "Send keys" chord is in flight; nothing else may send input until it has fully released.
    chord_busy: Arc<AtomicBool>,
    prev_mods: Vec<Key>,
    video_devices: Vec<DeviceInfo>,
    video_sel: usize,
    capture: Arc<Mutex<Option<Capture>>>,
    texture: Option<egui::TextureHandle>,
    last_seq: u64,
    library: Vec<Entry>,
    selected: Option<usize>,
    script: Option<Script>,
    script_err: Option<String>,
    var_values: BTreeMap<String, String>,
    preview: Option<Result<Preview, String>>,
    trust_ack: bool,
    run_handle: Option<RunHandle>,
    run_log: Vec<String>,
    type_text: String,
    type_secret: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let rt = Arc::new(tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(2).build().expect("tokio runtime"));
        let cfg = Config::load();
        let library = library::load_all(&cfg.script_dir());
        let video_devices = kvmit_video::list_devices();
        let mut app = App {
            rt,
            link: Arc::new(Mutex::new(Link::Disconnected)),
            link_gen: Arc::new(AtomicU64::new(0)),
            scan: Arc::new(Mutex::new(Scan::Idle)),
            status: Arc::default(),
            notice: Arc::default(),
            input_tx: None,
            input_for: None,
            capturing: false,
            chord_busy: Arc::default(),
            prev_mods: Vec::new(),
            video_sel: cfg.last_video.as_ref().and_then(|p| video_devices.iter().position(|d| &d.path == p)).unwrap_or(0),
            video_devices,
            capture: Arc::default(),
            texture: None,
            last_seq: 0,
            library,
            selected: None,
            script: None,
            script_err: None,
            var_values: BTreeMap::new(),
            preview: None,
            trust_ack: false,
            run_handle: None,
            run_log: Vec::new(),
            type_text: String::new(),
            type_secret: false,
            cfg,
        };
        // Zero-step reconnect: the last adapter and capture device come back on their own.
        if let Some(id) = app.cfg.last_device.clone() {
            app.start_link(id, cc.egui_ctx.clone(), false);
        }
        if !app.video_devices.is_empty() {
            app.open_video();
        }
        app
    }

    fn set_notice(&self, s: impl Into<String>) {
        *self.notice.lock().unwrap() = s.into();
    }

    /// Connect and keep reconnecting until superseded (generation counter) or the app exits.
    fn start_link(&mut self, id: String, ctx: egui::Context, pair_first: bool) {
        let generation = self.link_gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.cfg.last_device = Some(id.clone());
        self.cfg.save();
        let (link, gen_ref, status, notice) = (self.link.clone(), self.link_gen.clone(), self.status.clone(), self.notice.clone());
        self.rt.spawn(async move {
            if pair_first {
                *link.lock().unwrap() = Link::Connecting(id.clone());
                ctx.request_repaint();
                if let Err(e) = backend::pair(&id).await {
                    *notice.lock().unwrap() = format!("Pairing failed: {e}. Press BOOT briefly on the adapter to open its pairing window and try again.");
                    *link.lock().unwrap() = Link::Disconnected;
                    ctx.request_repaint();
                    return;
                }
            }
            while gen_ref.load(Ordering::SeqCst) == generation {
                *link.lock().unwrap() = Link::Connecting(id.clone());
                ctx.request_repaint();
                match session::connect(&id).await {
                    Ok((dev, conn)) => {
                        let name = dev.info().name.clone();
                        *link.lock().unwrap() = Link::Connected { dev: dev.clone(), name, id: id.clone() };
                        notice.lock().unwrap().clear();
                        ctx.request_repaint();
                        // poll target-USB state while connected
                        let poll = async {
                            loop {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                if let Ok(s) = dev.status().await {
                                    *status.lock().unwrap() = Some(s);
                                    ctx.request_repaint();
                                }
                            }
                        };
                        tokio::select! { _ = dev.closed() => {}, _ = poll => {} }
                        *status.lock().unwrap() = None;
                        conn.disconnect().await;
                        *link.lock().unwrap() = Link::Failed("link lost, reconnecting…".into());
                        ctx.request_repaint();
                    }
                    Err(e) => {
                        *link.lock().unwrap() = Link::Failed(e.to_string());
                        ctx.request_repaint();
                    }
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        });
    }

    fn stop_link(&mut self) {
        self.link_gen.fetch_add(1, Ordering::SeqCst);
        if let Link::Connected { dev, .. } = self.link.lock().unwrap().clone() {
            let d = dev.clone();
            self.rt.spawn(async move { d.shutdown().await });
        }
        *self.link.lock().unwrap() = Link::Disconnected;
        self.cfg.last_device = None;
        self.cfg.save();
    }

    fn device(&self) -> Option<Device> {
        match &*self.link.lock().unwrap() {
            Link::Connected { dev, .. } if dev.is_connected() => Some(dev.clone()),
            _ => None,
        }
    }

    /// Ordered input pump: keystrokes must reach the adapter in the order they happened.
    fn pump(&mut self) -> Option<mpsc::UnboundedSender<Input>> {
        let (dev, id) = match &*self.link.lock().unwrap() {
            Link::Connected { dev, id, .. } if dev.is_connected() => (dev.clone(), id.clone()),
            _ => {
                self.input_tx = None;
                return None;
            }
        };
        if self.input_for.as_deref() != Some(&id) || self.input_tx.as_ref().is_none_or(|t| t.is_closed()) {
            let (tx, mut rx) = mpsc::unbounded_channel::<Input>();
            let notice = self.notice.clone();
            self.rt.spawn(async move {
                while let Some(ev) = rx.recv().await {
                    let r = match ev {
                        Input::Down(k) => dev.key_down(k).await,
                        Input::Up(k) => dev.key_up(k).await,
                        Input::Button(m, d) => dev.button(m, d).await,
                        Input::Scroll(v) => dev.scroll(v, 0).await,
                        Input::ReleaseAll => dev.release_all().await,
                    };
                    if let Err(e) = r {
                        *notice.lock().unwrap() = format!("input not delivered: {e}");
                    }
                }
            });
            self.input_tx = Some(tx);
            self.input_for = Some(id);
        }
        self.input_tx.clone()
    }

    fn open_video(&mut self) {
        let Some(d) = self.video_devices.get(self.video_sel) else { return };
        match Capture::open(&d.path) {
            Ok(c) => {
                self.cfg.last_video = Some(d.path.clone());
                self.cfg.save();
                *self.capture.lock().unwrap() = Some(c);
            }
            Err(e) => self.set_notice(format!("video: {e}")),
        }
    }

    fn end_capture(&mut self, ctx: &egui::Context) {
        if self.capturing {
            self.capturing = false;
            self.prev_mods.clear();
            if let Some(tx) = &self.input_tx {
                let _ = tx.send(Input::ReleaseAll); // release-all on every exit path
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        }
    }

    fn begin_capture(&mut self, ctx: &egui::Context) {
        if self.run_handle.as_ref().is_some_and(|r| !r.done.load(Ordering::SeqCst))
            || self.chord_busy.load(Ordering::SeqCst)
            || self.pump().is_none()
        {
            return;
        }
        self.capturing = true;
        // Captured keys must reach the target only: a focused local widget would otherwise take Enter/Space.
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::Locked));
        ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(false));
    }

    /// Forward this frame's input while captured. The release chord (Ctrl+Alt+Esc) is consumed here and
    /// never reaches the target.
    fn forward_input(&mut self, ctx: &egui::Context) {
        let Some(tx) = self.pump() else {
            self.end_capture(ctx);
            return;
        };
        let (events, mods, focused) = ctx.input(|i| (i.events.clone(), i.modifiers, i.focused));
        if !focused {
            self.end_capture(ctx);
            return;
        }
        let dev = self.device();
        let mut release = false;
        for ev in &events {
            match ev {
                egui::Event::Key { key, physical_key, pressed, repeat, modifiers, .. } => {
                    if *key == egui::Key::Escape && modifiers.ctrl && modifiers.alt {
                        release = true;
                        break;
                    }
                    if *repeat {
                        continue; // the target generates its own repeat from the held key
                    }
                    // modifier changes first, then the key
                    self.sync_mods(&tx, modifiers);
                    if let Some(k) = keymap::to_hid(physical_key.unwrap_or(*key)) {
                        let _ = tx.send(if *pressed { Input::Down(k) } else { Input::Up(k) });
                    }
                }
                egui::Event::PointerButton { button, pressed, .. } => {
                    let mask = match button {
                        egui::PointerButton::Primary => kvmit_hid::MOUSE_LEFT,
                        egui::PointerButton::Secondary => kvmit_hid::MOUSE_RIGHT,
                        egui::PointerButton::Middle => kvmit_hid::MOUSE_MIDDLE,
                        _ => continue,
                    };
                    let _ = tx.send(Input::Button(mask, *pressed));
                }
                egui::Event::MouseMoved(d) => {
                    if let Some(dev) = &dev {
                        dev.mouse_move(d.x.round() as i32, d.y.round() as i32);
                    }
                }
                egui::Event::MouseWheel { delta, .. } => {
                    let v = delta.y.round().clamp(-127.0, 127.0) as i8;
                    if v != 0 {
                        let _ = tx.send(Input::Scroll(v));
                    }
                }
                _ => {}
            }
        }
        if release {
            self.end_capture(ctx);
        } else {
            self.sync_mods(&tx, &mods);
        }
    }

    fn sync_mods(&mut self, tx: &mpsc::UnboundedSender<Input>, m: &egui::Modifiers) {
        let now = keymap::modifier_keys(m);
        for k in &self.prev_mods {
            if !now.contains(k) {
                let _ = tx.send(Input::Up(*k));
            }
        }
        for k in &now {
            if !self.prev_mods.contains(k) {
                let _ = tx.send(Input::Down(*k));
            }
        }
        self.prev_mods = now;
    }

    // ---- scripts ----

    fn select_script(&mut self, i: usize) {
        self.selected = Some(i);
        self.trust_ack = false;
        self.preview = None;
        self.var_values.clear();
        match Script::parse(&self.library[i].source) {
            Ok(s) => {
                self.script_err = None;
                self.script = Some(s);
            }
            Err(e) => {
                self.script_err = Some(e);
                self.script = None;
            }
        }
    }

    fn current_vars(&self) -> Vars {
        self.var_values.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    fn start_run(&mut self, script: Script, base_dir: std::path::PathBuf, vars: Vars, dry: bool, ctx: egui::Context) {
        if self.chord_busy.load(Ordering::SeqCst) {
            self.set_notice("Wait for the key chord to finish.");
            return;
        }
        let Some(dev) = self.device() else {
            self.set_notice("Not connected to an adapter.");
            return;
        };
        let ops = match compile(&script, &vars, &UsAnsi) {
            Ok(o) => o,
            Err(e) => {
                self.set_notice(format!("Cannot run: {e}"));
                return;
            }
        };
        self.end_capture(&ctx);
        let handle = RunHandle {
            cancel: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(false)),
            log: Arc::default(),
            confirm: Arc::default(),
        };
        let (cancel, done, log, confirm) = (handle.cancel.clone(), handle.done.clone(), handle.log.clone(), handle.confirm.clone());
        let (rt, capture) = (self.rt.handle().clone(), self.capture.clone());
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let log2 = log.clone();
            let ctx3 = ctx2.clone();
            let mut host = ScriptHost {
                rt,
                dev,
                capture: Some(capture),
                base_dir,
                cancel,
                confirm: Box::new(move |msg| {
                    let (tx, rx) = std_mpsc::channel();
                    *confirm.lock().unwrap() = Some((msg.to_string(), tx));
                    ctx3.request_repaint();
                    rx.recv().unwrap_or(false)
                }),
                on_event: Box::new(move |ev| {
                    let line = match &ev {
                        RunEvent::Started { steps } => format!("started, {steps} steps"),
                        RunEvent::StepStarted { index, kind, detail } => format!("step {}: {kind} {detail}", index + 1),
                        RunEvent::WaitPolling { .. } => return,
                        RunEvent::Finished => "finished".into(),
                        RunEvent::Aborted { reason } => format!("stopped: {reason}"),
                    };
                    log2.lock().unwrap().push(line);
                    ctx2.request_repaint();
                }),
            };
            let opts = RunOptions { dry_run: dry, ..Default::default() };
            let _ = run(&ops, &mut host, &opts);
            done.store(true, Ordering::SeqCst);
        });
        self.run_log.clear();
        self.run_handle = Some(handle);
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // finished runs: flush log
        if let Some(h) = &self.run_handle {
            self.run_log = h.log.lock().unwrap().clone();
        }
        if self.capturing {
            self.forward_input(ctx);
        }

        // new video frame → texture
        let frame = self.capture.lock().unwrap().as_ref().and_then(|c| c.latest());
        if let Some(f) = frame {
            if f.seq != self.last_seq {
                self.last_seq = f.seq;
                let img = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
                match &mut self.texture {
                    Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                    None => self.texture = Some(ctx.load_texture("video", img, egui::TextureOptions::LINEAR)),
                }
            }
            ctx.request_repaint_after(Duration::from_millis(16));
        }

        let link = self.link.lock().unwrap().clone();
        let status = self.status.lock().unwrap().clone();
        let has_video = self.capture.lock().unwrap().is_some() && self.texture.is_some();

        // ---- status strip: one place, never ambiguous ----
        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let dot = |ui: &mut egui::Ui, color: egui::Color32, text: String| {
                    ui.colored_label(color, "●");
                    ui.label(text);
                    ui.separator();
                };
                let (green, amber, red, gray) = (egui::Color32::from_rgb(70, 190, 90), egui::Color32::from_rgb(230, 170, 40), egui::Color32::from_rgb(220, 70, 60), egui::Color32::GRAY);
                match &link {
                    Link::Connected { name, .. } => dot(ui, green, format!("Adapter: {name}")),
                    Link::Connecting(id) => dot(ui, amber, format!("Adapter: connecting to {id}…")),
                    Link::Failed(_) => dot(ui, red, "Adapter: not connected (retrying)".into()),
                    Link::Disconnected => dot(ui, gray, "Adapter: none selected".into()),
                }
                match (&link, &status) {
                    (Link::Connected { .. }, Some(s)) if s.hid_mounted => dot(ui, green, "Target USB: connected".into()),
                    (Link::Connected { .. }, Some(_)) => dot(ui, red, "Target USB: not enumerated".into()),
                    (Link::Connected { .. }, None) => dot(ui, amber, "Target USB: checking…".into()),
                    _ => dot(ui, gray, "Target USB: —".into()),
                }
                dot(ui, if has_video { green } else { gray }, if has_video { "Video: signal".into() } else { "Video: none".into() });
                if self.capturing {
                    ui.colored_label(red, "● INPUT CAPTURED — Ctrl+Alt+Esc to release");
                } else {
                    ui.label("Input: not captured");
                }
            });
            let n = self.notice.lock().unwrap().clone();
            let l = if let Link::Failed(e) = &link { e.clone() } else { String::new() };
            if !n.is_empty() || !l.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(230, 170, 40), if n.is_empty() { l } else { n });
            }
        });

        // confirm dialog from a running script
        if let Some(h) = &self.run_handle {
            let pending = h.confirm.lock().unwrap().clone();
            if let Some((msg, tx)) = pending {
                egui::Window::new("Script paused").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                    ui.label(msg);
                    ui.horizontal(|ui| {
                        if ui.button("Continue").clicked() {
                            let _ = tx.send(true);
                            *h.confirm.lock().unwrap() = None;
                        }
                        if ui.button("Abort").clicked() {
                            let _ = tx.send(false);
                            *h.confirm.lock().unwrap() = None;
                        }
                    });
                });
            }
        }

        egui::SidePanel::left("controls").default_width(320.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.controls(ui, ctx, &link));
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            match (&self.texture, has_video) {
                (Some(tex), true) => {
                    let avail = ui.available_size();
                    let ratio = tex.size()[0] as f32 / tex.size()[1] as f32;
                    let size = if avail.x / avail.y > ratio { egui::vec2(avail.y * ratio, avail.y) } else { egui::vec2(avail.x, avail.x / ratio) };
                    let resp = ui.centered_and_justified(|ui| ui.add(egui::Image::new((tex.id(), size)).sense(egui::Sense::click()))).inner;
                    if self.capturing {
                        ui.painter().rect_stroke(resp.rect, 0.0, egui::Stroke::new(4.0, egui::Color32::from_rgb(220, 70, 60)), egui::StrokeKind::Inside);
                    } else if resp.clicked() {
                        self.begin_capture(ctx);
                    } else if resp.hovered() {
                        resp.on_hover_text("Click to capture keyboard and mouse");
                    }
                }
                _ => {
                    // No picture, but input can still be driven (e.g. watching the target's own screen).
                    let can_capture = self.device().is_some();
                    let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click());
                    let msg = if self.video_devices.is_empty() { "No capture device found. Plug in an HDMI capture card." } else { "Open a capture device in the left panel." };
                    let hint = if self.capturing { "Input captured — Ctrl+Alt+Esc to release" } else if can_capture { "Click here to capture keyboard and mouse without video" } else { "Connect an adapter to send input" };
                    ui.painter().text(rect.center() - egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), ui.visuals().text_color());
                    ui.painter().text(rect.center() + egui::vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, hint, egui::FontId::proportional(13.0), ui.visuals().weak_text_color());
                    if self.capturing {
                        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(4.0, egui::Color32::from_rgb(220, 70, 60)), egui::StrokeKind::Inside);
                    } else if resp.clicked() && can_capture {
                        self.begin_capture(ctx);
                    }
                }
            }
        });

        if ctx.input(|i| i.viewport().close_requested()) {
            self.end_capture(ctx);
            if let Some(d) = self.device() {
                let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_millis(800), d.shutdown()).await });
            }
        }
    }
}

impl App {
    fn controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, link: &Link) {
        egui::CollapsingHeader::new("Adapter").default_open(true).show(ui, |ui| {
            match link {
                Link::Connected { name, id, .. } => {
                    ui.label(format!("{name}\n{id}"));
                    ui.horizontal(|ui| {
                        if ui.button("Release all keys").clicked() {
                            if let Some(d) = self.device() {
                                self.rt.spawn(async move { let _ = d.release_all().await; });
                            }
                        }
                        if ui.button("Disconnect").clicked() {
                            self.end_capture(ctx);
                            self.stop_link();
                        }
                    });
                }
                _ => {
                    let scanning = matches!(&*self.scan.lock().unwrap(), Scan::Scanning);
                    if ui.add_enabled(!scanning, egui::Button::new(if scanning { "Scanning…" } else { "Scan for adapters" })).clicked() {
                        *self.scan.lock().unwrap() = Scan::Scanning;
                        let (scan, ctx2) = (self.scan.clone(), ctx.clone());
                        self.rt.spawn(async move {
                            *scan.lock().unwrap() = match backend::scan(Duration::from_secs(5)).await {
                                Ok(f) => Scan::Done(f),
                                Err(e) => Scan::Error(e.to_string()),
                            };
                            ctx2.request_repaint();
                        });
                    }
                    let mut chosen: Option<(String, bool)> = None;
                    match &*self.scan.lock().unwrap() {
                        Scan::Done(list) if list.is_empty() => {
                            ui.label("None found. New adapter? Press BOOT briefly to open its pairing window, then scan again.");
                        }
                        Scan::Done(list) => {
                            for f in list {
                                ui.horizontal(|ui| {
                                    ui.label(format!("{} ({} dBm)", f.name, f.rssi.map(|r| r.to_string()).unwrap_or("?".into())));
                                    if ui.button("Pair & connect").clicked() {
                                        chosen = Some((f.id.clone(), true));
                                    }
                                    if ui.button("Connect").clicked() {
                                        chosen = Some((f.id.clone(), false));
                                    }
                                });
                            }
                        }
                        Scan::Error(e) => {
                            ui.colored_label(egui::Color32::from_rgb(220, 70, 60), e);
                        }
                        _ => {}
                    }
                    if let Some((id, pair)) = chosen {
                        self.start_link(id, ctx.clone(), pair);
                    }
                }
            }
        });

        egui::CollapsingHeader::new("Video").default_open(true).show(ui, |ui| {
            if self.video_devices.is_empty() {
                ui.label("No capture devices.");
            } else {
                egui::ComboBox::from_label("Device")
                    .selected_text(self.video_devices.get(self.video_sel).map(|d| d.name.clone()).unwrap_or_default())
                    .show_ui(ui, |ui| {
                        for (i, d) in self.video_devices.iter().enumerate() {
                            ui.selectable_value(&mut self.video_sel, i, format!("{} ({})", d.name, d.path));
                        }
                    });
                if ui.button("Open").clicked() {
                    *self.capture.lock().unwrap() = None;
                    self.texture = None;
                    self.open_video();
                }
                if let Some(c) = self.capture.lock().unwrap().as_ref() {
                    ui.label(format!("{}×{} @ {} fps {}", c.mode.width, c.mode.height, c.mode.fps, if c.mode.mjpeg { "MJPEG" } else { "YUYV" }));
                }
            }
            if ui.add_enabled(self.device().is_some(), egui::Button::new("Capture keyboard & mouse")).clicked() {
                self.begin_capture(ctx);
            }
        });

        egui::CollapsingHeader::new("Send keys").show(ui, |ui| {
            ui.small("Keys your own OS would intercept, or that the capture view can't see.");
            let running = self.run_handle.as_ref().is_some_and(|r| !r.done.load(Ordering::SeqCst));
            // One input producer at a time: no chords during capture, a script, or another chord.
            let on = self.device().is_some() && !self.capturing && !running && !self.chord_busy.load(Ordering::SeqCst);
            const CHORDS: &[(&str, &[&str])] = &[
                ("Ctrl+Alt+Del", &["CTRL", "ALT", "DEL"]),
                ("Win", &["WIN"]),
                ("Alt+Tab", &["ALT", "TAB"]),
                ("Alt+F4", &["ALT", "F4"]),
                ("Ctrl+Esc", &["CTRL", "ESC"]),
                ("Win+R", &["WIN", "R"]),
                ("PrintScreen", &["PRTSC"]),
                ("Menu", &["MENU"]),
                ("CapsLock", &["CAPSLOCK"]),
                ("NumLock", &["NUMLOCK"]),
                ("Pause", &["PAUSE"]),
                ("ScrollLock", &["SCROLLLOCK"]),
            ];
            ui.horizontal_wrapped(|ui| {
                for (label, keys) in CHORDS {
                    if ui.add_enabled(on, egui::Button::new(*label)).clicked() {
                        if let Some(d) = self.device() {
                            let ks: Vec<Key> = keys.iter().filter_map(|n| kvmit_hid::parse_key(n)).collect();
                            let (busy, notice, label) = (self.chord_busy.clone(), self.notice.clone(), *label);
                            busy.store(true, Ordering::SeqCst);
                            self.rt.spawn(async move {
                                let mut ok = true;
                                for k in &ks {
                                    if d.key_down(*k).await.is_err() {
                                        ok = false;
                                        break;
                                    }
                                }
                                for k in ks.iter().rev() {
                                    ok &= d.key_up(*k).await.is_ok();
                                }
                                if !ok {
                                    // A failed key-up leaves the key held on the target: release everything.
                                    let released = d.release_all().await.is_ok();
                                    *notice.lock().unwrap() = format!(
                                        "{label} was not delivered cleanly; {}",
                                        if released { "all keys released." } else { "release failed: check the adapter link." }
                                    );
                                }
                                busy.store(false, Ordering::SeqCst);
                            });
                        }
                    }
                }
            });
        });

        egui::CollapsingHeader::new("Type text").show(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.type_text).password(self.type_secret).hint_text("text to type on the target"));
            ui.checkbox(&mut self.type_secret, "Secret (masked, never logged)");
            if ui.add_enabled(self.device().is_some() && !self.type_text.is_empty(), egui::Button::new("Type")).clicked() {
                let mut s = Script::default();
                s.vars.insert("t".into(), kvmit_script::VarDef { secret: self.type_secret, ..Default::default() });
                s.steps.push(if self.type_secret { Step::SecretText("{{t}}".into()) } else { Step::Text("{{t}}".into()) });
                let vars: Vars = [("t".to_string(), std::mem::take(&mut self.type_text))].into();
                self.start_run(s, std::path::PathBuf::from("."), vars, false, ctx.clone());
            }
        });

        egui::CollapsingHeader::new("Scripts").default_open(true).show(ui, |ui| {
            let names: Vec<String> = self.library.iter().map(|e| e.name.clone()).collect();
            for (i, n) in names.iter().enumerate() {
                if ui.selectable_label(self.selected == Some(i), n).clicked() {
                    self.select_script(i);
                }
            }
            if ui.button("Reload folder").clicked() {
                self.library = library::load_all(&self.cfg.script_dir());
                self.selected = None;
                self.script = None;
            }
            ui.small(format!("Folder: {}", self.cfg.script_dir().display()));
            if let Some(e) = &self.script_err {
                ui.colored_label(egui::Color32::from_rgb(220, 70, 60), e);
            }
            if let (Some(script), Some(sel)) = (self.script.clone(), self.selected) {
                ui.separator();
                ui.strong(&script.name);
                ui.label(&script.description);
                for (name, def) in &script.vars {
                    let v = self.var_values.entry(name.clone()).or_insert_with(|| def.default.clone().unwrap_or_default());
                    ui.horizontal(|ui| {
                        ui.label(def.prompt.as_deref().unwrap_or(name));
                        ui.add(egui::TextEdit::singleline(v).password(def.secret));
                    });
                    if def.secret {
                        ui.small("Secret: held in memory only, never saved or logged.");
                    }
                }
                let vars = self.current_vars();
                let external = self.library[sel].path.is_some();
                match preview(&script, &vars, &UsAnsi, &RunOptions::default()) {
                    Ok(p) => {
                        ui.small(format!(
                            "{} steps · {} chars typed · {} secret · ~{} s typing{}{}",
                            p.steps, p.typed_chars, p.secret_chars, p.estimated_typing.as_secs(),
                            if p.needs_video { " · needs video" } else { "" },
                            if p.has_confirm { " · has prompts" } else { "" }
                        ));
                        for s in &p.suspicious {
                            ui.colored_label(egui::Color32::from_rgb(230, 170, 40), format!("⚠ looks like a command: {s}"));
                        }
                        self.preview = Some(Ok(p));
                    }
                    Err(e) => {
                        ui.small(format!("Not ready: {e}"));
                        self.preview = Some(Err(e.to_string()));
                    }
                }
                if external {
                    ui.checkbox(&mut self.trust_ack, "I wrote or reviewed this script — it will type into the target");
                }
                let running = self.run_handle.as_ref().is_some_and(|r| !r.done.load(Ordering::SeqCst));
                let ready = matches!(self.preview, Some(Ok(_))) && (!external || self.trust_ack);
                let base = self.library[sel].path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| self.cfg.script_dir());
                ui.horizontal(|ui| {
                    if ui.add_enabled(ready && !running && self.device().is_some(), egui::Button::new("▶ Run")).clicked() {
                        self.start_run(script.clone(), base.clone(), vars.clone(), false, ctx.clone());
                    }
                    if ui.add_enabled(matches!(self.preview, Some(Ok(_))) && !running && self.device().is_some(), egui::Button::new("Dry run")).clicked() {
                        self.start_run(script.clone(), base.clone(), vars.clone(), true, ctx.clone());
                    }
                    if ui.add_enabled(running, egui::Button::new("■ Abort")).clicked() {
                        if let Some(h) = &self.run_handle {
                            h.cancel.store(true, Ordering::SeqCst);
                        }
                    }
                });
            }
            if !self.run_log.is_empty() {
                ui.separator();
                ui.strong("Run log (no typed text or secrets)");
                egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                    for l in &self.run_log {
                        ui.small(l);
                    }
                });
            }
        });
    }
}

pub fn run_gui() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_title("kvm-it"),
        ..Default::default()
    };
    eframe::run_native("kvm-it", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
