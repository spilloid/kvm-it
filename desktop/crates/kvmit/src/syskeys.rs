//! OS-level keyboard capture. While input is captured, every key belongs to the target: the controller's OS must not
//! act on Win, Alt+Tab, Ctrl+Esc, Alt+F4 (macOS: Cmd+Tab, Cmd+Space, Cmd+Q) and friends. On Windows a low-level keyboard
//! hook (in a helper process) swallows each key locally and hands it to the app; on macOS a Quartz event tap does the
//! same in-process and needs the Accessibility permission. Elsewhere, or without the permission, `Grab::start` returns
//! `None` and the GUI keeps using egui's key events.
//!
//! The logic (physical key -> HID usage, auto-repeat, the Ctrl+Alt+Esc release chord) is pure and unit-tested
//! here; the Windows glue below it only moves events across a channel. Ctrl+Alt+Del and Win+L are handled by Windows
//! itself and can never be hooked: send those from the Keys menu.
//!
//! Never log which key was pressed: while capturing, that is the user's typing (passwords included).
use kvmit_hid::Key;

/// A key event as the OS reports it: the physical key (set-1 scan code + extended flag) and the virtual key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawKey {
    pub scan: u8,
    pub extended: bool,
    pub vk: u32,
}

const VK_PAUSE: u32 = 0x13;

/// Physical key -> USB HID usage. The target's layout decides what appears, so this is by position, never by the
/// controller's layout. `None` for keys the adapter cannot send (media keys and the like).
pub fn usage(k: RawKey) -> Option<Key> {
    let u: u8 = if !k.extended {
        match k.scan {
            0x01 => 0x29,
            0x02..=0x0A => 0x1E + (k.scan - 0x02), // 1..9
            0x0B => 0x27,                          // 0
            0x0C => 0x2D,
            0x0D => 0x2E,
            0x0E => 0x2A,
            0x0F => 0x2B,
            0x10..=0x19 => [0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13][(k.scan - 0x10) as usize], // QWERTYUIOP
            0x1A => 0x2F,
            0x1B => 0x30,
            0x1C => 0x28,
            0x1D => 0xE0,
            0x1E..=0x26 => [0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F][(k.scan - 0x1E) as usize], // ASDFGHJKL
            0x27 => 0x33,
            0x28 => 0x34,
            0x29 => 0x35,
            0x2A => 0xE1,
            0x2B => 0x31,
            0x2C..=0x32 => [0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10][(k.scan - 0x2C) as usize], // ZXCVBNM
            0x33 => 0x36,
            0x34 => 0x37,
            0x35 => 0x38,
            0x36 => 0xE5,
            0x37 => 0x55, // keypad *
            0x38 => 0xE2,
            0x39 => 0x2C,
            0x3A => 0x39,
            0x3B..=0x44 => 0x3A + (k.scan - 0x3B), // F1..F10
            0x45 => {
                if k.vk == VK_PAUSE {
                    0x48
                } else {
                    0x53 // NumLock
                }
            }
            0x46 => 0x47,
            0x47 => 0x5F,
            0x48 => 0x60,
            0x49 => 0x61,
            0x4A => 0x56,
            0x4B => 0x5C,
            0x4C => 0x5D,
            0x4D => 0x5E,
            0x4E => 0x57,
            0x4F => 0x59,
            0x50 => 0x5A,
            0x51 => 0x5B,
            0x52 => 0x62,
            0x53 => 0x63,
            0x54 => 0x46, // Alt+PrintScreen (SysRq): PrintScreen with Alt held
            0x56 => 0x64, // ISO key beside left shift
            0x57 => 0x44, // F11
            0x58 => 0x45, // F12
            0x64..=0x6E => 0x68 + (k.scan - 0x64), // F13..F23
            0x76 => 0x73, // F24
            _ => return None,
        }
    } else {
        match k.scan {
            0x1C => 0x58, // keypad Enter
            0x1D => 0xE4,
            0x35 => 0x54, // keypad /
            0x37 => 0x46, // PrintScreen
            0x38 => 0xE6,
            0x45 => 0x53, // NumLock, which Windows reports as extended
            0x46 => 0x48, // Ctrl+Pause (Break)
            0x47 => 0x4A,
            0x48 => 0x52,
            0x49 => 0x4B,
            0x4B => 0x50,
            0x4D => 0x4F,
            0x4F => 0x4D,
            0x50 => 0x51,
            0x51 => 0x4E,
            0x52 => 0x49,
            0x53 => 0x4C,
            0x5B => 0xE3,
            0x5C => 0xE7,
            0x5D => 0x65, // Menu
            _ => return None,
        }
    };
    Some(Key(u))
}

/// What the app should do with one forwarded key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Down(Key),
    Up(Key),
    /// Ctrl+Alt+Esc: give the keyboard back.
    Release,
}

/// The events up to (not including) the first release chord, and whether there was one. Anything after the chord
/// belongs to the controller again and must never be forwarded to the target.
pub fn until_release(events: Vec<Event>) -> (Vec<Event>, bool) {
    match events.iter().position(|e| *e == Event::Release) {
        Some(i) => (events[..i].to_vec(), true),
        None => (events, false),
    }
}

/// What the hook should do with the OS event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Let the OS have it (a key we cannot forward, or the release of a key pressed before capture began: swallowing
    /// that would leave the OS believing the key is still down).
    Pass,
    /// Keep it from the OS; forward the event, if any.
    Swallow(Option<Event>),
}

