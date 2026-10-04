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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// A "Send keys" chord: all downs, then ups in reverse. Runs inside the pump so it can never interleave
    /// with captured input, a capture's closing RELEASE_ALL, or the Release all button.
    Chord(&'static str, Vec<Key>),
}

/// Sender into the ordered input pump. Every queued item is counted until the pump has finished it, so the UI
/// can tell when input is fully idle (nothing queued, nothing in flight) before starting another producer.
#[derive(Clone)]
struct InputTx {
    tx: mpsc::UnboundedSender<Input>,
    pending: Arc<AtomicUsize>,
}

impl InputTx {
    fn send(&self, i: Input) -> Result<(), ()> {
        self.pending.fetch_add(1, Ordering::SeqCst);
        self.tx.send(i).map_err(|_| {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        })
    }
    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
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
    input_tx: Option<InputTx>,
    /// The connection session the current pump drives; a reconnect gets a new pump.
    input_for: Option<Device>,
    capturing: bool,
    prev_mods: Vec<Key>,
    /// OS-level keyboard capture while input is captured (Windows); `None` elsewhere, where egui's key events are used.
    grab: Option<crate::syskeys::Grab>,
    /// Capture was released earlier in this frame: nothing in the same frame may start it again (a click on the
    /// picture that arrived with the release chord would otherwise re-capture immediately).
    released_this_frame: bool,
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
    flash: Option<crate::flashwiz::Wizard>,
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
            prev_mods: Vec::new(),
            grab: None,
            released_this_frame: false,
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
            flash: None,
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
                    *notice.lock().unwrap() = format!("Pairing failed: {e}. Re-plug the adapter (15 s pairing window) or press BOOT briefly on it, then try again.");
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
                            let mut last_held = 0u8;
                            loop {
                                tokio::time::sleep(Duration::from_secs(2)).await;
                                if let Ok(s) = dev.status().await {
                                    if s.keys != last_held {
                                        debug_log(&format!("adapter reports {} key(s) held on the target", s.keys));
                                        last_held = s.keys;
                                    }
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
    /// True when no input producer is active: not capturing, no script/Type run, nothing queued in the pump.
    fn input_idle(&self) -> bool {
        !self.capturing
            && self.run_handle.as_ref().is_none_or(|r| r.done.load(Ordering::SeqCst))
            && self.input_tx.as_ref().is_none_or(|t| t.pending.load(Ordering::SeqCst) == 0)
    }

    fn pump(&mut self) -> Option<InputTx> {
        let dev = match &*self.link.lock().unwrap() {
            Link::Connected { dev, .. } if dev.is_connected() => dev.clone(),
            _ => {
                self.input_tx = None;
                self.input_for = None;
                return None;
            }
        };
        // One pump per connection session: after a reconnect (same adapter, new session) the old pump still holds
        // the closed device, so it is retired and a fresh pump with its own pending counter takes over.
        if self.input_for.as_ref().is_none_or(|d| !d.same_session(&dev)) || self.input_tx.as_ref().is_none_or(|t| t.is_closed()) {
            self.input_for = Some(dev.clone());
            let (tx, mut rx) = mpsc::unbounded_channel::<Input>();
            let notice = self.notice.clone();
            let pending = Arc::new(AtomicUsize::new(0));
            let done = pending.clone();
            self.rt.spawn(async move {
                while let Some(ev) = rx.recv().await {
                    let r = match ev {
                        Input::Down(k) => dev.key_down(k).await,
                        Input::Up(k) => dev.key_up(k).await,
                        Input::Button(m, d) => dev.button(m, d).await,
                        Input::Scroll(v) => dev.scroll(v, 0).await,
                        Input::ReleaseAll => {
                            let r = dev.release_all().await;
                            if r.is_err() {
                                // Keys may still be held on the target, and our keepalives would keep them held:
                                // close the session so the adapter's link-drop release takes over.
                                dev.shutdown().await;
                                *notice.lock().unwrap() = "Releasing the keys failed: disconnected so the adapter releases them.".into();
                            }
                            r
                        }
                        Input::Chord(label, ks) => {
                            let mut ok = true;
                            for k in &ks {
                                if dev.key_down(*k).await.is_err() {
                                    ok = false;
                                    break;
                                }
                            }
                            for k in ks.iter().rev() {
                                ok &= dev.key_up(*k).await.is_ok();
                            }
                            if !ok {
                                // A failed key-up leaves the key held on the target. Release everything; if even
                                // that fails, close the session so the adapter's link-drop/keepalive release takes
                                // over (our keepalives would otherwise keep the held key alive).
                                *notice.lock().unwrap() = if dev.release_all().await.is_ok() {
                                    format!("{label} was not delivered cleanly; all keys released.")
                                } else {
                                    dev.shutdown().await;
                                    format!("{label} failed and keys could not be released: disconnected so the adapter releases them.")
                                };
                            }
                            Ok(())
                        }
                    };
                    if let Err(e) = r {
                        debug_log(&format!("input not delivered: {e}"));
                        *notice.lock().unwrap() = format!("input not delivered: {e}");
                    }
                    done.fetch_sub(1, Ordering::SeqCst);
                }
            });
            self.input_tx = Some(InputTx { tx, pending });
        }
        self.input_tx.clone()
    }

    /// Switch to device `i`: close whatever is open, then open that one, so the old picture never lingers.
    fn switch_video(&mut self, i: usize) {
        if i >= self.video_devices.len() {
            return;
        }
        self.video_sel = i;
        // take the old capture out and drop it after the lock is released: closing one can wait on a stalled card, and
        // scripts need this mutex for `screen()`
        let old = self.capture.lock().unwrap().take();
        drop(old);
        self.texture = None;
        self.last_seq = 0; // a still source always reports the same sequence number
        self.open_video();
    }

    /// Enumerate the capture devices again (a card plugged in or reset after the app started), keeping the selection
    /// on the same device when it is still there.
    fn rescan_video(&mut self) {
        let previous = self.video_devices.get(self.video_sel).map(|d| d.path.clone());
        self.video_devices = kvmit_video::list_devices();
        self.video_sel = selection_after_rescan(previous.as_deref(), &self.video_devices);
    }

    fn open_video(&mut self) {
        let Some(d) = self.video_devices.get(self.video_sel) else { return };
        match Capture::open(&d.path) {
            Ok(c) => {
                if !d.path.starts_with(kvmit_video::DEMO_PREFIX) {
                    // the synthetic demo source is never the remembered card
                    self.cfg.last_video = Some(d.path.clone());
                    self.cfg.save();
                }
                *self.capture.lock().unwrap() = Some(c);
            }
            Err(e) => self.set_notice(format!("video: {e}")),
        }
    }

    fn end_capture(&mut self, ctx: &egui::Context) {
        if self.capturing {
            self.capturing = false;
            self.grab = None; // unhooks: the controller's OS gets its keys back
            self.prev_mods.clear();
            if let Some(tx) = &self.input_tx {
                let _ = tx.send(Input::ReleaseAll); // release-all on every exit path
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorGrab(egui::CursorGrab::None));
            ctx.send_viewport_cmd(egui::ViewportCommand::CursorVisible(true));
        }
    }

    fn begin_capture(&mut self, ctx: &egui::Context) {
        if self.released_this_frame {
            return;
        }
        if self.run_handle.as_ref().is_some_and(|r| !r.done.load(Ordering::SeqCst)) || self.pump().is_none() {
            return;
        }
        self.capturing = true;
        self.grab = crate::syskeys::Grab::start({
            let ctx = ctx.clone();
            move || ctx.request_repaint() // keyboard events arrive off-thread: wake the GUI to forward (and release) promptly
        });
        debug_log(&format!("capture begins (OS keyboard grab: {})", if self.grab.is_some() { "on" } else { "off" }));
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
            debug_log("capture ends: the adapter link is gone");
            self.end_capture(ctx);
            return;
        };
        let (events, mods, focused) = ctx.input(|i| (i.events.clone(), i.modifiers, i.focused));
        if !focused {
            debug_log("capture ends: the window lost focus");
            self.end_capture(ctx);
            return;
        }
        let dev = self.device();
        let mut release = false;
        // With an OS-level grab the keyboard comes from it alone (it also sees the keys the OS would otherwise
        // keep for itself: Win, Alt+Tab, ...); egui's key events are then ignored so nothing is sent twice.
        let hooked = self.grab.is_some();
        if let Some(g) = &mut self.grab {
            // The release chord is a terminal boundary: nothing queued after it is forwarded, keys or clicks.
            let (forward, released) = crate::syskeys::until_release(g.drain());
            for ev in forward {
                match ev {
                    crate::syskeys::Event::Down(k) => {
                        let _ = tx.send(Input::Down(k));
                    }
                    crate::syskeys::Event::Up(k) => {
                        let _ = tx.send(Input::Up(k));
                    }
                    crate::syskeys::Event::Release => {}
                }
            }
            if released {
                debug_log("capture ends: release chord");
                self.released_this_frame = true;
                self.end_capture(ctx);
                return;
            }
        }
        for ev in &events {
            // A fresh press of a key the grab would swallow reached egui: the grab is no longer in control (Windows
            // removed the hook, or it stopped swallowing for the chord or a missed heartbeat and the release
            // notice is still on its way). End capture, which releases everything held on the target, instead of
            // guessing which keys are down: continuing on egui's keys could leave a forwarded modifier stuck.
            if hooked && grab_should_have_swallowed(ev) {
                debug_log("capture ends: the keyboard grab is no longer swallowing keys");
                self.set_notice("Keyboard capture ended: Windows stopped routing keys through kvm-it's keyboard grab. Click the picture to capture again.");
                self.released_this_frame = true;
                self.end_capture(ctx);
                return;
            }
            match ev {
                egui::Event::Key { .. } if hooked => {}
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
            debug_log("capture ends: release chord");
            self.released_this_frame = true;
            self.end_capture(ctx);
        } else if !hooked {
            self.sync_mods(&tx, &mods);
        } else {
            // The grab's heartbeat is sent from here; keep this running even when nothing else repaints.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn sync_mods(&mut self, tx: &InputTx, m: &egui::Modifiers) {
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
        if !self.input_idle() {
            self.set_notice("Input is busy (capture, a running script, or queued keys): wait for it to finish.");
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
        self.released_this_frame = false;
        if self.capturing {
            self.forward_input(ctx);
        }

        // a capture whose card vanished must not keep showing its last frame as if it were live
        if self.capture.lock().unwrap().as_ref().is_some_and(|c| c.failed()) {
            *self.capture.lock().unwrap() = None;
            self.texture = None;
            self.set_notice("Video stopped: the capture card was unplugged, reset or is in use by another app. Open it again from the Video button.");
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

        // ---- top bar: status chips that open the controls they describe. Green = working, amber = in progress,
        // red = broken, grey = idle. While input is captured every key belongs to the target, so only the
        // capture indicator is live. ----
        let idle_ui = !self.capturing;
        let running = self.run_handle.as_ref().is_some_and(|r| !r.done.load(Ordering::SeqCst));
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled_ui(idle_ui, |ui| {
                    let (h, t) = match &link {
                        Link::Connected { name, .. } => (Health::Good, format!("Adapter: {name}")),
                        Link::Connecting(id) => (Health::Working, format!("Adapter: connecting to {id}…")),
                        Link::Failed(_) => (Health::Bad, "Adapter: not connected (retrying)".into()),
                        Link::Disconnected => (Health::Idle, "Adapter: none — click to connect".into()),
                    };
                    let r = chip(ui, h, t);
                    popup(&r, 380.0, |ui| self.adapter_ui(ui, ctx, &link));

                    let (h, t) = match (&link, &status) {
                        (Link::Connected { .. }, Some(s)) if s.hid_mounted => (Health::Good, "Target USB: connected"),
                        (Link::Connected { .. }, Some(_)) => (Health::Bad, "Target USB: not enumerated"),
                        (Link::Connected { .. }, None) => (Health::Working, "Target USB: checking…"),
                        _ => (Health::Idle, "Target USB: —"),
                    };
                    chip(ui, h, t);

                    let mode = self.capture.lock().unwrap().as_ref().map(|c| c.mode);
                    let (h, t) = match mode {
                        Some(m) if has_video => (Health::Good, format!("Video: {}×{} @ {} fps", m.width, m.height, m.fps)),
                        Some(_) => (Health::Working, "Video: waiting for a frame…".into()),
                        None if self.video_devices.is_empty() => (Health::Bad, "Video: no capture card found".into()),
                        None => (Health::Idle, "Video: not opened — click to open".into()),
                    };
                    let r = chip(ui, h, t);
                    popup(&r, 420.0, |ui| self.video_ui(ui, ctx));
                });

                if self.capturing {
                    chip(ui, Health::Bad, "INPUT CAPTURED — Ctrl+Alt+Esc to release");
                } else {
                    let can = self.device().is_some();
                    let r = ui.add_enabled_ui(can, |ui| chip(ui, Health::Idle, "Input: click to capture")).inner;
                    if r.clicked() && can {
                        self.begin_capture(ctx);
                    }
                }

                ui.separator();
                ui.add_enabled_ui(idle_ui, |ui| {
                    let r = ui.button("Keys…");
                    popup(&r, 360.0, |ui| self.keys_ui(ui));
                    let r = ui.button("Type…");
                    popup(&r, 340.0, |ui| self.text_ui(ui, ctx));
                    let r = ui.button(if running { "Scripts…  (running)" } else { "Scripts…" });
                    popup(&r, 460.0, |ui| self.scripts_ui(ui, ctx));
                });
            });
            let n = self.notice.lock().unwrap().clone();
            let l = if let Link::Failed(e) = &link { e.clone() } else { String::new() };
            if !n.is_empty() || !l.is_empty() {
                ui.colored_label(AMBER, if n.is_empty() { l } else { n });
            }
            ui.add_space(2.0);
        });

        // the flasher window (opened from the Adapter popup); it owns its own progress, so it can stay up while idle
        if self.flash.is_some() && !self.capturing {
            let keep = self.flash.as_mut().is_some_and(|w| w.show(ctx));
            if !keep {
                self.flash = None;
            }
        }

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

        // run progress outlives any popup: a strip along the bottom while a script runs or has output
        if running || !self.run_log.is_empty() {
            egui::TopBottomPanel::bottom("runlog").resizable(true).show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.strong(if running { "Script running…" } else { "Script finished" });
                    ui.weak("(no typed text or secrets are logged)");
                    if running {
                        if ui.button("■ Abort").clicked() {
                            if let Some(h) = &self.run_handle {
                                h.cancel.store(true, Ordering::SeqCst);
                            }
                        }
                    } else if ui.button("Clear").clicked() {
                        self.run_handle = None;
                        self.run_log.clear();
                    }
                });
                egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                    for l in &self.run_log {
                        ui.small(l);
                    }
                });
            });
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            match (&self.texture, has_video) {
                (Some(tex), true) => {
                    let avail = ui.available_size();
                    let ratio = tex.size()[0] as f32 / tex.size()[1] as f32;
                    let size = if avail.x / avail.y > ratio { egui::vec2(avail.y * ratio, avail.y) } else { egui::vec2(avail.x, avail.x / ratio) };
                    let resp = ui.centered_and_justified(|ui| ui.add(egui::Image::new((tex.id(), size)).sense(egui::Sense::click()))).inner;
                    if self.capturing {
                        ui.painter().rect_stroke(resp.rect, 0.0, egui::Stroke::new(4.0_f32, egui::Color32::from_rgb(220, 70, 60)), egui::StrokeKind::Inside);
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
                    let msg = if self.video_devices.is_empty() { "No capture device found. Plug in an HDMI capture card." } else { "Open the capture card from the Video button above." };
                    let hint = if self.capturing { "Input captured — Ctrl+Alt+Esc to release" } else if can_capture { "Click here to capture keyboard and mouse without video" } else { "Connect an adapter to send input" };
                    ui.painter().text(rect.center() - egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), ui.visuals().text_color());
                    ui.painter().text(rect.center() + egui::vec2(0.0, 14.0), egui::Align2::CENTER_CENTER, hint, egui::FontId::proportional(13.0), ui.visuals().weak_text_color());
                    if self.capturing {
                        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(4.0_f32, egui::Color32::from_rgb(220, 70, 60)), egui::StrokeKind::Inside);
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
    // ---- popup bodies: what used to be the left sidebar, one method per top-bar button ----
    fn adapter_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, link: &Link) {
        match link {
            Link::Connected { name, id, .. } => {
                ui.label(format!("{name}\n{id}"));
                ui.horizontal(|ui| {
                    if ui.button("Release all keys").clicked() {
                        // Through the pump, so it lands after (not between) anything already queued.
                        if let Some(tx) = self.pump() {
                            let _ = tx.send(Input::ReleaseAll);
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
                        *scan.lock().unwrap() = match backend::scan(Duration::from_secs(backend::DEFAULT_SCAN_SECS)).await {
                            Ok(f) => Scan::Done(f),
                            Err(e) => Scan::Error(e.to_string()),
                        };
                        ctx2.request_repaint();
                    });
                }
                let mut chosen: Option<(String, bool)> = None;
                match &*self.scan.lock().unwrap() {
                    Scan::Done(list) if list.is_empty() => {
                        ui.label("None found. New adapter? Re-plug it (it can pair for 15 s) or press BOOT briefly, then scan again.");
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
        ui.separator();
        if ui.button("Flash adapter…").on_hover_text("Write the adapter's firmware through its COM USB port").clicked() {
            self.flash = Some(crate::flashwiz::Wizard::new());
        }
    }

    fn video_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.strong("Capture device");
            if ui.small_button("Rescan").clicked() {
                self.rescan_video();
            }
        });
        if self.video_devices.is_empty() {
            ui.label("No capture devices found. Plug in an HDMI capture card, then Rescan.");
        } else {
            // One row per device, right here: a dropdown would be a second popup, and clicking one of its entries
            // counts as a click outside this popup, which closes it before the choice is applied.
            let open = self.capture.lock().unwrap().as_ref().map(|c| c.info.path.clone());
            let mut chosen = None;
            for (i, d) in self.video_devices.iter().enumerate() {
                let is_open = open.as_deref() == Some(d.path.as_str());
                let text = if is_open { format!("{}  (open; click to reopen)", d.name) } else { d.name.clone() };
                if ui.selectable_label(is_open, text).on_hover_text(&d.path).clicked() {
                    chosen = Some(i);
                }
            }
            if let Some(i) = chosen {
                self.switch_video(i);
            }
            if let Some(c) = self.capture.lock().unwrap().as_ref() {
                let kind = if c.info.path.starts_with("demo:") { "still picture" } else if c.mode.mjpeg { "MJPEG" } else { "YUYV" };
                ui.label(format!("{}×{} @ {} fps {}", c.mode.width, c.mode.height, c.mode.fps, kind));
            }
        }
        if ui.add_enabled(self.device().is_some(), egui::Button::new("Capture keyboard & mouse")).clicked() {
            self.begin_capture(ctx);
        }
    }

    fn keys_ui(&mut self, ui: &mut egui::Ui) {
        ui.small("Keys your own OS would intercept, or that the capture view can't see.");
        // One input producer at a time: no chords during capture, a script, or while the pump is busy.
        let on = self.device().is_some() && self.input_idle();
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
                    let ks: Vec<Key> = keys.iter().filter_map(|n| kvmit_hid::parse_key(n)).collect();
                    if let Some(tx) = self.pump() {
                        let _ = tx.send(Input::Chord(label, ks));
                    }
                }
            }
        });
    }

    fn text_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add(egui::TextEdit::singleline(&mut self.type_text).password(self.type_secret).hint_text("text to type on the target"));
        ui.checkbox(&mut self.type_secret, "Secret (masked, never logged)");
        if ui.add_enabled(self.device().is_some() && !self.type_text.is_empty() && self.input_idle(), egui::Button::new("Type")).clicked() {
            let mut s = Script::default();
            s.vars.insert("t".into(), kvmit_script::VarDef { secret: self.type_secret, ..Default::default() });
            s.steps.push(if self.type_secret { Step::SecretText("{{t}}".into()) } else { Step::Text("{{t}}".into()) });
            let vars: Vars = [("t".to_string(), std::mem::take(&mut self.type_text))].into();
            self.start_run(s, std::path::PathBuf::from("."), vars, false, ctx.clone());
        }
    }

    fn scripts_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
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
    }
}

