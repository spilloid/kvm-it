//! Compile (validate everything up front) then run. Compilation resolves variables, key names and layout
//! coverage *before* a single key is sent, so a script never types half a password and then fails.
use crate::frame::Frame;
use crate::model::{Script, Step, WaitSpec};
use kvmit_hid::{parse_key, Key, MOUSE_LEFT, MOUSE_MIDDLE, MOUSE_RIGHT};
use kvmit_layout::{Layout, Stroke};
use zeroize::Zeroize;
use std::collections::BTreeMap;
use std::time::Duration;

pub type Vars = BTreeMap<String, String>;

#[derive(Debug, Clone, PartialEq)]
pub enum RunError {
    MissingVar(String),
    UndefinedVar(String),
    SecretInPlainText { var: String },
    BadTemplate(String),
    UnknownKey(String),
    UnknownButton(String),
    /// `count` untypable characters; for plain text the characters are listed, for secrets never.
    Untypable { step: usize, chars: Option<Vec<char>>, count: usize },
    WaitTimeout { step: usize, saw_frames: bool, best_similarity: f64 },
    ConfirmDeclined { step: usize },
    Reference(String),
    Host(String),
    Cancelled,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::MissingVar(v) => write!(f, "variable {v:?} has no value and no default"),
            RunError::UndefinedVar(v) => write!(f, "step uses undefined variable {v:?}"),
            RunError::SecretInPlainText { var } => write!(
                f,
                "secret variable {var:?} is used in a plain `text` step; use `secret_text` so it is never shown or logged"
            ),
            RunError::BadTemplate(s) => write!(f, "bad {{{{variable}}}} template: {s}"),
            RunError::UnknownKey(k) => write!(f, "unknown key name {k:?}"),
            RunError::UnknownButton(b) => write!(f, "unknown mouse button {b:?} (left, right, middle)"),
            RunError::Untypable { step, chars: Some(c), .. } => {
                write!(f, "step {}: the layout cannot type {c:?}", step + 1)
            }
            RunError::Untypable { step, count, .. } => {
                write!(f, "step {}: the layout cannot type {count} character(s) of a secret", step + 1)
            }
            RunError::WaitTimeout { step, saw_frames: false, .. } => {
                write!(f, "step {}: timed out waiting for the screen, and no video frames were available (is a capture device open?)", step + 1)
            }
            RunError::WaitTimeout { step, best_similarity, .. } => {
                write!(f, "step {}: timed out waiting for the screen (best match {:.0}%)", step + 1, best_similarity * 100.0)
            }
            RunError::ConfirmDeclined { step } => write!(f, "step {}: operator declined to continue", step + 1),
            RunError::Reference(s) => write!(f, "cannot load reference image: {s}"),
            RunError::Host(s) => write!(f, "device error: {s}"),
            RunError::Cancelled => write!(f, "aborted"),
        }
    }
}
impl std::error::Error for RunError {}