/// Per-capture key state.
#[derive(Default)]
pub struct Tracker {
    down: Vec<Key>,
    /// Keys that were already physically down when capture began. The OS saw their press, so their repeats and
    /// their release must reach the OS (swallowing the release would leave the key stuck down locally); they count
    /// as held for the release chord (Ctrl held before Alt+Esc must still release).
    pre_held: Vec<Key>,
    chord_esc: bool,
}

impl Tracker {
    pub fn with_pre_held(keys: Vec<Key>) -> Tracker {
        Tracker { pre_held: keys, ..Tracker::default() }
    }

    fn is_down(&self, k: Key) -> bool {
        self.down.contains(&k) || self.pre_held.contains(&k)
    }

    pub fn on_key(&mut self, raw: RawKey, down: bool) -> Action {
        // Windows brackets some extended keys with a fake Shift (scan 0x2A/0x36, extended); drop it both ways.
        if raw.extended && (raw.scan == 0x2A || raw.scan == 0x36) {
            return Action::Swallow(None);
        }
        self.on_usage(usage(raw), down)
    }

    /// The platform-independent part: a key already mapped to its HID usage (`None`: one we cannot forward).
    pub fn on_usage(&mut self, key: Option<Key>, down: bool) -> Action {
        let Some(key) = key else { return Action::Pass };

        if key == Key::ESC {
            let ctrl = self.is_down(Key::LEFT_CTRL) || self.is_down(Key(0xE4));
            let alt = self.is_down(Key::LEFT_ALT) || self.is_down(Key(0xE6));
            if down && ctrl && alt {
                self.chord_esc = true;
                return Action::Swallow(Some(Event::Release));
            }
            if !down && self.chord_esc {
                self.chord_esc = false;
                return Action::Swallow(None);
            }
        }
        if let Some(i) = self.pre_held.iter().position(|d| *d == key) {
            if !down {
                self.pre_held.swap_remove(i); // its release goes to the OS, which saw the press
            }
            return Action::Pass;
        }
        if down {
            if self.down.contains(&key) {
                return Action::Swallow(None); // auto-repeat: the target repeats a held key by itself
            }
            self.down.push(key);
            Action::Swallow(Some(Event::Down(key)))
        } else if let Some(i) = self.down.iter().position(|d| *d == key) {
            self.down.swap_remove(i);
            Action::Swallow(Some(Event::Up(key)))
        } else {
            Action::Pass
        }
    }
}

/// macOS virtual key code (kVK_*, which is positional, like a scan code) -> USB HID usage. Known gap: on ISO Apple
/// keyboards macOS swaps the codes of the key left of 1 and the key left of Z; this table follows the ANSI meaning.
/// `None` for keys the adapter
/// cannot send (Fn, media keys) and for the modifiers, which arrive as flag changes (see `mac_modifier`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_usage(code: u16) -> Option<Key> {
    // ANSI letters/digits/punctuation, indexed by kVK code 0x00..=0x32.
    const MAIN: [u8; 0x33] = [
        0x04, 0x16, 0x07, 0x09, 0x0B, 0x0A, 0x1D, 0x1B, 0x06, 0x19, // a s d f h g z x c v
        0x64, 0x05, 0x14, 0x1A, 0x08, 0x15, 0x1C, 0x17, 0x1E, 0x1F, // ISO-section b q w e r y t 1 2
        0x20, 0x21, 0x23, 0x22, 0x2E, 0x26, 0x24, 0x2D, 0x25, 0x27, // 3 4 6 5 = 9 7 - 8 0
        0x30, 0x12, 0x18, 0x2F, 0x0C, 0x13, 0x28, 0x0F, 0x0D, 0x34, // ] o u [ i p Return l j '
        0x0E, 0x33, 0x31, 0x36, 0x38, 0x11, 0x10, 0x37, 0x2B, 0x2C, // k ; \ , / n m . Tab Space
        0x35, // `
    ];
    let u: u8 = match code {
        0x00..=0x32 => MAIN[code as usize],
        0x33 => 0x2A, // Delete (backspace)
        0x35 => 0x29, // Escape
        0x39 => 0x39, // CapsLock (handled as a tap: see the macOS grab)
        0x40 => 0x6C, // F17
        0x41 => 0x63, // keypad .
        0x43 => 0x55, // keypad *
        0x45 => 0x57, // keypad +
        0x47 => 0x53, // keypad Clear = NumLock position
        0x4B => 0x54, // keypad /
        0x4C => 0x58, // keypad Enter
        0x4E => 0x56, // keypad -
        0x4F => 0x6D, // F18
        0x50 => 0x6E, // F19
        0x51 => 0x67, // keypad =
        0x52 => 0x62, // keypad 0
        0x53..=0x59 => 0x59 + (code - 0x53) as u8, // keypad 1..7
        0x5A => 0x6F, // F20
        0x5B => 0x60, // keypad 8
        0x5C => 0x61, // keypad 9
        0x5D => 0x89, // JIS Yen
        0x5E => 0x87, // JIS underscore (International1)
        0x5F => 0x85, // JIS keypad comma
        0x60 => 0x3E, // F5
        0x61 => 0x3F, // F6
        0x62 => 0x40, // F7
        0x63 => 0x3C, // F3
        0x64 => 0x41, // F8
        0x65 => 0x42, // F9
        0x66 => 0x91, // JIS Eisu (LANG2)
        0x67 => 0x44, // F11
        0x68 => 0x90, // JIS Kana (LANG1)
        0x69 => 0x68, // F13
        0x6A => 0x6B, // F16
        0x6B => 0x69, // F14
        0x6D => 0x43, // F10
        0x6E => 0x65, // ContextualMenu -> Menu
        0x6F => 0x45, // F12
        0x71 => 0x6A, // F15
        0x72 => 0x49, // Help -> Insert (same position on full keyboards)
        0x73 => 0x4A, // Home
        0x74 => 0x4B, // PageUp
        0x75 => 0x4C, // Forward Delete
        0x76 => 0x3D, // F4
        0x77 => 0x4D, // End
        0x78 => 0x3B, // F2
        0x79 => 0x4E, // PageDown
        0x7A => 0x3A, // F1
        0x7B => 0x50, // Left
        0x7C => 0x4F, // Right
        0x7D => 0x51, // Down
        0x7E => 0x52, // Up
        _ => return mac_modifier(code).map(|(k, _)| k),
    };
    Some(Key(u))
}

