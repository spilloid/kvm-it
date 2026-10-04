//! OS-level keyboard capture. While input is captured, every key belongs to the target: the controller's OS must not
//! act on Win, Alt+Tab, Ctrl+Esc, Alt+F4 and friends. On Windows a low-level keyboard hook swallows each key locally
//! and hands it to the app; elsewhere `Grab::start` returns `None` and the GUI keeps using egui's key events.
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
    chord_esc: bool,
}

impl Tracker {
    fn is_down(&self, k: Key) -> bool {
        self.down.contains(&k)
    }

    pub fn on_key(&mut self, raw: RawKey, down: bool) -> Action {
        // Windows brackets some extended keys with a fake Shift (scan 0x2A/0x36, extended); drop it both ways.
        if raw.extended && (raw.scan == 0x2A || raw.scan == 0x36) {
            return Action::Swallow(None);
        }
        let Some(key) = usage(raw) else { return Action::Pass };

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
        if down {
            if self.is_down(key) {
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

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::*;

    /// The hook runs in a helper process, never inside the GUI. Inside the GUI process (eframe/winit, software GL)
    /// the hook callback was never invoked, while the same code in a separate process worked even with the GUI as
    /// the foreground window. It is also the safer design: the hook lives in an otherwise idle process that answers
    /// instantly, and it exits (removing the hook) the moment its parent goes away, so a hung or crashed GUI can
    /// never trap or slow the keyboard.
    pub const HELPER_ARG: &str = "--keyboard-grab-helper";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // ---- inside the helper process: the hook itself ----

    struct State {
        tx: Sender<Event>,
        tracker: Tracker,
    }
    /// The hook procedure is a bare function, so its state lives here; only the hook thread touches it.
    static STATE: Mutex<Option<State>> = Mutex::new(None);

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
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
            *STATE.lock().unwrap() = Some(State { tx, tracker: Tracker::default() });
            let join = std::thread::Builder::new()
                .name("kvmit-keyboard-hook".into())
                .spawn(move || unsafe {
                    let module = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
                    let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0) {
                        Ok(h) => h,
                        Err(_) => {
                            let _ = ready_tx.send(None);
                            return;
                        }
                    };
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
        // The parent holds our stdin open for as long as it wants the grab; EOF means it is gone (or let go).
        let alive = Arc::new(AtomicBool::new(true));
        let a2 = alive.clone();
        std::thread::spawn(move || {
            let mut sink = [0u8; 64];
            let mut stdin = std::io::stdin();
            while matches!(stdin.read(&mut sink), Ok(n) if n > 0) {}
            a2.store(false, Ordering::SeqCst);
        });
        while alive.load(Ordering::SeqCst) {
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
    }

    impl Grab {
        /// Start the helper. `None` if it cannot be started or Windows refuses the hook; the GUI then falls back to
        /// egui's key events.
        pub fn start() -> Option<Grab> {
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
                            }
                            (Some("U"), Some(k)) => {
                                let _ = tx.send(Event::Up(Key(k)));
                            }
                            (Some("R"), _) => {
                                let _ = tx.send(Event::Release);
                            }
                            _ => {}
                        }
                    }
                    // The helper ended (it crashed, or was killed): give the keyboard back.
                    let _ = ready.take().map(|r| r.send(false));
                    let _ = tx.send(Event::Release);
                })
                .ok()?;
            match ready_rx.recv_timeout(Duration::from_secs(3)) {
                Ok(true) => Some(Grab { rx, child }),
                Ok(false) | Err(RecvTimeoutError::Disconnected) | Err(RecvTimeoutError::Timeout) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    None
                }
            }
        }

        /// Events since the last call, in order.
        pub fn drain(&mut self) -> Vec<Event> {
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

#[cfg(not(windows))]
mod imp {
    use super::Event;

    /// No OS-level grab on this platform: the GUI uses egui's key events.
    pub struct Grab;
    impl Grab {
        pub fn start() -> Option<Grab> {
            None
        }
        pub fn drain(&mut self) -> Vec<Event> {
            Vec::new()
        }
    }
    pub fn run_helper_if_requested() -> bool {
        false
    }
}

pub use imp::{run_helper_if_requested, Grab};

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
                    // NumLock is reported both ways by Windows; every other usage has exactly one physical key
                    if let Some(prev) = seen.insert(k.0, (scan, extended)) {
                        assert_eq!(k.0, 0x53, "usage {:#x} is produced by {prev:?} and {:?}", k.0, (scan, extended));
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
    fn windows_fake_shift_around_extended_keys_is_dropped() {
        let mut t = Tracker::default();
        assert_eq!(t.on_key(ext(0x2A), true), Action::Swallow(None));
        assert_eq!(t.on_key(ext(0x2A), false), Action::Swallow(None));
        // a real left shift still works
        assert_eq!(t.on_key(plain(0x2A), true), Action::Swallow(Some(Event::Down(Key::LEFT_SHIFT))));
    }
}