/// Compiled key strokes. For secrets these *are* the password, so Debug shows only a length and the buffer is
/// overwritten on drop (best effort: copies made by the allocator or the OS are out of our hands).
#[derive(Clone, PartialEq)]
pub struct Strokes(Vec<Stroke>);
impl std::ops::Deref for Strokes {
    type Target = Vec<Stroke>;
    fn deref(&self) -> &Vec<Stroke> {
        &self.0
    }
}
impl std::fmt::Debug for Strokes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Strokes(<{}>)", self.0.len())
    }
}
impl Drop for Strokes {
    fn drop(&mut self) {
        for s in self.0.iter_mut() {
            // SAFETY: `s` is a valid, aligned, exclusive reference; volatile keeps the wipe from being optimised away.
            unsafe { std::ptr::write_volatile(s, Stroke { key: Key(0), shift: false }) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Type { strokes: Strokes, secret: bool },
    Key(Key),
    Chord(Vec<Key>),
    Delay(Duration),
    Repeat { count: u32, ops: Vec<(usize, Op)> },
    Wait(WaitSpec),
    Confirm(String),
    Click(u8),
    Move(i32, i32),
    Comment,
}

/// Events never carry typed text or secret values (docs/security.md).
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    Started { steps: usize },
    StepStarted { index: usize, kind: &'static str, detail: String },
    WaitPolling { index: usize, elapsed: Duration, best_similarity: f64 },
    Finished,
    Aborted { reason: String },
}

pub trait Host {
    fn key_down(&mut self, key: Key) -> Result<(), String>;
    fn key_up(&mut self, key: Key) -> Result<(), String>;
    fn release_all(&mut self) -> Result<(), String>;
    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), String>;
    fn mouse_button(&mut self, mask: u8, down: bool) -> Result<(), String>;
    fn sleep(&mut self, d: Duration);
    /// Latest video frame, if a capture device is open.
    fn screen(&mut self) -> Option<Frame>;
    fn reference(&mut self, name: &str) -> Result<Frame, String>;
    fn confirm(&mut self, message: &str) -> bool;
    fn cancelled(&self) -> bool;
    fn event(&mut self, ev: RunEvent);
}

#[derive(Debug, Clone)]
pub struct RunOptions {
    pub key_hold: Duration,
    pub key_gap: Duration,
    /// Applied after every top-level step (DuckyScript DEFAULT_DELAY).
    pub default_delay: Duration,
    pub poll: Duration,
    /// Walk the script and emit events without sending anything or sleeping.
    pub dry_run: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            key_hold: Duration::from_millis(8),
            key_gap: Duration::from_millis(8),
            default_delay: Duration::ZERO,
            poll: Duration::from_millis(250),
            dry_run: false,
        }
    }
}

// ---------- variables ----------

/// Effective values: declared defaults overridden by `provided`. Declared vars with neither are MissingVar
/// at use time (so a script may declare optional vars it never reaches).
fn substitute(
    template: &str,
    script: &Script,
    provided: &Vars,
    allow_secret: bool,
) -> Result<(String, bool), RunError> {
    let mut out = String::new();
    let mut used_secret = false;
    let mut rest = template;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        let j = after.find("}}").ok_or_else(|| RunError::BadTemplate("unterminated {{".into()))?;
        let name = after[..j].trim();
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(RunError::BadTemplate(format!("bad variable name {name:?}")));
        }
        let def = script.vars.get(name).ok_or_else(|| RunError::UndefinedVar(name.to_string()))?;
        if def.secret {
            if !allow_secret {
                return Err(RunError::SecretInPlainText { var: name.to_string() });
            }
            used_secret = true;
        }
        let v = provided
            .get(name)
            .or(def.default.as_ref())
            .ok_or_else(|| RunError::MissingVar(name.to_string()))?;
        out.push_str(v);
        rest = &after[j + 2..];
    }
    out.push_str(rest);
    Ok((out, used_secret))
}

// ---------- compile ----------

fn compile_steps(
    steps: &[Step],
    script: &Script,
    vars: &Vars,
    layout: &dyn Layout,
    top: Option<usize>,
) -> Result<Vec<(usize, Op)>, RunError> {
    let mut ops = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let idx = top.unwrap_or(i);
        let op = match step {
            Step::Text(_) | Step::SecretText(_) => {
                let secret = matches!(step, Step::SecretText(_));
                let t: &str = match step {
                    Step::Text(t) => t,
                    Step::SecretText(t) => t,
                    _ => unreachable!(),
                };
                let (mut text, used_secret) = substitute(t, script, vars, secret)?;
                let secret = secret || used_secret;
                let mut strokes = Vec::new();
                let mut bad = Vec::new();
                for c in text.chars() {
                    match layout.stroke(c) {
                        Some(s) => strokes.push(s),
                        None => bad.push(c),
                    }
                }
                text.zeroize();
                if !bad.is_empty() {
                    let count = bad.len();
                    let shown = (!secret).then(|| {
                        let mut b = bad.clone();
                        b.dedup();
                        b
                    });
                    bad.zeroize();
                    return Err(RunError::Untypable { step: idx, chars: shown, count });
                }
                Op::Type { strokes: Strokes(strokes), secret }
            }
            Step::Key(k) => Op::Key(parse_key(k).ok_or_else(|| RunError::UnknownKey(k.clone()))?),
            Step::Chord(ks) => Op::Chord(
                ks.iter().map(|k| parse_key(k).ok_or_else(|| RunError::UnknownKey(k.clone()))).collect::<Result<_, _>>()?,
            ),
            Step::Delay(d) => Op::Delay(*d),
            Step::Repeat { count, steps } => {
                Op::Repeat { count: *count, ops: compile_steps(steps, script, vars, layout, Some(idx))? }
            }
            Step::Wait(w) => Op::Wait(w.clone()),
            Step::Confirm(m) => Op::Confirm(m.clone()),
            Step::Click(b) => Op::Click(match b.to_ascii_lowercase().as_str() {
                "left" => MOUSE_LEFT,
                "right" => MOUSE_RIGHT,
                "middle" => MOUSE_MIDDLE,
                _ => return Err(RunError::UnknownButton(b.clone())),
            }),
            Step::MoveMouse { dx, dy } => Op::Move(*dx, *dy),
            Step::Comment(_) => Op::Comment,
        };
        ops.push((idx, op));
    }
    Ok(ops)
}

