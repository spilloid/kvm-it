//! Pure UI state derivation for the GUI (no egui widgets, no I/O), so the button/chip contract is unit-tested:
//! chip colours and texts, why a control is disabled, the run-log header, notice expiry, mouse-wheel units and
//! the global abort key. `gui.rs` only maps these results onto widgets.
use kvmit_hid::Key;
use kvmit_layout::{Layout, UsAnsi};
use std::time::{Duration, Instant};

/// How a part of the setup is doing; drives a status chip's colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    Good,
    Working,
    Bad,
    Idle,
}

// ---------- chips ----------

/// The adapter link, reduced to what the chip needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind<'a> {
    Disconnected,
    Connecting(&'a str),
    Connected(&'a str),
    Failed,
}

pub fn adapter_chip(link: LinkKind<'_>) -> (Health, String) {
    match link {
        LinkKind::Connected(name) => (Health::Good, format!("Adapter: {name}")),
        LinkKind::Connecting(id) => (Health::Working, format!("Adapter: connecting to {id}…")),
        LinkKind::Failed => (Health::Bad, "Adapter: not connected (retrying)".into()),
        LinkKind::Disconnected => (Health::Idle, "Adapter: none — click to connect".into()),
    }
}

/// `hid_mounted`: `None` until the adapter's first status report has arrived.
pub fn target_usb_chip(connected: bool, hid_mounted: Option<bool>) -> (Health, &'static str) {
    match (connected, hid_mounted) {
        (true, Some(true)) => (Health::Good, "Target USB: connected"),
        (true, Some(false)) => (Health::Bad, "Target USB: not enumerated"),
        (true, None) => (Health::Working, "Target USB: checking…"),
        (false, _) => (Health::Idle, "Target USB: —"),
    }
}

pub const WHY_NO_ADAPTER: &str = "Connect an adapter first (Adapter chip).";
pub const WHY_SCRIPT_RUNNING: &str = "A script is running: abort it first (Esc).";
pub const WHY_BUSY: &str = "Input is busy (a script is running or keys are still being sent): wait for it to finish.";

/// Why input cannot be captured / sent right now, or `None` when it can.
pub fn input_block_reason(connected: bool, script_running: bool) -> Option<&'static str> {
    if !connected {
        Some(WHY_NO_ADAPTER)
    } else if script_running {
        Some(WHY_SCRIPT_RUNNING)
    } else {
        None
    }
}