const GREEN: egui::Color32 = egui::Color32::from_rgb(46, 160, 67);
const AMBER: egui::Color32 = egui::Color32::from_rgb(214, 150, 30);
const RED: egui::Color32 = egui::Color32::from_rgb(200, 60, 50);

/// How a part of the setup is doing; drives a status chip's colour.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Health {
    Good,
    Working,
    Bad,
    Idle,
}

/// A status chip: a button filled with its health colour (grey/neutral when idle), so state reads at a glance.
fn chip(ui: &mut egui::Ui, h: Health, text: impl Into<String>) -> egui::Response {
    let text: String = text.into();
    let fill = match h {
        Health::Good => Some(GREEN),
        Health::Working => Some(AMBER),
        Health::Bad => Some(RED),
        Health::Idle => None,
    };
    let btn = match fill {
        Some(c) => egui::Button::new(egui::RichText::new(text).color(egui::Color32::WHITE).strong()).fill(c),
        None => egui::Button::new(text),
    };
    ui.add(btn)
}

/// A popup under `anchor` that stays open until you click outside it (these hold forms, not one-shot menu items).
fn popup(anchor: &egui::Response, width: f32, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::menu(anchor).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).width(width).show(add_contents);
}

/// Where the selection should point after the device list was re-read: the same device if it is still there, else the first.
fn selection_after_rescan(previous: Option<&str>, devices: &[DeviceInfo]) -> usize {
    previous.and_then(|p| devices.iter().position(|d| d.path == p)).unwrap_or(0)
}