/// Validate and flatten a script against variable values and a layout. Nothing is sent.
pub fn compile(script: &Script, vars: &Vars, layout: &dyn Layout) -> Result<Vec<(usize, Op)>, RunError> {
    compile_steps(&script.steps, script, vars, layout, None)
}

// ---------- preview ----------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preview {
    pub steps: usize,
    pub typed_chars: usize,
    pub secret_chars: usize,
    pub needs_video: bool,
    pub has_confirm: bool,
    /// Plain (non-secret) text steps, for display. Secrets are counted, never listed.
    pub text_lines: Vec<String>,
    /// Plain text that looks like a command to run on the target.
    pub suspicious: Vec<String>,
    pub estimated_typing: Duration,
}

const RISKY: &[&str] = &[
    "powershell", "cmd.exe", "cmd /", "curl ", "wget ", "iex", "invoke-", "sudo ", "rm -", "del /", "format ",
    "net user", "reg add", "schtasks", "bash -c", "| sh", "|sh", "mshta", "certutil", "bitsadmin", "regsvr32",
];

/// Whether any step, including inside a `repeat`, waits on the screen (and so needs a capture card open).
pub fn needs_video(ops: &[(usize, Op)]) -> bool {
    ops.iter().any(|(_, op)| match op {
        Op::Wait(_) => true,
        Op::Repeat { ops, .. } => needs_video(ops),
        _ => false,
    })
}

pub fn preview(script: &Script, vars: &Vars, layout: &dyn Layout, opts: &RunOptions) -> Result<Preview, RunError> {
    fn walk(ops: &[(usize, Op)], mult: usize, p: &mut Preview, opts: &RunOptions, plain: &mut Vec<String>) {
        for (_, op) in ops {
            p.steps = p.steps.saturating_add(mult);
            match op {
                Op::Type { strokes, secret } => {
                    let n = strokes.len().saturating_mul(mult);
                    if *secret { p.secret_chars = p.secret_chars.saturating_add(n) } else { p.typed_chars = p.typed_chars.saturating_add(n) }
                    p.estimated_typing = p.estimated_typing.saturating_add((opts.key_hold + opts.key_gap).saturating_mul(n.min(u32::MAX as usize) as u32));
                    let _ = plain;
                }
                Op::Delay(d) => p.estimated_typing = p.estimated_typing.saturating_add(d.saturating_mul(mult.min(u32::MAX as usize) as u32)),
                Op::Wait(_) => p.needs_video = true,
                Op::Confirm(_) => p.has_confirm = true,
                Op::Repeat { count, ops } => walk(ops, mult.saturating_mul(*count as usize), p, opts, plain),
                _ => {}
            }
        }
    }
    let ops = compile(script, vars, layout)?;
    let mut p = Preview::default();
    walk(&ops, 1, &mut p, opts, &mut Vec::new());
    // Plain text lines come from the source steps (already validated as secret-free).
    fn collect(steps: &[Step], script: &Script, vars: &Vars, p: &mut Preview) {
        for s in steps {
            match s {
                Step::Text(t) => {
                    if let Ok((text, _)) = substitute(t, script, vars, false) {
                        let low = text.to_lowercase();
                        if RISKY.iter().any(|r| low.contains(r)) {
                            p.suspicious.push(text.clone());
                        }
                        p.text_lines.push(text);
                    }
                }
                Step::Repeat { steps, .. } => collect(steps, script, vars, p),
                _ => {}
            }
        }
    }
    collect(&script.steps, script, vars, &mut p);
    Ok(p)
}