/// Why a send-input control (chords, Type, Run) is unavailable: no adapter, else something else is producing input.
pub fn send_block_reason(connected: bool, input_idle: bool) -> Option<&'static str> {
    if !connected {
        Some(WHY_NO_ADAPTER)
    } else if !input_idle {
        Some(WHY_BUSY)
    } else {
        None
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputChip {
    pub health: Health,
    pub text: &'static str,
    /// Clickable (starts capture)? While captured it is an indicator only.
    pub enabled: bool,
    pub disabled_reason: Option<&'static str>,
}

pub fn input_chip(capturing: bool, block: Option<&'static str>) -> InputChip {
    if capturing {
        InputChip { health: Health::Bad, text: "INPUT CAPTURED — Ctrl+Alt+Esc to release", enabled: true, disabled_reason: None }
    } else {
        InputChip { health: Health::Idle, text: "Input: click to capture", enabled: block.is_none(), disabled_reason: block }
    }
}

// ---------- Keys chords ----------

/// The "Keys" popup: label and key names (resolved with `kvmit_hid::parse_key`).
pub const CHORDS: &[(&str, &[&str])] = &[
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

/// All keys of a chord, or `None` if any name does not resolve (a silently shortened chord would be wrong).
pub fn chord_keys(names: &[&str]) -> Option<Vec<Key>> {
    names.iter().map(|n| kvmit_hid::parse_key(n)).collect()
}

// ---------- Type ----------

/// Characters of `text` the US layout cannot type, in order, without duplicates.
pub fn untypable_chars(text: &str) -> Vec<char> {
    let mut out = Vec::new();
    for c in text.chars() {
        if UsAnsi.stroke(c).is_none() && !out.contains(&c) {
            out.push(c);
        }
    }
    out
}

/// The warning shown before the click. A secret's characters are never named, only counted.
pub fn untypable_warning(text: &str, secret: bool) -> Option<String> {
    let bad = untypable_chars(text);
    if bad.is_empty() {
        return None;
    }
    Some(if secret {
        let n = text.chars().filter(|c| UsAnsi.stroke(*c).is_none()).count();
        format!("{n} character(s) cannot be typed with the US layout.")
    } else {
        format!("Cannot type {bad:?} with the US layout.")
    })
}

// ---------- run log ----------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Finished,
    Aborted(String),
    Failed(String),
}

/// Classify a finished run: the operator stopping it (abort, declining a confirm) is Aborted, anything else Failed.
pub fn classify_run(result: &Result<(), kvmit_script::RunError>) -> RunOutcome {
    use kvmit_script::RunError;
    match result {
        Ok(()) => RunOutcome::Finished,
        Err(e @ (RunError::Cancelled | RunError::ConfirmDeclined { .. })) => RunOutcome::Aborted(e.to_string()),
        Err(e) => RunOutcome::Failed(e.to_string()),
    }
}

/// The run-log strip's header: text and colour. `outcome` is `None` while the run has not reported one.
pub fn run_header(running: bool, dry: bool, outcome: Option<&RunOutcome>) -> (String, Health) {
    if running {
        return (if dry { "Dry run…" } else { "Script running…" }.into(), Health::Working);
    }
    match outcome {
        Some(RunOutcome::Finished) => (if dry { "Dry run finished" } else { "Finished" }.into(), Health::Good),
        Some(RunOutcome::Aborted(r)) => (format!("Aborted: {r}"), Health::Working),
        Some(RunOutcome::Failed(r)) => (format!("Failed: {r}"), Health::Bad),
        None => ("Stopped".into(), Health::Idle),
    }
}

// ---------- notices ----------

/// How long a notice stays before it clears itself.
pub const NOTICE_TTL: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
pub enum NoticeAction {
    /// Nothing to do; if a notice is showing, it expires after this long.
    Keep(Option<Duration>),
    Clear,
}

/// Tracks when the current notice text first appeared (notices are plain strings set from many threads).
#[derive(Default)]
pub struct NoticeClock {
    seen: String,
    since: Option<Instant>,
}

impl NoticeClock {
    pub fn tick(&mut self, current: &str, now: Instant, ttl: Duration) -> NoticeAction {
        if current.is_empty() {
            self.seen.clear();
            self.since = None;
            return NoticeAction::Keep(None);
        }
        if current != self.seen || self.since.is_none() {
            self.seen = current.to_string();
            self.since = Some(now);
        }
        let age = now.saturating_duration_since(self.since.unwrap_or(now));
        if age >= ttl {
            self.seen.clear();
            self.since = None;
            NoticeAction::Clear
        } else {
            NoticeAction::Keep(Some(ttl - age))
        }
    }
}

/// What is true now, for deciding whether a notice's cause has gone away.
#[derive(Clone, Copy, Debug)]
pub struct Facts {
    pub video_live: bool,
    pub adapter_connected: bool,
}

/// A notice whose cause has resolved (video is showing again, the adapter is back) should go.
pub fn notice_resolved(notice: &str, f: Facts) -> bool {
    (notice.starts_with("Video stopped") || notice.starts_with("video:")) && f.video_live
        || (notice.starts_with("Pairing failed") || notice.starts_with("Not connected") || notice.starts_with("Releasing the keys failed"))
            && f.adapter_connected
}

// ---------- mouse wheel ----------

/// egui points that make one wheel notch for touchpad/pixel deltas (egui's own scroll uses 40 per line).
pub const POINTS_PER_NOTCH: f32 = 40.0;
const PAGE_NOTCHES: f32 = 10.0;

/// Turns egui wheel deltas into whole notches, carrying the fractional rest of pixel deltas to the next event.
#[derive(Default)]
pub struct WheelAccum {
    rest: f32,
}

impl WheelAccum {
    pub fn reset(&mut self) {
        self.rest = 0.0;
    }

    /// Notches (positive = as egui's delta) to send for this event, or 0.
    pub fn add(&mut self, unit: egui::MouseWheelUnit, delta: f32) -> i8 {
        let notches = match unit {
            // Line deltas are already notches: unchanged behaviour (rounded, no carry).
            egui::MouseWheelUnit::Line => return clamp(delta.round()),
            egui::MouseWheelUnit::Page => delta * PAGE_NOTCHES,
            egui::MouseWheelUnit::Point => delta / POINTS_PER_NOTCH,
        };
        if (self.rest > 0.0) != (notches > 0.0) && self.rest != 0.0 {
            self.rest = 0.0; // direction changed: drop the leftover of the other way
        }
        let total = self.rest + notches;
        let whole = total.trunc();
        self.rest = total - whole;
        clamp(whole)
    }
}

fn clamp(v: f32) -> i8 {
    v.clamp(-127.0, 127.0) as i8
}

// ---------- global abort key ----------

/// The key that aborts a running script: plain Esc, no modifiers (Ctrl+Alt+Esc stays the capture release chord).
pub fn is_abort_press(ev: &egui::Event) -> bool {
    matches!(ev, egui::Event::Key { key: egui::Key::Escape, pressed: true, repeat: false, modifiers, .. } if modifiers.is_none())
}

/// Abort fires only while a script runs and no text field has keyboard focus (Esc there leaves the field).
pub fn abort_fires(running: bool, text_field_focused: bool, events: &[egui::Event]) -> bool {
    running && !text_field_focused && events.iter().any(is_abort_press)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmit_script::RunError;

    #[test]
    fn target_usb_chip_states() {
        assert_eq!(target_usb_chip(false, None), (Health::Idle, "Target USB: —"));
        assert_eq!(target_usb_chip(false, Some(true)).0, Health::Idle, "stale status without a link is ignored");
        assert_eq!(target_usb_chip(true, None), (Health::Working, "Target USB: checking…"));
        assert_eq!(target_usb_chip(true, Some(true)), (Health::Good, "Target USB: connected"));
        assert_eq!(target_usb_chip(true, Some(false)), (Health::Bad, "Target USB: not enumerated"));
        for c in [false, true] {
            for m in [None, Some(true), Some(false)] {
                assert!(target_usb_chip(c, m).1.starts_with("Target USB:"), "UIA/screenshot tools find it by this prefix");
            }
        }
    }

    #[test]
    fn adapter_chip_states() {
        assert_eq!(adapter_chip(LinkKind::Connected("kvm-it")), (Health::Good, "Adapter: kvm-it".into()));
        assert_eq!(adapter_chip(LinkKind::Connecting("AA")).0, Health::Working);
        assert_eq!(adapter_chip(LinkKind::Failed).0, Health::Bad);
        assert_eq!(adapter_chip(LinkKind::Disconnected).0, Health::Idle);
        for l in [LinkKind::Disconnected, LinkKind::Failed, LinkKind::Connecting("x"), LinkKind::Connected("x")] {
            assert!(adapter_chip(l).1.starts_with("Adapter:"));
        }
    }

    #[test]
    fn input_chip_state_table() {
        assert_eq!(input_block_reason(false, false), Some(WHY_NO_ADAPTER));
        assert_eq!(input_block_reason(false, true), Some(WHY_NO_ADAPTER));
        assert_eq!(input_block_reason(true, true), Some(WHY_SCRIPT_RUNNING));
        assert_eq!(input_block_reason(true, false), None);

        let ready = input_chip(false, None);
        assert!(ready.enabled && ready.disabled_reason.is_none() && ready.text.starts_with("Input:"));
        let blocked = input_chip(false, Some(WHY_NO_ADAPTER));
        assert!(!blocked.enabled);
        assert_eq!(blocked.disabled_reason, Some(WHY_NO_ADAPTER));
        assert_eq!(blocked.text, ready.text, "same name whether or not it is clickable");
        let cap = input_chip(true, Some(WHY_NO_ADAPTER));
        assert_eq!((cap.health, cap.disabled_reason), (Health::Bad, None));
        assert!(cap.text.contains("Ctrl+Alt+Esc"));
    }

    #[test]
    fn send_controls_say_why() {
        assert_eq!(send_block_reason(false, true), Some(WHY_NO_ADAPTER));
        assert_eq!(send_block_reason(true, false), Some(WHY_BUSY));
        assert_eq!(send_block_reason(true, true), None);
    }

    #[test]
    fn every_chord_label_maps_to_its_keys() {
        let expect: &[(&str, &[u8])] = &[
            ("Ctrl+Alt+Del", &[0xE0, 0xE2, 0x4C]),
            ("Win", &[0xE3]),
            ("Alt+Tab", &[0xE2, 0x2B]),
            ("Alt+F4", &[0xE2, 0x3D]),
            ("Ctrl+Esc", &[0xE0, 0x29]),
            ("Win+R", &[0xE3, 0x15]),
            ("PrintScreen", &[0x46]),
            ("Menu", &[0x65]),
            ("CapsLock", &[0x39]),
            ("NumLock", &[0x53]),
            ("Pause", &[0x48]),
            ("ScrollLock", &[0x47]),
        ];
        assert_eq!(CHORDS.len(), expect.len(), "a chord was added or removed without updating this table");
        for ((label, names), (elabel, codes)) in CHORDS.iter().zip(expect) {
            assert_eq!(label, elabel);
            let keys = chord_keys(names).unwrap_or_else(|| panic!("{label}: a key name does not resolve"));
            assert_eq!(keys.iter().map(|k| k.0).collect::<Vec<_>>(), *codes, "{label}");
        }
        assert_eq!(chord_keys(&["CTRL", "NOPE"]), None);
    }

    #[test]
    fn untypable_text_is_flagged_without_leaking_secrets() {
        assert!(untypable_chars("hello World 1!").is_empty());
        assert_eq!(untypable_chars("caf\u{e9} \u{e9}"), vec!['\u{e9}']);
        let plain = untypable_warning("caf\u{e9}", false).unwrap();
        assert!(plain.contains('\u{e9}'));
        let secret = untypable_warning("p\u{e9}ss\u{e9}", true).unwrap();
        assert!(!secret.contains('\u{e9}') && secret.starts_with("2 "), "{secret}");
        assert_eq!(untypable_warning("fine", true), None);
    }

    #[test]
    fn run_outcome_and_header() {
        assert_eq!(classify_run(&Ok(())), RunOutcome::Finished);
        assert!(matches!(classify_run(&Err(RunError::Cancelled)), RunOutcome::Aborted(_)));
        assert!(matches!(classify_run(&Err(RunError::ConfirmDeclined { step: 1 })), RunOutcome::Aborted(_)));
        assert!(matches!(classify_run(&Err(RunError::Host("x".into()))), RunOutcome::Failed(_)));

        assert_eq!(run_header(true, false, None), ("Script running…".into(), Health::Working));
        assert_eq!(run_header(true, true, None).0, "Dry run…");
        assert_eq!(run_header(false, false, Some(&RunOutcome::Finished)), ("Finished".into(), Health::Good));
        assert_eq!(run_header(false, true, Some(&RunOutcome::Finished)), ("Dry run finished".into(), Health::Good));
        let (t, h) = run_header(false, false, Some(&RunOutcome::Aborted("aborted".into())));
        assert_eq!((t.as_str(), h), ("Aborted: aborted", Health::Working));
        let (t, h) = run_header(false, false, Some(&RunOutcome::Failed("device error: x".into())));
        assert_eq!((t.as_str(), h), ("Failed: device error: x", Health::Bad));
        assert_ne!(run_header(false, false, None).0, "Finished", "no outcome is never reported as finished");
    }

    #[test]
    fn notices_expire_and_restart_on_new_text() {
        let t0 = Instant::now();
        let ttl = Duration::from_secs(10);
        let mut c = NoticeClock::default();
        assert_eq!(c.tick("", t0, ttl), NoticeAction::Keep(None));
        assert_eq!(c.tick("a", t0, ttl), NoticeAction::Keep(Some(ttl)));
        assert_eq!(c.tick("a", t0 + Duration::from_secs(4), ttl), NoticeAction::Keep(Some(Duration::from_secs(6))));
        // new text restarts the clock
        assert_eq!(c.tick("b", t0 + Duration::from_secs(9), ttl), NoticeAction::Keep(Some(ttl)));
        assert_eq!(c.tick("b", t0 + Duration::from_secs(19), ttl), NoticeAction::Clear);
        // after clearing, the same text appearing again is a new notice
        assert_eq!(c.tick("b", t0 + Duration::from_secs(20), ttl), NoticeAction::Keep(Some(ttl)));
    }

    #[test]
    fn notices_resolve_with_their_cause() {
        let on = Facts { video_live: true, adapter_connected: true };
        let off = Facts { video_live: false, adapter_connected: false };
        assert!(notice_resolved("Video stopped: the card...", on));
        assert!(!notice_resolved("Video stopped: the card...", off));
        assert!(notice_resolved("Pairing failed: x", on));
        assert!(!notice_resolved("Pairing failed: x", Facts { video_live: true, adapter_connected: false }));
        assert!(!notice_resolved("Input is busy", on), "unrelated notices wait for their timeout");
    }

    #[test]
    fn wheel_points_accumulate_into_notches() {
        use egui::MouseWheelUnit::{Line, Point};
        let mut w = WheelAccum::default();
        // a touchpad: many small pixel deltas; nothing for the first, a notch once 40 points have built up
        assert_eq!(w.add(Point, 15.0), 0);
        assert_eq!(w.add(Point, 15.0), 0);
        assert_eq!(w.add(Point, 15.0), 1);
        assert_eq!(w.add(Point, 35.0), 1, "5 carried + 35");
        assert_eq!(w.add(Point, -10.0), 0, "a reversal first cancels nothing: the leftover is dropped");
        assert_eq!(w.add(Point, -30.0), -1);
        assert_eq!(w.add(Point, 4000.0), 100, "a fling is many notches, not dozens of 127s");
        assert_eq!(w.add(Point, 1.0e9), 127, "clamped");
        // lines unchanged
        w.reset();
        assert_eq!(w.add(Line, 1.0), 1);
        assert_eq!(w.add(Line, -3.0), -3);
        assert_eq!(w.add(Line, 0.2), 0);
        assert_eq!(w.add(egui::MouseWheelUnit::Page, 1.0), 10);
    }

    fn key(k: egui::Key, mods: egui::Modifiers, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key { key: k, physical_key: Some(k), pressed, repeat, modifiers: mods }
    }

    #[test]
    fn abort_key_only_when_running_and_no_text_field_is_focused() {
        let esc = key(egui::Key::Escape, egui::Modifiers::NONE, true, false);
        assert!(abort_fires(true, false, std::slice::from_ref(&esc)));
        assert!(!abort_fires(false, false, std::slice::from_ref(&esc)), "nothing to abort");
        assert!(!abort_fires(true, true, std::slice::from_ref(&esc)), "Esc while editing text leaves the field");
        assert!(!abort_fires(true, false, &[key(egui::Key::Escape, egui::Modifiers::NONE, false, false)]), "release");
        assert!(!abort_fires(true, false, &[key(egui::Key::Escape, egui::Modifiers::NONE, true, true)]), "repeat");
        assert!(!abort_fires(true, false, &[key(egui::Key::Escape, egui::Modifiers::CTRL | egui::Modifiers::ALT, true, false)]), "the release chord");
        assert!(!abort_fires(true, false, &[key(egui::Key::A, egui::Modifiers::NONE, true, false)]));
    }
}