/// A fresh press of a key the OS-level grab forwards (one that maps to a HID usage), as egui sees it. With the grab in
/// control such a press is swallowed before egui can see it, so seeing one means the grab is not in control.
/// Auto-repeats (a key held before capture began) and keys the grab cannot map legitimately reach egui and do not count.
fn grab_should_have_swallowed(ev: &egui::Event) -> bool {
    matches!(ev, egui::Event::Key { key, physical_key, pressed: true, repeat: false, .. } if keymap::to_hid(physical_key.unwrap_or(*key)).is_some())
}

/// Diagnostics for support (`KVMIT_DEBUG=1`): why capture started or ended. Never anything about which keys were pressed.
fn debug_log(msg: &str) {
    if std::env::var_os("KVMIT_DEBUG").is_some() {
        eprintln!("[debug] {msg}");
    }
}

/// The window/taskbar icon (the logo on a light tile so it reads on dark taskbars too).
fn app_icon() -> Option<egui::IconData> {
    let img = image::load_from_memory_with_format(include_bytes!("../assets/icon-256.png"), image::ImageFormat::Png).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData { rgba: img.into_raw(), width, height })
}

pub fn run_gui() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: {
            let v = egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]).with_title("kvm-it");
            match app_icon() {
                Some(icon) => v.with_icon(icon),
                None => v,
            }
        },
        ..Default::default()
    };
    eframe::run_native("kvm-it", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(k: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key { key: k, physical_key: Some(k), pressed, repeat, modifiers: egui::Modifiers::NONE }
    }

    #[test]
    fn a_rescan_keeps_the_selection_on_the_same_device() {
        let dev = |path: &str| DeviceInfo { path: path.into(), name: path.into() };
        let after = vec![dev("/dev/video4"), dev("/dev/video0"), dev("/dev/video2")];
        assert_eq!(selection_after_rescan(Some("/dev/video0"), &after), 1, "same device, new position");
        assert_eq!(selection_after_rescan(Some("/dev/video9"), &after), 0, "gone: back to the first");
        assert_eq!(selection_after_rescan(None, &after), 0);
        assert_eq!(selection_after_rescan(Some("/dev/video0"), &[]), 0, "no devices at all");
    }

    #[test]
    fn only_a_fresh_press_of_a_mappable_key_proves_the_grab_is_not_in_control() {
        assert!(grab_should_have_swallowed(&key(egui::Key::A, true, false)));
        assert!(!grab_should_have_swallowed(&key(egui::Key::A, true, true)), "auto-repeat of a key held before capture");
        assert!(!grab_should_have_swallowed(&key(egui::Key::A, false, false)), "a release passes through legitimately");
        assert!(!grab_should_have_swallowed(&egui::Event::PointerGone), "not a key at all");
    }
}