// ---------- run ----------

struct Runner<'a> {
    host: &'a mut dyn Host,
    opts: &'a RunOptions,
}

impl Runner<'_> {
    fn pause(&mut self, d: Duration) {
        if !self.opts.dry_run && !d.is_zero() {
            self.host.sleep(d);
        }
    }
    fn check(&self) -> Result<(), RunError> {
        if self.host.cancelled() { Err(RunError::Cancelled) } else { Ok(()) }
    }
    fn h<T>(r: Result<T, String>) -> Result<T, RunError> {
        r.map_err(RunError::Host)
    }

    fn tap(&mut self, key: Key) -> Result<(), RunError> {
        Self::h(self.host.key_down(key))?;
        self.pause(self.opts.key_hold);
        Self::h(self.host.key_up(key))?;
        self.pause(self.opts.key_gap);
        Ok(())
    }

    fn op(&mut self, idx: usize, op: &Op) -> Result<(), RunError> {
        self.check()?;
        let dry = self.opts.dry_run;
        let (kind, detail) = match op {
            Op::Type { strokes, secret } => ("type", format!("{} chars{}", strokes.len(), if *secret { " (secret)" } else { "" })),
            Op::Key(_) => ("key", String::new()),
            Op::Chord(k) => ("chord", format!("{} keys", k.len())),
            Op::Delay(d) => ("delay", format!("{} ms", d.as_millis())),
            Op::Repeat { count, .. } => ("repeat", format!("x{count}")),
            Op::Wait(WaitSpec::Screen { .. }) => ("wait_screen", String::new()),
            Op::Wait(WaitSpec::Stable { .. }) => ("wait_stable", String::new()),
            Op::Confirm(_) => ("confirm", String::new()),
            Op::Click(_) => ("click", String::new()),
            Op::Move(..) => ("move", String::new()),
            Op::Comment => ("comment", String::new()),
        };
        self.host.event(RunEvent::StepStarted { index: idx, kind, detail });
        match op {
            Op::Type { strokes, .. } => {
                for s in strokes.iter() {
                    self.check()?;
                    if !dry {
                        if s.shift {
                            Self::h(self.host.key_down(Key::LEFT_SHIFT))?;
                        }
                        self.tap(s.key)?;
                        if s.shift {
                            Self::h(self.host.key_up(Key::LEFT_SHIFT))?;
                        }
                    }
                }
            }
            Op::Key(k) => {
                if !dry {
                    self.tap(*k)?
                }
            }
            Op::Chord(keys) => {
                if !dry {
                    for k in keys {
                        Self::h(self.host.key_down(*k))?;
                    }
                    self.pause(self.opts.key_hold);
                    for k in keys.iter().rev() {
                        Self::h(self.host.key_up(*k))?;
                    }
                    self.pause(self.opts.key_gap);
                }
            }
            Op::Delay(d) => {
                // Sleep in slices so an abort is honoured promptly.
                let mut left = *d;
                while !dry && !left.is_zero() {
                    self.check()?;
                    let step = left.min(Duration::from_millis(100));
                    self.host.sleep(step);
                    left -= step;
                }
            }
            Op::Repeat { count, ops } => {
                for _ in 0..*count {
                    for (i, o) in ops {
                        self.op(*i, o)?;
                    }
                }
            }
            Op::Wait(w) => {
                if !dry {
                    self.wait(idx, w)?
                }
            }
            Op::Confirm(m) => {
                if !dry && !self.host.confirm(m) {
                    return Err(RunError::ConfirmDeclined { step: idx });
                }
            }
            Op::Click(mask) => {
                if !dry {
                    Self::h(self.host.mouse_button(*mask, true))?;
                    self.pause(self.opts.key_hold);
                    Self::h(self.host.mouse_button(*mask, false))?;
                    self.pause(self.opts.key_gap);
                }
            }
            Op::Move(dx, dy) => {
                if !dry {
                    Self::h(self.host.mouse_move(*dx, *dy))?
                }
            }
            Op::Comment => {}
        }
        Ok(())
    }

    fn wait(&mut self, idx: usize, w: &WaitSpec) -> Result<(), RunError> {
        let (timeout, cont) = match w {
            WaitSpec::Screen { timeout, on_timeout_continue, .. } | WaitSpec::Stable { timeout, on_timeout_continue, .. } => {
                (*timeout, *on_timeout_continue)
            }
        };
        let reference = match w {
            WaitSpec::Screen { image, .. } => Some(self.host.reference(image).map_err(RunError::Reference)?),
            _ => None,
        };
        let poll = self.opts.poll;
        let mut elapsed = Duration::ZERO;
        let mut saw_frames = false;
        let mut best = 0.0f64;
        let mut prev: Option<Frame> = None;
        let mut stable_for = Duration::ZERO;
        loop {
            self.check()?;
            if let Some(frame) = self.host.screen() {
                saw_frames = true;
                match w {
                    WaitSpec::Screen { threshold, .. } => {
                        let sim = frame.similarity(reference.as_ref().expect("screen wait has reference"));
                        best = best.max(sim);
                        if sim >= *threshold {
                            return Ok(());
                        }
                    }
                    WaitSpec::Stable { stable_for: need, .. } => {
                        if let Some(p) = &prev {
                            let sim = p.similarity(&frame);
                            best = sim;
                            if sim >= 0.995 {
                                stable_for += poll;
                            } else {
                                stable_for = Duration::ZERO;
                            }
                            if stable_for >= *need {
                                return Ok(());
                            }
                        }
                        prev = Some(frame);
                    }
                }
            }
            self.host.event(RunEvent::WaitPolling { index: idx, elapsed, best_similarity: best });
            if elapsed >= timeout {
                return if cont { Ok(()) } else { Err(RunError::WaitTimeout { step: idx, saw_frames, best_similarity: best }) };
            }
            self.host.sleep(poll);
            elapsed += poll;
        }
    }
}