/// A macOS modifier key code -> its HID usage and the device-dependent flag bit (NX_DEVICE*KEYMASK) that says whether
/// that exact key (left or right) is down after a flags-changed event.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_modifier(code: u16) -> Option<(Key, u64)> {
    Some(match code {
        0x3B => (Key::LEFT_CTRL, 0x0001),
        0x3E => (Key(0xE4), 0x2000), // right Control
        0x38 => (Key::LEFT_SHIFT, 0x0002),
        0x3C => (Key(0xE5), 0x0004), // right Shift
        0x3A => (Key::LEFT_ALT, 0x0020), // Option
        0x3D => (Key(0xE6), 0x0040), // right Option
        0x37 => (Key::LEFT_GUI, 0x0008), // Command
        0x36 => (Key(0xE7), 0x0010), // right Command
        _ => return None,
    })
}

/// For a flags-changed event: is the modifier `code` down now? `None` if `code` is not a tracked modifier.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_modifier_down(code: u16, flags: u64) -> Option<bool> {
    mac_modifier(code).map(|(_, bit)| flags & bit != 0)
}

/// The release chord as this platform's keyboard labels it.
pub const RELEASE_CHORD: &str = if cfg!(target_os = "macos") { "Ctrl+Option+Esc" } else { "Ctrl+Alt+Esc" };

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MapVirtualKeyW, MAPVK_VK_TO_VSC_EX};
    use windows::Win32::UI::WindowsAndMessaging::*;

    /// The hook runs in a helper process, never inside the GUI. Inside the GUI process (eframe/winit, software GL)
    /// the hook callback was never invoked, while the same code in a separate process worked even with the GUI as
    /// the foreground window. It is also the safer design: the hook lives in an otherwise idle process that answers
    /// instantly, and it exits (removing the hook) the moment its parent goes away, so a hung or crashed GUI can
    /// never trap or slow the keyboard.
    pub const HELPER_ARG: &str = "--keyboard-grab-helper";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    /// The GUI sends a heartbeat byte this often (from the thread that handles input, so a hung GUI stops it);
    /// with none for `HEARTBEAT_TIMEOUT` the helper stops swallowing and tells the parent to release.
    const HEARTBEAT_EVERY: Duration = Duration::from_millis(250);
    const HEARTBEAT_TIMEOUT: Duration = Duration::from_millis(3000);

    // ---- inside the helper process: the hook itself ----

    struct State {
        tx: Sender<Event>,
        tracker: Tracker,
    }
    /// The hook procedure is a bare function, so its state lives here; only the hook thread touches it.
    static STATE: Mutex<Option<State>> = Mutex::new(None);
    /// Whether the hook swallows keys at all. Turned off by the release chord (in the hook itself, so it does not
    /// depend on the GUI acting on it) and by a missing heartbeat; from then on every key goes to the controller.
    static SWALLOW: AtomicBool = AtomicBool::new(true);
    /// Milliseconds (since `epoch()`) of the last heartbeat from the parent.
    static LAST_BEAT: AtomicU64 = AtomicU64::new(0);
    /// Set by the watchdog thread when the heartbeat stopped; the writer loop then reports it to the parent.
    static STALE: AtomicBool = AtomicBool::new(false);

    fn epoch() -> Instant {
        static T: OnceLock<Instant> = OnceLock::new();
        *T.get_or_init(Instant::now)
    }
    fn now_ms() -> u64 {
        epoch().elapsed().as_millis() as u64
    }

    /// Keys physically down right now (the controller's OS already saw their press).
    fn keys_down_now() -> Vec<Key> {
        let mut out = Vec::new();
        // specific left/right modifiers (0xA0..=0xA5) instead of the generic Shift/Ctrl/Alt (0x10..=0x12)
        for vk in (0x08u32..=0xFE).filter(|v| !(0x10..=0x12).contains(v)) {
            if unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 == 0 {
                continue;
            }
            let sc = unsafe { MapVirtualKeyW(vk, MAPVK_VK_TO_VSC_EX) };
            let raw = RawKey { scan: (sc & 0xFF) as u8, extended: (sc >> 8) & 0xFF == 0xE0, vk };
            if let Some(k) = usage(raw) {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 && SWALLOW.load(Ordering::Relaxed) {
            let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let m = wparam.0 as u32;
            let down = m == WM_KEYDOWN || m == WM_SYSKEYDOWN;
            let up = m == WM_KEYUP || m == WM_SYSKEYUP;
            // Never block here (Windows drops a slow hook) and never trap the keyboard: if the state is busy or
            // nobody is listening, let the key through.
            if down || up {
                if let Ok(mut guard) = STATE.try_lock() {
                    if let Some(s) = guard.as_mut() {
                        let raw = RawKey { scan: (k.scanCode & 0xFF) as u8, extended: k.flags.0 & LLKHF_EXTENDED.0 != 0, vk: k.vkCode };
                        match s.tracker.on_key(raw, down) {
                            Action::Pass => {}
                            Action::Swallow(ev) => {
                                if ev == Some(Event::Release) {
                                    SWALLOW.store(false, Ordering::Relaxed); // the chord stands on its own, GUI or not
                                }
                                if ev.is_none_or(|e| s.tx.send(e).is_ok()) {
                                    return LRESULT(1);
                                }
                            }
                        }
                    }
                }
            }
        }
        CallNextHookEx(None, code, wparam, lparam)
    }

    /// The hook, installed on its own thread of this (helper) process; events come out of `rx`.
    struct Hook {
        rx: Receiver<Event>,
        thread: u32,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl Hook {
        fn install() -> Option<Hook> {
            let (tx, rx) = channel();
            let (ready_tx, ready_rx) = channel::<Option<u32>>();
            SWALLOW.store(true, Ordering::Relaxed);
            *STATE.lock().unwrap() = Some(State { tx, tracker: Tracker::default() });
            let join = std::thread::Builder::new()
                .name("kvmit-keyboard-hook".into())
                .spawn(move || unsafe {
                    let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
                    // Sample the keys held right now and install the hook while holding the state lock: callbacks
                    // that arrive meanwhile find it locked and pass their key through (the OS keeps it, consistent
                    // with the sample), and the gap between the sample and the hook going live is as small as it can be.
                    let mut state = STATE.lock().unwrap();
                    let held = keys_down_now();
                    let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) {
                        Ok(h) => h,
                        Err(_) => {
                            drop(state);
                            let _ = ready_tx.send(None);
                            return;
                        }
                    };
                    if let Some(s) = state.as_mut() {
                        s.tracker = Tracker::with_pre_held(held);
                    }
                    drop(state);
                    let _ = ready_tx.send(Some(GetCurrentThreadId()));
                    // A low-level hook is only called while its thread pumps messages; WM_QUIT ends the loop.
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {}
                    let _ = UnhookWindowsHookEx(hook);
                })
                .ok()?;
            match ready_rx.recv() {
                Ok(Some(thread)) => Some(Hook { rx, thread, join: Some(join) }),
                _ => {
                    *STATE.lock().unwrap() = None;
                    None
                }
            }
        }
    }

    impl Drop for Hook {
        fn drop(&mut self) {
            *STATE.lock().unwrap() = None; // from here on the hook only passes keys through
            unsafe {
                let _ = PostThreadMessageW(self.thread, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(j) = self.join.take() {
                let _ = j.join();
            }
        }
    }

    /// If this process was started as the helper, run it (until its parent goes away) and return true.
    /// Call first thing in `main` of any binary that uses `Grab`.
    pub fn run_helper_if_requested() -> bool {
        if std::env::args().nth(1).as_deref() != Some(HELPER_ARG) {
            return false;
        }
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let Some(hook) = Hook::install() else {
            let _ = writeln!(out, "failed");
            return true;
        };
        let _ = writeln!(out, "ready");
        let _ = out.flush();
        // The parent holds our stdin open for as long as it wants the grab (EOF = gone or let go) and writes a
        // heartbeat byte while it is responsive.
        let alive = Arc::new(AtomicBool::new(true));
        let a2 = alive.clone();
        LAST_BEAT.store(now_ms(), Ordering::Relaxed);
        std::thread::spawn(move || {
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {
                LAST_BEAT.store(now_ms(), Ordering::Relaxed);
            }
            a2.store(false, Ordering::SeqCst);
        });
        // The watchdog is its own thread so that nothing the writer loop below can block on (a full pipe to a
        // suspended parent, say) can stop it from giving the keyboard back.
        STALE.store(false, Ordering::Relaxed);
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(200));
            if now_ms().saturating_sub(LAST_BEAT.load(Ordering::Relaxed)) > HEARTBEAT_TIMEOUT.as_millis() as u64 {
                // The GUI has stopped answering (hung?): give the keyboard back now.
                SWALLOW.store(false, Ordering::Relaxed);
                STALE.store(true, Ordering::Relaxed);
                return;
            }
        });
        let mut timed_out_reported = false;
        while alive.load(Ordering::SeqCst) {
            if !timed_out_reported && STALE.load(Ordering::Relaxed) {
                // Say so, so a recovering GUI ends capture instead of believing it still has the keyboard.
                let _ = writeln!(out, "R");
                let _ = out.flush();
                timed_out_reported = true;
            }
            for e in hook.rx.try_iter() {
                let _ = match e {
                    Event::Down(k) => writeln!(out, "D {:x}", k.0),
                    Event::Up(k) => writeln!(out, "U {:x}", k.0),
                    Event::Release => writeln!(out, "R"),
                };
                let _ = out.flush();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        drop(hook);
        true
    }

    // ---- in the GUI process: start the helper and read its events ----

    pub struct Grab {
        rx: Receiver<Event>,
        child: Child,
        last_beat: Instant,
    }

    impl Grab {
        /// Start the helper. `None` if it cannot be started or Windows refuses the hook; the GUI then falls back to
        /// egui's key events.
        /// `wake` is called (from another thread) whenever events arrive, so the GUI can repaint and drain promptly.
        pub fn start(wake: impl Fn() + Send + 'static) -> Option<Grab> {
            let mut child = Command::new(std::env::current_exe().ok()?)
                .arg(HELPER_ARG)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .ok()?;
            let stdout = child.stdout.take()?;
            let (tx, rx) = channel();
            let (ready_tx, ready_rx) = channel::<bool>();
            std::thread::Builder::new()
                .name("kvmit-keyboard-grab-reader".into())
                .spawn(move || {
                    let mut ready = Some(ready_tx);
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        let mut it = line.split_whitespace();
                        match (it.next(), it.next().and_then(|h| u8::from_str_radix(h, 16).ok())) {
                            (Some("ready"), _) => {
                                let _ = ready.take().map(|r| r.send(true));
                            }
                            (Some("failed"), _) => {
                                let _ = ready.take().map(|r| r.send(false));
                            }
                            (Some("D"), Some(k)) => {
                                let _ = tx.send(Event::Down(Key(k)));
                                wake();
                            }
                            (Some("U"), Some(k)) => {
                                let _ = tx.send(Event::Up(Key(k)));
                                wake();
                            }
                            (Some("R"), _) => {
                                let _ = tx.send(Event::Release);
                                wake();
                            }
                            _ => {}
                        }
                    }
                    // The helper ended (it crashed, or was killed): give the keyboard back.
                    let _ = ready.take().map(|r| r.send(false));
                    let _ = tx.send(Event::Release);
                    wake();
                })
                .ok()?;
            match ready_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(true) => Some(Grab { rx, child, last_beat: Instant::now() }),
                Ok(false) | Err(RecvTimeoutError::Disconnected) | Err(RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    None
                }
            }
        }

        /// Events since the last call, in order. Call it from the thread that handles input: it doubles as the
        /// heartbeat that tells the helper this side is still responsive.
        pub fn drain(&mut self) -> Vec<Event> {
            if self.last_beat.elapsed() >= HEARTBEAT_EVERY {
                if let Some(stdin) = self.child.stdin.as_mut() {
                    let _ = stdin.write_all(b".").and_then(|_| stdin.flush());
                }
                self.last_beat = Instant::now();
            }
            self.rx.try_iter().collect()
        }
    }

    impl Drop for Grab {
        fn drop(&mut self) {
            drop(self.child.stdin.take()); // EOF: the helper unhooks and exits by itself
            let _ = self.child.kill(); // and if it is slow about it, don't wait
            let _ = self.child.wait();
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    //! A Quartz event tap on a private run-loop thread. It swallows keyboard events while capture is on and hands them
    //! to the GUI. Safety valve, as on Windows: if the GUI stops draining (hung) for `HEARTBEAT_TIMEOUT`, the tap stops
    //! swallowing so a stuck app can never trap the Mac's keyboard; after the release chord everything passes through.
    use super::{mac_modifier_down, mac_usage, Action, Event, Tracker};
    use kvmit_hid::Key;
    use objc2_core_foundation::{kCFRunLoopCommonModes, CFMachPort, CFRetained, CFRunLoop};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventMask, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement, CGEventTapProxy,
        CGEventType, CGPreflightPostEventAccess, CGRequestPostEventAccess,
    };
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);

    struct State {
        tracker: Mutex<Tracker>,
        tx: Mutex<Sender<Event>>,
        wake: Box<dyn Fn() + Send + Sync>,
        epoch: Instant,
        last_beat_ms: AtomicU64,
        released: AtomicBool,
        port: Mutex<Option<PortRef>>,
    }

    /// The tap's port. Only used on the tap thread itself (the callback runs there), so it never actually crosses threads.
    struct PortRef(CFRetained<CFMachPort>);
    unsafe impl Send for PortRef {}

    impl State {
        fn stale(&self) -> bool {
            let now = self.epoch.elapsed().as_millis() as u64;
            now.saturating_sub(self.last_beat_ms.load(Ordering::SeqCst)) > HEARTBEAT_TIMEOUT.as_millis() as u64
        }
        fn send(&self, ev: Event) {
            let _ = self.tx.lock().unwrap().send(ev);
            (self.wake)();
        }
    }

    /// The tap callback. Returns the event to let macOS have it, null to swallow it.
    unsafe extern "C-unwind" fn on_event(_proxy: CGEventTapProxy, ty: CGEventType, event: NonNull<CGEvent>, user: *mut c_void) -> *mut CGEvent {
        let pass = event.as_ptr();
        // SAFETY: `user` is the `State` the tap thread keeps alive for as long as the tap exists.
        let st = unsafe { &*(user as *const State) };
        if ty == CGEventType::TapDisabledByTimeout || ty == CGEventType::TapDisabledByUserInput {
            if let Some(port) = st.port.lock().unwrap().as_ref() {
                CGEvent::tap_enable(&port.0, true); // macOS turns a tap off if a callback was slow; turn it back on
            }
            return pass;
        }
        if st.released.load(Ordering::SeqCst) || st.stale() {
            return pass;
        }
        let ev = unsafe { event.as_ref() };
        let code = CGEvent::integer_value_field(Some(ev), CGEventField::KeyboardEventKeycode) as u16;
        let down = match ty {
            CGEventType::KeyDown => true,
            CGEventType::KeyUp => false,
            CGEventType::FlagsChanged => {
                if code == 0x39 {
                    // CapsLock reports only its toggles: forward each as a tap.
                    st.send(Event::Down(Key(0x39)));
                    st.send(Event::Up(Key(0x39)));
                    return std::ptr::null_mut();
                }
                match mac_modifier_down(code, CGEvent::flags(Some(ev)).0) {
                    Some(d) => d,
                    None => return pass, // Fn and other flag changes stay with macOS
                }
            }
            _ => return pass,
        };
        let action = st.tracker.lock().unwrap().on_usage(mac_usage(code), down);
        match action {
            Action::Pass => pass,
            Action::Swallow(Some(e)) => {
                if e == Event::Release {
                    st.released.store(true, Ordering::SeqCst);
                }
                st.send(e);
                std::ptr::null_mut()
            }
            Action::Swallow(None) => std::ptr::null_mut(),
        }
    }

    /// The run loop is only touched through CFRunLoopStop, which Core Foundation documents as callable from any thread.
    struct LoopRef(CFRetained<CFRunLoop>);
    unsafe impl Send for LoopRef {}

    pub struct Grab {
        rx: Receiver<Event>,
        state: Arc<State>,
        run_loop: Option<LoopRef>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Grab {
        /// Start the tap. `None` without the Accessibility permission (macOS is then asked to show its prompt once; the
        /// GUI falls back to egui's key events and says why).
        pub fn start(wake: impl Fn() + Send + Sync + 'static) -> Option<Grab> {
            let (tx, rx) = channel();
            let state = Arc::new(State {
                tracker: Mutex::new(Tracker::default()),
                tx: Mutex::new(tx),
                wake: Box::new(wake),
                epoch: Instant::now(),
                last_beat_ms: AtomicU64::new(0),
                released: AtomicBool::new(false),
                port: Mutex::new(None),
            });
            let (ready_tx, ready_rx) = channel::<Option<LoopRef>>();
            let st = state.clone();
            let thread = std::thread::Builder::new()
                .name("kvmit-keyboard-grab".into())
                .spawn(move || {
                    let mask: CGEventMask =
                        (1 << CGEventType::KeyDown.0) | (1 << CGEventType::KeyUp.0) | (1 << CGEventType::FlagsChanged.0);
                    // SAFETY: the callback only reads `st`, which this thread keeps alive until the tap is gone.
                    let port = unsafe {
                        CGEvent::tap_create(
                            CGEventTapLocation::SessionEventTap,
                            CGEventTapPlacement::HeadInsertEventTap,
                            CGEventTapOptions::Default,
                            mask,
                            Some(on_event),
                            Arc::as_ptr(&st) as *mut c_void,
                        )
                    };
                    let (Some(port), Some(run_loop)) = (port, CFRunLoop::current()) else {
                        let _ = ready_tx.send(None);
                        return;
                    };
                    let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
                        let _ = ready_tx.send(None);
                        return;
                    };
                    run_loop.add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
                    CGEvent::tap_enable(&port, true);
                    *st.port.lock().unwrap() = Some(PortRef(port.clone()));
                    let _ = ready_tx.send(Some(LoopRef(run_loop.clone())));
                    CFRunLoop::run(); // until Grab::drop stops it
                    CGEvent::tap_enable(&port, false);
                    st.port.lock().unwrap().take();
                })
                .ok()?;
            match ready_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(Some(run_loop)) => {
                    state.last_beat_ms.store(state.epoch.elapsed().as_millis() as u64, Ordering::SeqCst);
                    Some(Grab { rx, state, run_loop: Some(run_loop), thread: Some(thread) })
                }
                _ => {
                    let _ = thread.join();
                    if !CGPreflightPostEventAccess() {
                        CGRequestPostEventAccess(); // macOS shows its Accessibility prompt (once)
                    }
                    None
                }
            }
        }

        /// Events since the last call, in order; also the heartbeat that keeps the tap swallowing.
        pub fn drain(&mut self) -> Vec<Event> {
            self.state.last_beat_ms.store(self.state.epoch.elapsed().as_millis() as u64, Ordering::SeqCst);
            self.rx.try_iter().collect()
        }
    }

    impl Drop for Grab {
        fn drop(&mut self) {
            self.state.released.store(true, Ordering::SeqCst); // pass everything from this instant
            if let Some(l) = self.run_loop.take() {
                l.0.stop();
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    pub fn run_helper_if_requested() -> bool {
        false
    }

    /// Why there is no OS-level grab, for the GUI to show once.
    pub fn unavailable_hint() -> Option<&'static str> {
        Some("macOS is keeping Cmd+Tab, Cmd+Space, Cmd+Q and similar shortcuts for itself: allow kvm-it in System Settings > Privacy & Security > Accessibility, then capture again so they go to the target. Until then use the Keys menu for them.")
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod imp {
    use super::Event;

    /// No OS-level grab on this platform: the GUI uses egui's key events.
    pub struct Grab;
    impl Grab {
        pub fn start(_wake: impl Fn() + Send + 'static) -> Option<Grab> {
            None
        }
        pub fn drain(&mut self) -> Vec<Event> {
            Vec::new()
        }
    }
    pub fn run_helper_if_requested() -> bool {
        false
    }
    /// No OS-level grab here; egui's key events are used, which is fine on Linux desktops.
    pub fn unavailable_hint() -> Option<&'static str> {
        None
    }
}

#[cfg(windows)]
pub fn unavailable_hint() -> Option<&'static str> {
    None
}
#[cfg(not(windows))]
pub use imp::unavailable_hint;
pub use imp::{run_helper_if_requested, Grab};

#[cfg(test)]
mod mac_tests {
    use super::*;

    #[test]
    fn mac_letters_digits_and_named_keys_agree_with_the_name_parser() {
        for (code, name) in [(0x00, "A"), (0x0B, "B"), (0x08, "C"), (0x06, "Z"), (0x0C, "Q"), (0x12, "1"), (0x1D, "0"),
                             (0x24, "ENTER"), (0x35, "ESC"), (0x30, "TAB"), (0x31, "SPACE"), (0x33, "BACKSPACE"),
                             (0x75, "DELETE"), (0x7A, "F1"), (0x6F, "F12"), (0x7E, "UP"), (0x7B, "LEFT"), (0x73, "HOME")] {
            assert_eq!(mac_usage(code), kvmit_hid::parse_key(name), "kVK 0x{code:02X} should be {name}");
        }
    }

    #[test]
    fn every_mapped_mac_code_is_a_distinct_valid_usage() {
        let mut seen = std::collections::HashMap::new();
        for code in 0u16..0x80 {
            if let Some(k) = mac_usage(code) {
                assert!(k.is_valid(), "0x{code:02X}");
                assert!(seen.insert(k, code).is_none(), "0x{code:02X} and 0x{:02X} both map to {k:?}", seen[&k]);
            }
        }
        let letters = (0x04..=0x1D).filter(|u| seen.contains_key(&Key(*u))).count();
        assert_eq!(letters, 26);
    }

    #[test]
    fn mac_modifiers_read_their_own_left_or_right_bit() {
        assert_eq!(mac_usage(0x3B), Some(Key::LEFT_CTRL));
        assert_eq!(mac_usage(0x36), Some(Key(0xE7)), "right Command");
        assert_eq!(mac_modifier_down(0x3B, 0x0001), Some(true));
        assert_eq!(mac_modifier_down(0x3B, 0x2000), Some(false), "right Control down does not mean left is");
        assert_eq!(mac_modifier_down(0x3A, 0x0020 | 0x80000), Some(true));
        assert_eq!(mac_modifier_down(0x3F, 0xFFFF), None, "Fn is not forwarded");
    }

    #[test]
    fn the_release_chord_works_from_mac_codes() {
        let mut t = Tracker::default();
        assert_eq!(t.on_usage(mac_usage(0x3B), true), Action::Swallow(Some(Event::Down(Key::LEFT_CTRL))));
        assert_eq!(t.on_usage(mac_usage(0x3A), true), Action::Swallow(Some(Event::Down(Key::LEFT_ALT))));
        assert_eq!(t.on_usage(mac_usage(0x35), true), Action::Swallow(Some(Event::Release)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(scan: u8) -> RawKey {
        RawKey { scan, extended: false, vk: 0 }
    }
    fn ext(scan: u8) -> RawKey {
        RawKey { scan, extended: true, vk: 0 }
    }

    #[test]
    fn physical_keys_map_to_hid_usages() {
        assert_eq!(usage(plain(0x1E)), Some(Key::A));
        assert_eq!(usage(plain(0x10)), Some(Key(0x14))); // Q is position, not layout
        assert_eq!(usage(plain(0x02)), Some(Key(0x1E))); // 1
        assert_eq!(usage(plain(0x0B)), Some(Key(0x27))); // 0
        assert_eq!(usage(plain(0x1C)), Some(Key::ENTER));
        assert_eq!(usage(plain(0x39)), Some(Key::SPACE));
        assert_eq!(usage(plain(0x58)), Some(Key(0x45))); // F12
        assert_eq!(usage(plain(0x76)), Some(Key(0x73))); // F24
        assert_eq!(usage(ext(0x5B)), Some(Key::LEFT_GUI));
        assert_eq!(usage(ext(0x38)), Some(Key(0xE6))); // right Alt
        assert_eq!(usage(ext(0x48)), Some(Key(0x52))); // up arrow
        assert_eq!(usage(ext(0x1C)), Some(Key(0x58))); // keypad Enter
        assert_eq!(usage(ext(0x37)), Some(Key(0x46))); // PrintScreen
        assert_eq!(usage(plain(0x37)), Some(Key(0x55))); // keypad *
        assert_eq!(usage(RawKey { scan: 0x45, extended: false, vk: VK_PAUSE }), Some(Key(0x48)));
        assert_eq!(usage(ext(0x45)), Some(Key(0x53))); // NumLock
        assert_eq!(usage(ext(0x20)), None); // a media key: not forwardable
    }

    #[test]
    fn every_forwardable_usage_is_valid_and_unique() {
        let mut seen = std::collections::HashMap::new();
        for extended in [false, true] {
            for scan in 0..=255u8 {
                let raw = RawKey { scan, extended, vk: 0 };
                if let Some(k) = usage(raw) {
                    assert!(k.is_valid(), "scan {scan:#x} ext {extended}");
                    // The only documented aliases: NumLock (Windows reports it plain and extended) and PrintScreen
                    // (plain, and the Alt+PrintScreen form 0x54). Any other duplicate is a table mistake.
                    if let Some(prev) = seen.insert(k.0, (scan, extended)) {
                        assert!(matches!(k.0, 0x53 | 0x46), "usage {:#x} is produced by {prev:?} and {:?}", k.0, (scan, extended));
                    }
                }
            }
        }
    }

    #[test]
    fn a_press_is_forwarded_and_swallowed_and_its_release_too() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(plain(0x1E), true), Action::Swallow(Some(Event::Down(Key::A))));
        assert_eq!(t.on_key(plain(0x1E), false), Action::Swallow(Some(Event::Up(Key::A))));
    }

    #[test]
    fn the_windows_key_and_alt_tab_go_to_the_target_not_the_controller() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(ext(0x5B), true), Action::Swallow(Some(Event::Down(Key::LEFT_GUI))));
        assert_eq!(t.on_key(ext(0x5B), false), Action::Swallow(Some(Event::Up(Key::LEFT_GUI))));
        assert_eq!(t.on_key(plain(0x38), true), Action::Swallow(Some(Event::Down(Key::LEFT_ALT))));
        assert_eq!(t.on_key(plain(0x0F), true), Action::Swallow(Some(Event::Down(Key::TAB))));
    }

    #[test]
    fn auto_repeat_is_swallowed_without_a_second_press() {
        let mut t = Tracker::default();
        t.on_key(plain(0x1E), true);
        assert_eq!(t.on_key(plain(0x1E), true), Action::Swallow(None));
        assert_eq!(t.on_key(plain(0x1E), true), Action::Swallow(None));
        assert_eq!(t.on_key(plain(0x1E), false), Action::Swallow(Some(Event::Up(Key::A))));
    }

    #[test]
    fn ctrl_alt_esc_releases_and_is_never_forwarded() {
        let mut t = Tracker::default();
        t.on_key(plain(0x1D), true); // Ctrl
        t.on_key(plain(0x38), true); // Alt
        assert_eq!(t.on_key(plain(0x01), true), Action::Swallow(Some(Event::Release)));
        assert_eq!(t.on_key(plain(0x01), false), Action::Swallow(None));
    }

    #[test]
    fn right_hand_modifiers_also_make_the_release_chord() {
        let mut t = Tracker::default();
        t.on_key(ext(0x1D), true); // right Ctrl
        t.on_key(ext(0x38), true); // right Alt
        assert_eq!(t.on_key(plain(0x01), true), Action::Swallow(Some(Event::Release)));
    }

    #[test]
    fn esc_alone_or_with_only_one_modifier_is_an_ordinary_key() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(plain(0x01), true), Action::Swallow(Some(Event::Down(Key::ESC))));
        t.on_key(plain(0x01), false);
        t.on_key(plain(0x1D), true); // Ctrl only
        assert_eq!(t.on_key(plain(0x01), true), Action::Swallow(Some(Event::Down(Key::ESC))));
    }

    #[test]
    fn a_release_of_a_key_pressed_before_capture_passes_through() {
        // swallowing it would leave the controller's OS thinking the key is still held
        let mut t = Tracker::default();
        assert_eq!(t.on_key(plain(0x1D), false), Action::Pass);
    }

    #[test]
    fn keys_the_adapter_cannot_send_pass_through() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(ext(0x20), true), Action::Pass); // volume mute
    }

    #[test]
    fn alt_printscreen_and_ctrl_pause_are_forwardable_not_leaked_to_the_controller() {
        assert_eq!(usage(plain(0x54)), Some(Key(0x46))); // Alt+PrintScreen
        assert_eq!(usage(ext(0x46)), Some(Key(0x48))); // Ctrl+Pause (Break)
    }

    #[test]
    fn a_key_held_before_capture_keeps_going_to_the_controller_until_released() {
        // The OS saw the press: a repeat must not become a captured press, and its release must not be swallowed
        // (that would leave the key stuck down on the controller).
        let mut t = Tracker::with_pre_held(vec![Key::A]);
        assert_eq!(t.on_key(plain(0x1E), true), Action::Pass); // auto-repeat
        assert_eq!(t.on_key(plain(0x1E), false), Action::Pass); // release
        // after that it is an ordinary captured key again
        assert_eq!(t.on_key(plain(0x1E), true), Action::Swallow(Some(Event::Down(Key::A))));
    }

    #[test]
    fn modifiers_held_before_capture_still_make_the_release_chord() {
        let mut t = Tracker::with_pre_held(vec![Key::LEFT_CTRL, Key::LEFT_ALT]);
        assert_eq!(t.on_key(plain(0x01), true), Action::Swallow(Some(Event::Release)));
    }

    #[test]
    fn ctrl_alt_esc_checks_every_step_of_the_chord() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(plain(0x1D), true), Action::Swallow(Some(Event::Down(Key::LEFT_CTRL))));
        assert_eq!(t.on_key(plain(0x38), true), Action::Swallow(Some(Event::Down(Key::LEFT_ALT))));
        assert_eq!(t.on_key(plain(0x01), true), Action::Swallow(Some(Event::Release)));
        assert_eq!(t.on_key(plain(0x01), false), Action::Swallow(None));
    }

    #[test]
    fn nothing_after_the_release_chord_is_forwarded() {
        let a = Key::A;
        let (fwd, released) = until_release(vec![Event::Down(a), Event::Release, Event::Down(a), Event::Up(a)]);
        assert!(released);
        assert_eq!(fwd, vec![Event::Down(a)]);
        let (fwd, released) = until_release(vec![Event::Down(a), Event::Up(a)]);
        assert!(!released);
        assert_eq!(fwd.len(), 2);
        assert_eq!(until_release(vec![]), (vec![], false));
    }

    #[test]
    fn windows_fake_shift_around_extended_keys_is_dropped() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(ext(0x2A), true), Action::Swallow(None));
        assert_eq!(t.on_key(ext(0x2A), false), Action::Swallow(None));
        // a real left shift still works
        assert_eq!(t.on_key(plain(0x2A), true), Action::Swallow(Some(Event::Down(Key::LEFT_SHIFT))));
    }
}