/// Run a compiled script. Always ends with `release_all` (success, error or abort) unless dry-running.
pub fn run(ops: &[(usize, Op)], host: &mut dyn Host, opts: &RunOptions) -> Result<(), RunError> {
    host.event(RunEvent::Started { steps: ops.len() });
    let mut r = Runner { host, opts };
    let mut result = Ok(());
    for (idx, op) in ops {
        result = r.op(*idx, op);
        if result.is_err() {
            break;
        }
        r.pause(opts.default_delay);
    }
    if !opts.dry_run {
        if let Err(e) = r.host.release_all() {
            if result.is_ok() {
                result = Err(RunError::Host(e));
            }
        }
    }
    match &result {
        Ok(()) => r.host.event(RunEvent::Finished),
        Err(e) => r.host.event(RunEvent::Aborted { reason: e.to_string() }),
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmit_layout::UsAnsi;

    #[derive(Default)]
    struct Mock {
        log: Vec<String>,
        frames: Vec<Frame>,
        frame_pos: usize,
        slept: Duration,
        cancel_after_sleeps: Option<usize>,
        sleeps: usize,
        confirm: bool,
        events: Vec<RunEvent>,
    }
    impl Host for Mock {
        fn key_down(&mut self, k: Key) -> Result<(), String> { self.log.push(format!("dn {:02x}", k.0)); Ok(()) }
        fn key_up(&mut self, k: Key) -> Result<(), String> { self.log.push(format!("up {:02x}", k.0)); Ok(()) }
        fn release_all(&mut self) -> Result<(), String> { self.log.push("release_all".into()); Ok(()) }
        fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), String> { self.log.push(format!("mv {dx} {dy}")); Ok(()) }
        fn mouse_button(&mut self, m: u8, d: bool) -> Result<(), String> { self.log.push(format!("btn {m} {d}")); Ok(()) }
        fn sleep(&mut self, d: Duration) { self.slept += d; self.sleeps += 1; }
        fn screen(&mut self) -> Option<Frame> {
            if self.frames.is_empty() { return None; }
            let f = self.frames[self.frame_pos.min(self.frames.len() - 1)].clone();
            self.frame_pos += 1;
            Some(f)
        }
        fn reference(&mut self, name: &str) -> Result<Frame, String> {
            if name == "ref.png" { Ok(Frame::new(64, 36, vec![200; 64 * 36]).unwrap()) } else { Err("missing".into()) }
        }
        fn confirm(&mut self, _: &str) -> bool { self.confirm }
        fn cancelled(&self) -> bool { self.cancel_after_sleeps.is_some_and(|n| self.sleeps >= n) }
        fn event(&mut self, ev: RunEvent) { self.events.push(ev); }
    }

    fn solid(v: u8) -> Frame { Frame::new(64, 36, vec![v; 64 * 36]).unwrap() }
    fn script(toml: &str) -> Script { Script::parse(&format!("format = 1\n{toml}")).unwrap() }
    fn compiled(s: &Script, vars: &Vars) -> Vec<(usize, Op)> { compile(s, vars, &UsAnsi).unwrap() }
    fn fast() -> RunOptions { RunOptions { key_hold: Duration::ZERO, key_gap: Duration::ZERO, ..Default::default() } }

    #[test]
    fn types_with_shift_and_always_releases() {
        let s = script("[[steps]]\ntext = \"Hi!\"\n[[steps]]\nchord = [\"CTRL\", \"ALT\", \"DELETE\"]\n");
        let mut m = Mock::default();
        run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap();
        assert_eq!(m.log, vec![
            "dn e1", "dn 0b", "up 0b", "up e1",  // H
            "dn 0c", "up 0c",                    // i
            "dn e1", "dn 1e", "up 1e", "up e1",  // !
            "dn e0", "dn e2", "dn 4c", "up 4c", "up e2", "up e0",
            "release_all",
        ]);
        assert_eq!(m.events.last(), Some(&RunEvent::Finished));
    }

    #[test]
    fn variables_defaults_secrets_and_leak_guard() {
        let s = script("[vars]\nuser = { default = \"tech\" }\npw = { secret = true }\n[[steps]]\ntext = \"{{user}}\"\n[[steps]]\nsecret_text = \"{{pw}}\"\n");
        assert_eq!(compile(&s, &Vars::new(), &UsAnsi).unwrap_err(), RunError::MissingVar("pw".into()));
        let vars: Vars = [("pw".to_string(), "x9".to_string())].into();
        let ops = compile(&s, &vars, &UsAnsi).unwrap();
        assert!(matches!(&ops[0].1, Op::Type { strokes, secret: false } if strokes.len() == 4));
        assert!(matches!(&ops[1].1, Op::Type { strokes, secret: true } if strokes.len() == 2));

        let leak = script("[vars]\npw = { secret = true }\n[[steps]]\ntext = \"{{pw}}\"\n");
        assert!(matches!(compile(&leak, &vars, &UsAnsi), Err(RunError::SecretInPlainText { .. })));
        let undefined = script("[[steps]]\ntext = \"{{nope}}\"\n");
        assert!(matches!(compile(&undefined, &vars, &UsAnsi), Err(RunError::UndefinedVar(_))));
        let bad = script("[[steps]]\ntext = \"{{oops\"\n");
        assert!(matches!(compile(&bad, &vars, &UsAnsi), Err(RunError::BadTemplate(_))));
    }

    #[test]
    fn untypable_secret_error_never_contains_the_characters() {
        let s = script("[vars]\npw = { secret = true }\n[[steps]]\nsecret_text = \"{{pw}}\"\n");
        let vars: Vars = [("pw".to_string(), "pä€".to_string())].into();
        let e = compile(&s, &vars, &UsAnsi).unwrap_err();
        assert_eq!(e, RunError::Untypable { step: 0, chars: None, count: 2 });
        assert!(!e.to_string().contains('ä'));
        let plain = script("[[steps]]\ntext = \"ä\"\n");
        assert!(matches!(compile(&plain, &Vars::new(), &UsAnsi), Err(RunError::Untypable { chars: Some(_), .. })));
    }

    #[test]
    fn debug_output_of_compiled_secrets_is_redacted_and_deep_repeats_do_not_overflow() {
        let s = script("[vars]\npw = { secret = true }\n[[steps]]\nsecret_text = \"{{pw}}\"\n");
        let vars: Vars = [("pw".to_string(), "abc".to_string())].into();
        let ops = compile(&s, &vars, &UsAnsi).unwrap();
        let dump = format!("{ops:?}");
        assert!(!dump.contains("Key(") && dump.contains("Strokes(<3>)"), "{dump}");
        let deep = script("[[steps]]\nrepeat = { count = 10000, steps = [ { repeat = { count = 10000, steps = [ { repeat = { count = 10000, steps = [ { repeat = { count = 10000, steps = [ { repeat = { count = 10000, steps = [ { text = \"a\" } ] } } ] } } ] } } ] } } ] }\n");
        let p = preview(&deep, &Vars::new(), &UsAnsi, &RunOptions::default()).unwrap();
        assert_eq!(p.typed_chars, 10_000usize.saturating_pow(5));
    }

    #[test]
    fn nothing_is_sent_when_any_step_is_invalid() {
        let s = script("[[steps]]\ntext = \"ok\"\n[[steps]]\nkey = \"NOPE\"\n");
        assert_eq!(compile(&s, &Vars::new(), &UsAnsi).unwrap_err(), RunError::UnknownKey("NOPE".into()));
    }

    #[test]
    fn repeat_expands() {
        let s = script("[[steps]]\nrepeat = { count = 3, steps = [ { key = \"TAB\" } ] }\n");
        let mut m = Mock::default();
        run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap();
        assert_eq!(m.log.iter().filter(|l| *l == "dn 2b").count(), 3);
    }

    #[test]
    fn wait_for_screen_succeeds_when_frame_matches() {
        let s = script("[[steps]]\nwait_for = { screen = \"ref.png\", timeout = \"10s\" }\n[[steps]]\nkey = \"ENTER\"\n");
        let mut m = Mock { frames: vec![solid(0), solid(0), solid(200)], ..Default::default() };
        run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap();
        assert_eq!(m.slept, Duration::from_millis(500)); // two polls before the match
        assert!(m.log.contains(&"dn 28".to_string()));
    }

    #[test]
    fn wait_for_screen_times_out_without_pressing_anything() {
        let s = script("[[steps]]\nwait_for = { screen = \"ref.png\", timeout = \"1s\" }\n[[steps]]\nkey = \"ENTER\"\n");
        let mut m = Mock { frames: vec![solid(0)], ..Default::default() };
        let e = run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap_err();
        assert!(matches!(e, RunError::WaitTimeout { saw_frames: true, .. }));
        assert_eq!(m.log, vec!["release_all"]);
        // no video at all is reported distinctly
        let mut none = Mock::default();
        let e = run(&compiled(&s, &Vars::new()), &mut none, &fast()).unwrap_err();
        assert!(matches!(e, RunError::WaitTimeout { saw_frames: false, .. }));
        assert!(e.to_string().contains("no video frames"));
    }

    #[test]
    fn wait_timeout_continue_and_stable() {
        let s = script("[[steps]]\nwait_for = { screen = \"ref.png\", timeout = \"500ms\", on_timeout = \"continue\" }\n[[steps]]\nwait_for = { stable_for = \"1s\", timeout = \"20s\" }\n[[steps]]\nkey = \"A\"\n");
        let mut m = Mock { frames: vec![solid(0), solid(50), solid(90), solid(90), solid(90), solid(90), solid(90), solid(90), solid(90)], ..Default::default() };
        run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap();
        assert!(m.log.contains(&"dn 04".to_string()));
    }

    #[test]
    fn confirm_declined_aborts_and_releases() {
        let s = script("[[steps]]\nconfirm = \"go?\"\n[[steps]]\nkey = \"A\"\n");
        let mut m = Mock::default();
        let e = run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap_err();
        assert_eq!(e, RunError::ConfirmDeclined { step: 0 });
        assert_eq!(m.log, vec!["release_all"]);
        let mut yes = Mock { confirm: true, ..Default::default() };
        run(&compiled(&s, &Vars::new()), &mut yes, &fast()).unwrap();
    }

    #[test]
    fn abort_mid_delay_releases_everything() {
        let s = script("[[steps]]\nchord = [\"CTRL\", \"C\"]\n[[steps]]\ndelay = \"10s\"\n");
        let mut m = Mock { cancel_after_sleeps: Some(3), ..Default::default() };
        let e = run(&compiled(&s, &Vars::new()), &mut m, &fast()).unwrap_err();
        assert_eq!(e, RunError::Cancelled);
        assert_eq!(m.log.last().map(String::as_str), Some("release_all"));
        assert!(m.slept < Duration::from_secs(1));
    }

    #[test]
    fn dry_run_sends_nothing() {
        let s = script("[[steps]]\ntext = \"abc\"\n[[steps]]\ndelay = \"5s\"\n[[steps]]\nwait_for = { stable_for = \"1s\", timeout = \"5s\" }\n[[steps]]\nconfirm = \"x\"\n");
        let mut m = Mock::default();
        run(&compiled(&s, &Vars::new()), &mut m, &RunOptions { dry_run: true, ..Default::default() }).unwrap();
        assert!(m.log.is_empty());
        assert_eq!(m.slept, Duration::ZERO);
        assert_eq!(m.events.iter().filter(|e| matches!(e, RunEvent::StepStarted { .. })).count(), 4);
    }

    #[test]
    fn events_never_contain_typed_text_or_secrets() {
        let s = script("[vars]\npw = { secret = true }\n[[steps]]\ntext = \"visible-text\"\n[[steps]]\nsecret_text = \"{{pw}}\"\n");
        let vars: Vars = [("pw".to_string(), "hunter2".to_string())].into();
        let mut m = Mock::default();
        run(&compiled(&s, &vars), &mut m, &fast()).unwrap();
        let dump = format!("{:?}", m.events);
        assert!(!dump.contains("hunter2") && !dump.contains("visible-text"), "{dump}");
        assert!(dump.contains("7 chars (secret)"));
    }

    #[test]
    fn a_wait_inside_a_repeat_still_needs_video() {
        let nested = script("[[steps]]\nrepeat = { count = 2, steps = [ { key = \"TAB\" }, { wait_for = { stable_for = \"1s\", timeout = \"5s\" } } ] }\n");
        let flat = script("[[steps]]\nrepeat = { count = 2, steps = [ { key = \"TAB\" } ] }\n");
        assert!(needs_video(&compile(&nested, &Vars::new(), &UsAnsi).unwrap()));
        assert!(!needs_video(&compile(&flat, &Vars::new(), &UsAnsi).unwrap()));
    }

    #[test]
    fn preview_counts_flags_and_lists_risky_text() {
        let s = script("[vars]\npw = { secret = true }\n[[steps]]\ntext = \"powershell -c whoami\"\n[[steps]]\nsecret_text = \"{{pw}}\"\n[[steps]]\nrepeat = { count = 4, steps = [ { text = \"ab\" } ] }\n[[steps]]\nwait_for = { stable_for = \"1s\", timeout = \"5s\" }\n[[steps]]\nconfirm = \"ok\"\n");
        let vars: Vars = [("pw".to_string(), "12345".to_string())].into();
        let p = preview(&s, &vars, &UsAnsi, &RunOptions::default()).unwrap();
        assert_eq!(p.typed_chars, 20 + 8);
        assert_eq!(p.secret_chars, 5);
        assert!(p.needs_video && p.has_confirm);
        assert_eq!(p.suspicious, vec!["powershell -c whoami".to_string()]);
        assert!(!p.text_lines.iter().any(|l| l.contains("12345")));
    }
}
