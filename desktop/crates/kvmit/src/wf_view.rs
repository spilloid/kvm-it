//! What the workflow panel shows, derived from a run's trace (no egui widgets, no I/O), so it is unit-tested like
//! `uistate`: the state chip, the transition timeline, and the names of exported files. `gui.rs` only draws these.
use crate::uistate::Health;
use kvmit_script::workflow::{TraceEntry, WorkflowEvent};
use std::collections::BTreeMap;

/// The current state, from the trace: the last state entered, and how the run stands.
pub fn state_chip(trace: &[TraceEntry], running: bool) -> (Health, String) {
    let state = trace.iter().rev().find_map(|t| match &t.event {
        WorkflowEvent::StateEntered { state, .. } => Some(state.as_str()),
        _ => None,
    });
    let last = trace.last().map(|t| &t.event);
    match (last, state) {
        (Some(WorkflowEvent::RunCompleted { state }), _) => (Health::Good, format!("Done: {state}")),
        (Some(WorkflowEvent::Stopped { .. }), Some(s)) => (Health::Bad, format!("Stopped in {s}")),
        (Some(WorkflowEvent::Stopped { .. }), None) => (Health::Bad, "Stopped before the first screen".into()),
        (Some(WorkflowEvent::ManualStepRequested { state, .. }), _) if running => {
            (Health::Working, format!("{state}: your turn (do the step on the target)"))
        }
        (_, Some(s)) if running => {
            let attempt = trace.iter().rev().find_map(|t| match &t.event {
                WorkflowEvent::ActionAttempted { state, attempt, .. } if state == s => Some(*attempt),
                WorkflowEvent::StateEntered { .. } => Some(0),
                _ => None,
            });
            match attempt {
                Some(n) if n > 1 => (Health::Working, format!("{s} (attempt {n})")),
                _ => (Health::Working, s.to_string()),
            }
        }
        (_, None) if running => (Health::Working, "Looking at the screen…".into()),
        (_, Some(s)) => (Health::Idle, s.to_string()),
        (_, None) => (Health::Idle, "Not started".into()),
    }
}

/// One line of the timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Seconds since the run started.
    pub at: f64,
    pub text: String,
    pub health: Health,
}

/// The events worth a line, in order. Observations and context changes are left to the inspector.
pub fn timeline(trace: &[TraceEntry]) -> Vec<Row> {
    trace
        .iter()
        .filter_map(|t| {
            let (text, health) = match &t.event {
                WorkflowEvent::StateEntered { state, reason, .. } => (format!("Entered {state} ({reason})"), Health::Idle),
                WorkflowEvent::PicturesMissing { states } => (
                    format!("Not recognised this run (picture not saved): {}. If one appears, the run usually stops there (it may be taken for a similar screen).", states.join(", ")),
                    Health::Working,
                ),
                // A state with nothing to type only waits for the target (updates, a restart, enrolment).
                WorkflowEvent::ActionAttempted { state, steps: 0, .. } => {
                    (format!("Waiting in {state} for the screen to move on"), Health::Working)
                }
                WorkflowEvent::ManualStepRequested { state, message, timeout_ms } => (
                    format!("Your turn in {state}: {message} (waiting up to {})", wait_text(*timeout_ms)),
                    Health::Working,
                ),
                WorkflowEvent::ActionAttempted { state, attempt, steps, dry_run } => (
                    format!(
                        "{} {steps} step(s) in {state}{}",
                        if *dry_run { "Would send" } else { "Sent" },
                        if *attempt > 1 { format!(", attempt {attempt}") } else { String::new() }
                    ),
                    Health::Working,
                ),
                WorkflowEvent::TransitionVerified { from, to, confidence, .. } => {
                    (format!("Verified {from} → {to} ({:.0}% match)", confidence * 100.0), Health::Good)
                }
                WorkflowEvent::TenantVerified { tenant, confidence, .. } => {
                    (format!("Tenant {tenant} confirmed ({:.0}% match)", confidence * 100.0), Health::Good)
                }
                WorkflowEvent::GuardChecked { state, ok: false, detail } => {
                    (format!("Check failed in {state}: {detail}"), Health::Bad)
                }
                WorkflowEvent::HumanGateRequested { message, approved, .. } => (
                    format!("Asked: {message} — {}", if *approved { "yes" } else { "no" }),
                    if *approved { Health::Idle } else { Health::Bad },
                ),
                WorkflowEvent::StateTimedOut { state, attempts } => {
                    (format!("{state}: the screen did not move on after {attempts} attempt(s)"), Health::Bad)
                }
                WorkflowEvent::Stopped { reason, .. } => (format!("Stopped: {reason}"), Health::Bad),
                WorkflowEvent::RunCompleted { state } => (format!("Completed in {state}"), Health::Good),
                _ => return None,
            };
            Some(Row { at: t.at_ms as f64 / 1000.0, text, health })
        })
        .collect()
}

/// What `kvmit run` prints for a trace entry as it happens: the timeline's line, and each recorded fact (the app shows
/// those in its variables table).
pub fn cli_lines(t: &TraceEntry) -> Vec<String> {
    let at = t.at_ms as f64 / 1000.0;
    let mut out: Vec<String> = timeline(std::slice::from_ref(t)).into_iter().map(|r| format!("{at:>7.1}s  {}", r.text)).collect();
    if let WorkflowEvent::ContextChanged { key, to, .. } = &t.event {
        out.push(format!("{at:>7.1}s    {key} = {to}"));
    }
    out
}

/// A workflow file as a checkpoint's source: its full path where it can be resolved, so the app (which lists the
/// scripts folder) and `kvmit run` (given any path to the same file) name it alike.
pub fn file_source(path: &std::path::Path) -> Source {
    Source::File(std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// One reference image a workflow compares against, and what uses it.
#[derive(Debug, Clone, PartialEq)]
pub struct Reference {
    pub image: String,
    /// "state REGION", "tenant contoso".
    pub used_by: Vec<String>,
    /// Where it is (or should be saved), or why it cannot be.
    pub path: Result<std::path::PathBuf, String>,
    pub present: bool,
}

/// Where a reference image lives: relative to the script's or workflow's folder, never outside it. The run host reads
/// references through this, so a check and a run agree.
pub fn reference_path(base: &std::path::Path, name: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::Path::new(name);
    if name.is_empty() || p.is_absolute() || p.components().any(|c| !matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir)) {
        return Err(format!("reference image {name:?} must be a relative path inside the file's folder"));
    }
    Ok(base.join(p))
}

/// Save a picture as a reference image, only inside `base`. Each folder on the way is checked before the next is
/// created: one that resolves outside `base` (a link or junction) stops the save before anything is made there. An
/// existing destination must be a plain file. The picture is written to a new file beside it and renamed over it, so a
/// destination hard-linked elsewhere is replaced, never written through. Returns where it was saved.
pub fn save_reference(base: &std::path::Path, name: &str, w: usize, h: usize, rgba: &[u8]) -> Result<std::path::PathBuf, String> {
    let path = reference_path(base, name)?;
    // Saved as PNG, so only a .png name reads back as what was saved.
    if !path.extension().is_some_and(|x| x.eq_ignore_ascii_case("png")) {
        return Err(format!("{name:?}: reference screens are saved as PNG; give the state a .png name"));
    }
    // The workflow's own folder (for a built-in, the scripts folder, which may not exist yet on a fresh install).
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let root = std::fs::canonicalize(base).map_err(|e| e.to_string())?;
    let outside = || format!("{name:?} leads outside the workflow's folder (through a link); not saved");
    let rel = path.strip_prefix(base).map_err(|_| outside())?;
    let mut dir = root.clone();
    let folders: Vec<_> = rel.parent().map(|p| p.components().collect()).unwrap_or_default();
    for c in folders {
        if matches!(c, std::path::Component::CurDir) {
            continue;
        }
        let next = dir.join(c);
        if std::fs::symlink_metadata(&next).is_err() {
            std::fs::create_dir(&next).map_err(|e| e.to_string())?;
        }
        dir = std::fs::canonicalize(&next).map_err(|e| e.to_string())?;
        if !dir.starts_with(&root) || !dir.is_dir() {
            return Err(outside());
        }
    }
    let file = path.file_name().ok_or("no file name")?;
    let dest = dir.join(file);
    if let Ok(m) = std::fs::symlink_metadata(&dest) {
        if !m.is_file() {
            return Err(format!("{} is a link or a folder, not a picture; not saved", dest.display()));
        }
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = dir.join(format!(".{}.{}-{nanos}.tmp.png", file.to_string_lossy(), std::process::id()));
    let saved = kvmit_video::convert::save_png(&tmp, w, h, rgba)
        .map_err(|e| e.to_string())
        .and_then(|_| std::fs::rename(&tmp, &dest).map_err(|e| e.to_string()));
    if let Err(e) = saved {
        // Say so if the screenshot is left behind under its temporary name.
        return Err(match std::fs::remove_file(&tmp) {
            Err(r) if r.kind() != std::io::ErrorKind::NotFound => format!("{e}; {} was left behind: {r}", tmp.display()),
            _ => e,
        });
    }
    Ok(dest)
}

/// Every reference image of a workflow, in file order (states, then tenants), each listed once.
pub fn references(wf: &kvmit_script::Workflow, base: &std::path::Path) -> Vec<Reference> {
    let mut out: Vec<Reference> = Vec::new();
    let uses = wf
        .states
        .iter()
        .flat_map(|s| s.detect.screens.iter().map(move |i| (i.clone(), format!("state {}", s.name))))
        .chain(wf.tenants.iter().map(|(t, i)| (i.clone(), format!("tenant {t}"))));
    for (image, by) in uses {
        match out.iter_mut().find(|r| r.image == image) {
            Some(r) => r.used_by.push(by),
            None => {
                let path = reference_path(base, &image);
                let present = path.as_ref().is_ok_and(|p| p.is_file());
                out.push(Reference { image, used_by: vec![by], path, present });
            }
        }
    }
    out
}

/// "90 s", "10 min": how long a manual step may take, for the timeline.
fn wait_text(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 120 && s.is_multiple_of(60) { format!("{} min", s / 60) } else { format!("{s} s") }
}

/// Save a run's trace and notes as `workflow-run-<stamp>[-n].trace.jsonl` and `.notes.md` in `dir`. The name never
/// comes from the workflow (its name could hold a secret's value the engine refused), files are created new (an
/// existing export is never overwritten: a numbered name is taken instead), and a pair is never left half written.
/// Returns the stem used.
pub fn save_exports(dir: &std::path::Path, stamp: &str, jsonl: &str, md: &str) -> std::io::Result<String> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    // Removing a file this call created: if that fails, say which file is left, rather than carry on as if it were not.
    let undo = |p: &std::path::Path| {
        std::fs::remove_file(p).map_err(|e| std::io::Error::new(e.kind(), format!("{} was left behind: {e}", p.display())))
    };
    for n in 0..1000 {
        let stem = if n == 0 { format!("workflow-run-{stamp}") } else { format!("workflow-run-{stamp}-{n}") };
        let (a, b) = (dir.join(format!("{stem}.trace.jsonl")), dir.join(format!("{stem}.notes.md")));
        let open = |p: &std::path::Path| std::fs::OpenOptions::new().write(true).create_new(true).open(p);
        let mut fa = match open(&a) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        let mut fb = match open(&b) {
            Ok(f) => f,
            Err(e) => {
                drop(fa);
                undo(&a)?;
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(e);
            }
        };
        let written = fa.write_all(jsonl.as_bytes()).and_then(|_| fb.write_all(md.as_bytes()));
        if let Err(e) = written {
            drop((fa, fb));
            // Try to remove both, whatever happens to the first, and name everything left behind.
            let left: Vec<String> = [undo(&a), undo(&b)].into_iter().filter_map(|r| r.err().map(|e| e.to_string())).collect();
            if left.is_empty() {
                return Err(e);
            }
            return Err(std::io::Error::new(e.kind(), format!("{e}; {}", left.join("; "))));
        }
        return Ok(stem);
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many exports with this timestamp"))
}

/// File names for a run's exports, from the workflow name (made safe for a file name) and a local timestamp.
pub fn export_stem(workflow: &str, stamp: &str) -> String {
    let safe: String = workflow
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let safe = if safe.is_empty() { "workflow".to_string() } else { safe.chars().take(60).collect() };
    format!("{safe}-{stamp}")
}

/// What a Resume button may resume from: only a stopped **live** run (a dry run sent nothing, so its attempt counts mean
/// nothing to a live one) of **exactly** the selected definition (another workflow's facts and history, or this file's
/// before it was edited, mean nothing here).
/// Where a workflow came from. Paths are compared as paths, exactly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Source {
    File(std::path::PathBuf),
    Builtin(String),
}

/// `ran` and `selected` are (definition, source): two files with the same contents are still two workflows.
pub fn resume_point(
    dry: bool,
    ran: (&kvmit_script::Workflow, &Source),
    selected: (&kvmit_script::Workflow, &Source),
    report: Option<&kvmit_script::WorkflowReport>,
) -> Option<kvmit_script::Checkpoint> {
    if dry || ran != selected {
        return None;
    }
    report.filter(|r| r.result.is_err()).and_then(|r| r.checkpoint(ran.0))
}

/// FNV-1a, as hex: an identity for a file's exact text or a source, not a security measure.
fn fnv(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// A new copy's default file name: the workflow's name with the organisation (its `tenant` setup value) appended.
pub fn copy_file_name(file: &str, values: &BTreeMap<String, String>) -> String {
    let stem = file.strip_suffix(".toml").unwrap_or(file);
    match values.get("tenant").filter(|t| !t.is_empty()) {
        Some(t) => format!("{stem}-{t}.toml"),
        None => format!("{stem}-copy.toml"),
    }
}

/// A plain file name for the scripts folder: `<name>.toml`, of letters, digits, spaces, `.`, `-` and `_`, not hidden.
pub fn check_file_name(name: &str) -> Result<(), String> {
    let stem = name.strip_suffix(".toml").unwrap_or("");
    let plain = !stem.is_empty()
        && stem.len() <= 100
        && !stem.starts_with(['.', ' '])
        && !stem.ends_with(['.', ' '])
        && stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'));
    if !plain {
        return Err("a file name ending in .toml, of letters, digits, spaces, . - and _".into());
    }
    // Windows device names, with any extension: `con.toml` is not a file there.
    let device = stem.split('.').next().unwrap_or("").trim().to_ascii_uppercase();
    let reserved = matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((device.starts_with("COM") || device.starts_with("LPT")) && device.len() == 4 && device.as_bytes()[3].is_ascii_digit());
    match reserved {
        true => Err(format!("{device} is a device name on Windows: choose another file name")),
        false => Ok(()),
    }
}

/// What a dry run passes for prompted secrets left empty: a dry run sends nothing, so it needs no real value. Unique
/// to this run, so it never matches text in the workflow file (which the secret checks would refuse).
pub fn dry_run_secrets(wf: &kvmit_script::Workflow, given: &kvmit_script::Vars) -> kvmit_script::Vars {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut all = given.clone();
    for k in wf.secrets.keys() {
        all.entry(k.clone()).or_insert_with(|| format!("dry-run-{}-{nanos:x}-{k}", std::process::id()));
    }
    all
}

/// Write a new workflow file into the scripts folder. Never replaces a file already there.
pub fn write_new_workflow(dir: &std::path::Path, name: &str, text: &str) -> Result<std::path::PathBuf, String> {
    check_file_name(name)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(name);
    use std::io::Write;
    let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!("{} already exists: choose another name, or select that file and change its profile", path.display()))
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if let Err(e) = f.write_all(text.as_bytes()).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(&path);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(path)
}

/// Replace a workflow file's text, only if it still holds `was` (what the caller read and changed): a change made
/// elsewhere in between is never overwritten. Written to a temporary file and renamed, so a reader sees one whole
/// version or the other.
pub fn rewrite_workflow(path: &std::path::Path, was: &str, text: &str) -> Result<(), String> {
    // One save at a time (the app and `kvmit workflow set`): the lock covers the comparison and the replacement. An
    // editor saving in that instant is not covered.
    let lock = path.with_extension("toml.saving");
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "another save of {} is in progress (or one crashed: then delete {})",
                path.display(),
                lock.display()
            ))
        }
        Err(e) => return Err(format!("{}: {e}", lock.display())),
    }
    let r = replace_if_unchanged(path, was, text);
    let _ = std::fs::remove_file(&lock);
    r
}

fn replace_if_unchanged(path: &std::path::Path, was: &str, text: &str) -> Result<(), String> {
    let now = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if now != was {
        return Err(format!("{} changed since it was opened: reload it and set it up again", path.display()));
    }
    use std::io::Write;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = path.with_extension(format!("toml.{}-{nanos}.tmp", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|_| f.sync_all()))
        .and_then(|_| std::fs::rename(&tmp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(|e| format!("{}: {e}", path.display()))
}

/// Which run a checkpoint belongs to: a workflow (its file, or its built-in name) **on one adapter**. Several targets
/// may run the same workflow at once, one adapter each (`kvmit run --device`); each keeps its own checkpoint, and a
/// checkpoint of one target is never resumed on another.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Slot {
    pub source: Source,
    /// The adapter's id (its Bluetooth address; on macOS the id macOS gave it), in capitals: ids are compared without
    /// regard to case when connecting, so `aa:…` and `AA:…` must be one slot.
    pub adapter: String,
}

impl Slot {
    pub fn new(source: Source, adapter: &str) -> Slot {
        Slot { source, adapter: adapter.trim().to_ascii_uppercase() }
    }
}

/// A stopped run's checkpoint, saved so a later run (in the app or `kvmit run`) can resume it. It holds no secrets
/// and no images (see [`kvmit_script::Checkpoint`]), and names the very file, text and adapter it came from: a
/// checkpoint of another file, of this file before it was edited, or of another target is never offered.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedCheckpoint {
    pub slot: Slot,
    /// The workflow file's exact text, hashed.
    pub definition: String,
    pub checkpoint: kvmit_script::Checkpoint,
}

/// Where a slot's checkpoint lives: one file per workflow and adapter, under `<scripts>/checkpoints/`. `None` when the
/// source cannot be named exactly (a file name that is not Unicode): such a run is not saved for resuming.
pub fn checkpoint_path(dir: &std::path::Path, slot: &Slot) -> Option<std::path::PathBuf> {
    let id = serde_json::to_string(slot).ok()?;
    Some(dir.join("checkpoints").join(format!("{}.checkpoint.json", fnv(id.as_bytes()))))
}

/// Save (or replace) the checkpoint for this slot. Written to a temporary file and renamed, so a reader never sees
/// half a checkpoint.
pub fn save_checkpoint(
    dir: &std::path::Path,
    slot: &Slot,
    text: &str,
    checkpoint: &kvmit_script::Checkpoint,
) -> std::io::Result<std::path::PathBuf> {
    let unnamed = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "the workflow's file name is not Unicode");
    let path = checkpoint_path(dir, slot).ok_or_else(unnamed)?;
    let saved = SavedCheckpoint { slot: slot.clone(), definition: fnv(text.as_bytes()), checkpoint: checkpoint.clone() };
    let json = serde_json::to_string_pretty(&saved).map_err(|_| unnamed())?;
    std::fs::create_dir_all(path.parent().expect("under checkpoints/"))?;
    // A temporary file of this writer's own, created new, then renamed over the checkpoint: a reader sees one whole
    // checkpoint or the other, never a mix.
    use std::io::Write;
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = path.with_extension(format!("json.{}-{nanos}.tmp", std::process::id()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(json.as_bytes()).and_then(|_| f.sync_all()))
        .and_then(|_| std::fs::rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|_| path)
}

/// The saved checkpoint for exactly this slot and text, if there is one. A checkpoint of an earlier version of the
/// file is not returned (`Ok(None)`); an unreadable one is an error, so it is reported rather than silently ignored.
pub fn load_checkpoint(dir: &std::path::Path, slot: &Slot, text: &str) -> Result<Option<kvmit_script::Checkpoint>, String> {
    Ok(load_checkpoint_raw(dir, slot, text)?.map(|(_, cp)| cp))
}

/// [`load_checkpoint`], with the checkpoint file's exact text: what a later look compares to tell whether another run
/// saved a new one in between.
pub fn load_checkpoint_raw(
    dir: &std::path::Path,
    slot: &Slot,
    text: &str,
) -> Result<Option<(String, kvmit_script::Checkpoint)>, String> {
    let Some(path) = checkpoint_path(dir, slot) else { return Ok(None) };
    read_saved(&path, slot, text)
}

/// A checkpoint file, if it is this slot's and of this very text.
fn read_saved(path: &std::path::Path, slot: &Slot, text: &str) -> Result<Option<(String, kvmit_script::Checkpoint)>, String> {
    let json = match std::fs::read_to_string(path) {
        Ok(j) => j,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let saved: SavedCheckpoint = serde_json::from_str(&json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((saved.slot == *slot && saved.definition == fnv(text.as_bytes())).then_some((json, saved.checkpoint)))
}

/// A live run's hold on its slot, from before it sends anything until [`keep_checkpoint`]: a lock file only one run
/// can create, and the checkpoint it set aside.
#[derive(Debug)]
pub struct Claim {
    lock: std::path::PathBuf,
    /// The saved checkpoint, renamed so it is never resumed while this run goes on.
    held: Option<std::path::PathBuf>,
}

/// Before a live run sends anything: take the slot's lock (created new, so a second run of this workflow on this
/// adapter is refused, in any window or process) and set its saved checkpoint aside (renamed to `.claimed`, never
/// resumed). From then on no checkpoint older than this run can be resumed. A lock left by a run that crashed stays
/// until removed by hand: that is the safe side, since that run may have sent keys. If either step fails, the run
/// must not start.
pub fn claim_checkpoint(dir: &std::path::Path, slot: &Slot) -> Result<Option<Claim>, String> {
    let Some(path) = checkpoint_path(dir, slot) else { return Ok(None) };
    std::fs::create_dir_all(path.parent().expect("under checkpoints/")).map_err(|e| e.to_string())?;
    let lock = path.with_extension("json.lock");
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(format!(
                "another run of this workflow on this adapter is in progress (or one crashed: then delete {} and check the target before running)",
                lock.display()
            ))
        }
        Err(e) => return Err(format!("cannot lock this workflow's checkpoint ({}): {e}", lock.display())),
    }
    let held = path.with_extension("json.claimed");
    match std::fs::rename(&path, &held) {
        Ok(()) => Ok(Some(Claim { lock, held: Some(held) })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(Claim { lock, held: None })),
        Err(e) => {
            let _ = std::fs::remove_file(&lock);
            Err(format!(
                "the workflow's saved checkpoint {} cannot be set aside ({e}); fix that (or delete it) before running",
                path.display()
            ))
        }
    }
}

impl Claim {
    /// The checkpoint this claim set aside, read while the slot is locked (so no other run can change it meanwhile):
    /// what a resume must start from. `None` if there was none.
    pub fn held(&self, slot: &Slot, text: &str) -> Result<Option<(String, kvmit_script::Checkpoint)>, String> {
        match &self.held {
            Some(p) => read_saved(p, slot, text),
            None => Ok(None),
        }
    }
}

/// Give a claim back without running (the run was refused after claiming): the checkpoint is put back and the slot
/// unlocked. Both are attempted; whatever failed is reported with the file it left behind.
pub fn release_claim(dir: &std::path::Path, slot: &Slot, claim: Claim) -> Result<(), String> {
    let mut failed = Vec::new();
    if let (Some(held), Some(path)) = (&claim.held, checkpoint_path(dir, slot)) {
        if let Err(e) = std::fs::rename(held, path) {
            failed.push(format!("the checkpoint is left at {} ({e})", held.display()));
        }
    }
    if let Err(e) = std::fs::remove_file(&claim.lock) {
        failed.push(format!("the lock {} could not be removed ({e}): delete it before running", claim.lock.display()));
    }
    if failed.is_empty() { Ok(()) } else { Err(failed.join("; ")) }
}

/// The checkpoint a resume starts from, taken under the slot's lock: it must be the very one that was shown (its file's
/// exact text), or the claim is given back and the resume refused.
pub fn resume_from_claim(
    dir: &std::path::Path,
    slot: &Slot,
    text: &str,
    claim: Option<Claim>,
    shown: &str,
) -> Result<(Option<Claim>, kvmit_script::Checkpoint), String> {
    let changed = "the checkpoint changed since it was shown (another run saved one): look at the target and resume again";
    let Some(claim) = claim else { return Err(changed.into()) };
    match claim.held(slot, text) {
        Ok(Some((now, cp))) if now == shown => Ok((Some(claim), cp)),
        other => {
            let why = match other {
                Err(e) => e,
                _ => changed.into(),
            };
            Err(match release_claim(dir, slot, claim) {
                Ok(()) => why,
                Err(left) => format!("{why}; and {left}"),
            })
        }
    }
}

/// Whether a run sent any key (a dry run never does). An attempt is recorded before its keys go out, so a burst cut
/// short counts as sent: the safe side.
fn sent_keys(report: &kvmit_script::WorkflowReport) -> bool {
    report.trace.iter().any(|t| {
        matches!(t.event, kvmit_script::WorkflowEvent::ActionAttempted { steps, dry_run: false, .. } if steps > 0)
    })
}

/// After a run: a stopped live run with a known state saves its checkpoint. The checkpoint the run's `claim` set
/// aside is discarded once it sent keys or completed; if it sent nothing and saved nothing (it stopped before
/// recognising a screen), it is put back, so resuming is still possible. Then the lock is released. A dry run changes
/// nothing. Returns what to tell the user.
pub fn keep_checkpoint(
    dir: &std::path::Path,
    slot: &Slot,
    text: &str,
    dry: bool,
    wf: &kvmit_script::Workflow,
    report: &kvmit_script::WorkflowReport,
    claim: Option<&Claim>,
) -> Option<String> {
    if dry {
        return None;
    }
    let sent = sent_keys(report);
    let saved = if report.result.is_ok() { None } else { report.checkpoint(wf).map(|cp| (save_checkpoint(dir, slot, text, &cp), cp.state)) };
    let restore = !sent && report.result.is_err() && !matches!(saved, Some((Ok(_), _)));
    // `Some(Ok)`: the earlier checkpoint is back; `Some(Err)`: it could not be put back, and is left where it is.
    let mut restored: Option<Result<(), String>> = None;
    if let Some(c) = claim {
        if let Some(held) = &c.held {
            match checkpoint_path(dir, slot) {
                Some(path) if restore => {
                    restored = Some(std::fs::rename(held, &path).map_err(|e| format!("{} could not be put back ({e})", held.display())));
                }
                _ => {
                    let _ = std::fs::remove_file(held);
                }
            }
        }
        let _ = std::fs::remove_file(&c.lock);
    }
    let lost = |why: String| format!("{why}, so this run cannot be resumed: run it again from the start.");
    match (saved, restored) {
        (Some((Ok(_), state)), _) => Some(format!("Saved a checkpoint at {state}: Resume continues from there after looking at the screen.")),
        (Some((Err(e), _)), Some(Ok(()))) => Some(format!("Could not save a checkpoint ({e}); the earlier one is back, as nothing was sent.")),
        (Some((Err(e), _)), Some(Err(r))) => Some(lost(format!("Could not save a checkpoint ({e}), and the earlier one {r}"))),
        (Some((Err(e), _)), None) => Some(lost(format!("Could not save a checkpoint ({e})"))),
        (None, Some(Err(r))) => Some(format!("Nothing was sent, but the earlier checkpoint {r}: it cannot be resumed until it is renamed back.")),
        (None, _) => None,
    }
}

/// A run's outcome for the run-log header: the operator stopping it is Aborted, anything else Failed.
pub fn outcome(result: &Result<(), kvmit_script::WorkflowError>) -> crate::uistate::RunOutcome {
    use crate::uistate::RunOutcome;
    use kvmit_script::WorkflowError;
    match result {
        Ok(()) => RunOutcome::Finished,
        Err(e @ (WorkflowError::Cancelled | WorkflowError::GateDeclined { .. } | WorkflowError::ResumeDeclined { .. })) => {
            RunOutcome::Aborted(e.to_string())
        }
        Err(e) => RunOutcome::Failed(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uistate::RunOutcome;
    use kvmit_script::WorkflowError;

    fn at(ms: u64, event: WorkflowEvent) -> TraceEntry {
        TraceEntry { at_ms: ms, event }
    }
    fn entered(s: &str) -> WorkflowEvent {
        WorkflowEvent::StateEntered { state: s.into(), from: None, reason: "observed at start".into() }
    }

    #[test]
    fn the_chip_follows_the_run() {
        assert_eq!(state_chip(&[], false), (Health::Idle, "Not started".into()));
        assert_eq!(state_chip(&[], true).0, Health::Working);
        let mut t = vec![at(0, entered("REGION"))];
        assert_eq!(state_chip(&t, true), (Health::Working, "REGION".into()));
        t.push(at(10, WorkflowEvent::ActionAttempted { state: "REGION".into(), attempt: 2, steps: 1, dry_run: false }));
        assert_eq!(state_chip(&t, true), (Health::Working, "REGION (attempt 2)".into()));
        t.push(at(20, WorkflowEvent::Stopped { state: Some("REGION".into()), reason: "x".into() }));
        assert_eq!(state_chip(&t, false), (Health::Bad, "Stopped in REGION".into()));
        let done = vec![at(0, entered("A")), at(5, WorkflowEvent::RunCompleted { state: "A".into() })];
        assert_eq!(state_chip(&done, false), (Health::Good, "Done: A".into()));
        let early = vec![at(0, WorkflowEvent::Stopped { state: None, reason: "x".into() })];
        assert_eq!(state_chip(&early, false).1, "Stopped before the first screen");
    }

    #[test]
    fn the_timeline_keeps_the_events_that_matter() {
        let t = vec![
            at(0, WorkflowEvent::Started { workflow: "w".into(), dry_run: false, resumed_from: None }),
            at(100, entered("REGION")),
            at(1500, WorkflowEvent::ActionAttempted { state: "REGION".into(), attempt: 1, steps: 2, dry_run: true }),
            at(3000, WorkflowEvent::TransitionVerified { from: "REGION".into(), to: "KEYBOARD".into(), confidence: 0.995, duration_ms: 1 }),
            at(3000, WorkflowEvent::ContextChanged { key: "derived.x".into(), from: None, to: "y".into() }),
            at(9000, WorkflowEvent::StateTimedOut { state: "KEYBOARD".into(), attempts: 1 }),
        ];
        let rows = timeline(&t);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Entered REGION (observed at start)",
                "Would send 2 step(s) in REGION",
                "Verified REGION → KEYBOARD (100% match)",
                "KEYBOARD: the screen did not move on after 1 attempt(s)"
            ]
        );
        assert_eq!(rows[2].health, Health::Good);
        assert_eq!(rows[3].health, Health::Bad);
        assert_eq!(rows[1].at, 1.5);
    }

    #[test]
    fn manual_and_waiting_states_read_as_such() {
        let manual = WorkflowEvent::ManualStepRequested { state: "MFA".into(), message: "Approve on the phone".into(), timeout_ms: 600_000 };
        let t = vec![
            at(0, entered("UPDATES")),
            at(10, WorkflowEvent::ActionAttempted { state: "UPDATES".into(), attempt: 1, steps: 0, dry_run: false }),
            at(20, entered("MFA")),
            at(30, manual),
        ];
        let texts: Vec<String> = timeline(&t).into_iter().map(|r| r.text).collect();
        assert_eq!(texts[1], "Waiting in UPDATES for the screen to move on");
        assert_eq!(texts[3], "Your turn in MFA: Approve on the phone (waiting up to 10 min)");
        assert_eq!(state_chip(&t, true).1, "MFA: your turn (do the step on the target)");
        assert_eq!(wait_text(90_000), "90 s");
    }

    #[test]
    fn references_are_listed_once_with_their_users_and_stay_inside_the_folder() {
        let src = "workflow_format = 1\nstart = [\"A\"]\nmatch = \"luma\"\n[config]\ntenant = \"contoso\"\n[tenants]\ncontoso = \"t.png\"\n[[states]]\nname = \"A\"\ndetect = { screens = [\"a.png\", \"shared.png\"] }\nnext = [\"B\"]\n[[states]]\nname = \"B\"\ndetect = { screen = \"shared.png\" }\nterminal = true\n";
        let wf = kvmit_script::Workflow::parse(src).unwrap();
        let dir = std::env::temp_dir().join(format!("kvmit-refs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        let r = references(&wf, &dir);
        let names: Vec<(&str, bool)> = r.iter().map(|r| (r.image.as_str(), r.present)).collect();
        assert_eq!(names, [("a.png", true), ("shared.png", false), ("t.png", false)]);
        assert_eq!(r[1].used_by, ["state A", "state B"]);
        assert_eq!(r[2].used_by, ["tenant contoso"]);
        assert!(reference_path(&dir, "../x.png").is_err());
        assert!(reference_path(&dir, "./x.png").is_ok(), "as scripts have always allowed");
        assert!(reference_path(&dir, "a/../../x.png").is_err());
        assert!(reference_path(&dir, "").is_err());
        assert!(reference_path(&dir, if cfg!(windows) { "C:\\x.png" } else { "/x.png" }).is_err());
        assert_eq!(reference_path(&dir, "sub/x.png").unwrap(), dir.join("sub/x.png"));
        let saved = save_reference(&dir, "sub/new.png", 2, 1, &[0; 8]).unwrap();
        assert!(saved.is_file() && saved.ends_with("new.png"));
        assert!(save_reference(&dir, "../escape.png", 2, 1, &[0; 8]).is_err());
        assert!(save_reference(&dir, "screen.jpg", 2, 1, &[0; 8]).unwrap_err().contains("PNG"));
        let fresh = dir.join("not-yet");
        assert!(save_reference(&fresh, "first.png", 2, 1, &[0; 8]).is_ok(), "a scripts folder that does not exist yet is made");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_over_a_hard_linked_picture_replaces_it() {
        let root = std::env::temp_dir().join(format!("kvmit-hardlink-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (base, outside) = (root.join("wf"), root.join("outside"));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("shared.png"), b"keep").unwrap();
        std::fs::hard_link(outside.join("shared.png"), base.join("h.png")).unwrap();
        let saved = save_reference(&base, "h.png", 2, 1, &[0; 8]).unwrap();
        assert_eq!(std::fs::read(outside.join("shared.png")).unwrap(), b"keep", "the other name is untouched");
        assert_ne!(std::fs::read(saved).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&base).unwrap().count(), 1, "no temporary file left");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A folder inside the workflow's folder that links elsewhere is not followed when saving.
    #[cfg(unix)]
    #[test]
    fn saving_a_reference_never_follows_a_link_out_of_the_folder() {
        let root = std::env::temp_dir().join(format!("kvmit-link-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (base, outside) = (root.join("wf"), root.join("outside"));
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("refs")).unwrap();
        assert!(save_reference(&base, "refs/x.png", 2, 1, &[0; 8]).unwrap_err().contains("outside"));
        assert!(!outside.join("x.png").exists());
        std::os::unix::fs::symlink(outside.join("y.png"), base.join("y.png")).unwrap();
        assert!(save_reference(&base, "y.png", 2, 1, &[0; 8]).is_err(), "a linked destination is refused");
        // A folder below a link that points outside is never created out there.
        assert!(save_reference(&base, "refs/new/deeper/z.png", 2, 1, &[0; 8]).is_err());
        assert!(!outside.join("new").exists());
        // A picture hard-linked to a file outside is replaced, not written through.
        std::fs::write(outside.join("shared.png"), b"keep").unwrap();
        std::fs::hard_link(outside.join("shared.png"), base.join("h.png")).unwrap();
        save_reference(&base, "h.png", 2, 1, &[0; 8]).unwrap();
        assert_eq!(std::fs::read(outside.join("shared.png")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn export_names_are_safe_file_names() {
        assert_eq!(export_stem("Windows 11 OOBE (Autopilot)", "20261102-1530"), "windows-11-oobe-autopilot-20261102-1530");
        assert_eq!(export_stem("../../etc/passwd", "s"), "etc-passwd-s");
        assert_eq!(export_stem("", "s"), "workflow-s");
    }

    #[test]
    fn exports_never_overwrite_or_use_the_workflow_name() {
        let dir = std::env::temp_dir().join(format!("kvmit-export-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = save_exports(&dir, "20261102-1530", "{}", "# a").unwrap();
        assert_eq!(first, "workflow-run-20261102-1530");
        // Same second: a new name, the first pair untouched.
        let second = save_exports(&dir, "20261102-1530", "{}", "# b").unwrap();
        assert_eq!(second, "workflow-run-20261102-1530-1");
        assert_eq!(std::fs::read_to_string(dir.join(format!("{first}.notes.md"))).unwrap(), "# a");
        // A stray file holding one half of the next name: that name is skipped whole, nothing is overwritten.
        std::fs::write(dir.join("workflow-run-20261102-1530-2.notes.md"), "keep").unwrap();
        let third = save_exports(&dir, "20261102-1530", "{}", "# c").unwrap();
        assert_eq!(third, "workflow-run-20261102-1530-3");
        assert_eq!(std::fs::read_to_string(dir.join("workflow-run-20261102-1530-2.notes.md")).unwrap(), "keep");
        assert!(!dir.join("workflow-run-20261102-1530-2.trace.jsonl").exists(), "no half pair left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_a_stopped_live_run_of_this_very_workflow_can_be_resumed() {
        use kvmit_script::workflow::Context;
        use kvmit_script::{Workflow, WorkflowReport};
        let src = "workflow_format = 1\nname = \"w\"\nstart = [\"A\"]\nmatch = \"luma\"\n[[states]]\nname = \"A\"\ndetect = { screen = \"a.png\" }\nterminal = true\n";
        let wf = Workflow::parse(src).unwrap();
        let mut other = wf.clone();
        other.description = "edited".into();
        let stopped = WorkflowReport {
            result: Err(WorkflowError::Cancelled),
            trace: vec![],
            context: Context { current: Some("A".into()), ..Default::default() },
        };
        let done = WorkflowReport { result: Ok(()), ..stopped.clone() };
        let (a, b) = (&Source::File("a.toml".into()), &Source::File("b.toml".into()));
        assert!(resume_point(false, (&wf, a), (&wf, a), Some(&stopped)).is_some());
        assert!(resume_point(true, (&wf, a), (&wf, a), Some(&stopped)).is_none(), "a dry run never seeds a live resume");
        assert!(resume_point(false, (&wf, a), (&other, a), Some(&stopped)).is_none(), "another definition's run");
        assert!(resume_point(false, (&wf, a), (&wf, b), Some(&stopped)).is_none(), "same contents, another file");
        assert!(resume_point(false, (&wf, a), (&wf, a), Some(&done)).is_none(), "a completed run");
        assert!(resume_point(false, (&wf, a), (&wf, a), None).is_none());
    }

    /// Distinct non-Unicode file names display alike (both lossy); as sources they stay distinct.
    #[cfg(unix)]
    #[test]
    fn sources_compare_paths_exactly() {
        use std::os::unix::ffi::OsStrExt;
        let a = Source::File(std::ffi::OsStr::from_bytes(&[b'a', b'-', 0x80, b'.', b't', b'o', b'm', b'l']).into());
        let b = Source::File(std::ffi::OsStr::from_bytes(&[b'a', b'-', 0x81, b'.', b't', b'o', b'm', b'l']).into());
        if let (Source::File(pa), Source::File(pb)) = (&a, &b) {
            assert_eq!(pa.display().to_string(), pb.display().to_string(), "the display strings collide");
        }
        assert_ne!(a, b);
    }

    #[test]
    fn a_saved_checkpoint_resumes_only_the_same_file_text_and_adapter() {
        use kvmit_script::workflow::Context;
        use kvmit_script::{Workflow, WorkflowReport};
        let dir = std::env::temp_dir().join(format!("kvmit-cp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "workflow_format = 1\nname = \"w\"\nstart = [\"A\"]\nmatch = \"luma\"\n[[states]]\nname = \"A\"\ndetect = { screen = \"a.png\" }\nterminal = true\n";
        let wf = Workflow::parse(text).unwrap();
        let slot = Slot::new;
        let a = slot(Source::File(dir.join("a.toml")), "AA:01");
        let other_file = slot(Source::Builtin("[built-in] a.toml".into()), "AA:01");
        let other_adapter = slot(Source::File(dir.join("a.toml")), "BB:02");
        let stopped = WorkflowReport {
            result: Err(WorkflowError::Cancelled),
            trace: vec![],
            context: Context { current: Some("A".into()), ..Default::default() },
        };
        assert_eq!(keep_checkpoint(&dir, &a, text, true, &wf, &stopped, None), None, "a dry run saves nothing");
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None));
        assert!(keep_checkpoint(&dir, &a, text, false, &wf, &stopped, None).unwrap().starts_with("Saved a checkpoint at A"));
        assert_eq!(load_checkpoint(&dir, &a, text).unwrap().map(|c| c.state), Some("A".into()));
        assert_eq!(load_checkpoint(&dir, &other_file, text), Ok(None), "another workflow");
        assert_eq!(load_checkpoint(&dir, &other_adapter, text), Ok(None), "the same workflow on another target");
        assert_eq!(load_checkpoint(&dir, &a, &format!("{text}# edited\n")), Ok(None), "the file changed since");

        // A live run claims its slot before sending anything: the checkpoint is not resumable meanwhile, and a second
        // run of the same workflow on the same adapter is refused; the same workflow on another adapter is not.
        let claim = claim_checkpoint(&dir, &a).unwrap().unwrap();
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None));
        assert!(claim_checkpoint(&dir, &a).unwrap_err().contains("in progress"));
        let elsewhere = claim_checkpoint(&dir, &other_adapter).unwrap().unwrap();
        keep_checkpoint(&dir, &other_adapter, text, false, &wf, &stopped, Some(&elsewhere));
        // That run stopped before recognising a screen and sent nothing: the claimed checkpoint is put back, and the
        // slot is free again.
        let nothing = WorkflowReport { result: Err(WorkflowError::Cancelled), trace: vec![], context: Context::default() };
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &nothing, Some(&claim)), None);
        assert_eq!(load_checkpoint(&dir, &a, text).unwrap().map(|c| c.state), Some("A".into()));
        assert_eq!(load_checkpoint(&dir, &other_adapter, text).unwrap().map(|c| c.state), Some("A".into()), "kept apart");
        // A run that sent keys and then could not tell where it stopped: the claimed one is discarded.
        let claim = claim_checkpoint(&dir, &a).unwrap().unwrap();
        let sent = WorkflowReport {
            trace: vec![TraceEntry {
                at_ms: 0,
                event: WorkflowEvent::ActionAttempted { state: "A".into(), attempt: 1, steps: 2, dry_run: false },
            }],
            ..nothing.clone()
        };
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &sent, Some(&claim)), None);
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None), "never resumed past keys that run sent");
        let claim = claim_checkpoint(&dir, &a).unwrap().unwrap();
        let done = WorkflowReport { result: Ok(()), ..stopped.clone() };
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &done, Some(&claim)), None, "a completed run leaves none");
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None));
        assert!(claim_checkpoint(&dir, &a).is_ok(), "and releases the slot");
        // Ids differing only in case are one adapter: one checkpoint, one lock.
        let lower = slot(Source::File(dir.join("a.toml")), " aa:01 ");
        assert_eq!(lower, a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resume_starts_from_the_checkpoint_read_under_the_lock() {
        use kvmit_script::workflow::Context;
        use kvmit_script::{Workflow, WorkflowReport};
        let dir = std::env::temp_dir().join(format!("kvmit-cp-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "workflow_format = 1\nname = \"w\"\nstart = [\"A\"]\nmatch = \"luma\"\n[[states]]\nname = \"A\"\ndetect = { screen = \"a.png\" }\nnext = [\"B\"]\n[[states]]\nname = \"B\"\ndetect = { screen = \"b.png\" }\nterminal = true\n";
        let wf = Workflow::parse(text).unwrap();
        let a = Slot::new(Source::File(dir.join("a.toml")), "AA:01");
        let at = |state: &str| WorkflowReport {
            result: Err(WorkflowError::Cancelled),
            trace: vec![],
            context: Context { current: Some(state.into()), ..Default::default() },
        };
        keep_checkpoint(&dir, &a, text, false, &wf, &at("A"), None);
        let (shown, _) = load_checkpoint_raw(&dir, &a, text).unwrap().unwrap();
        // Unchanged: the resume starts from the copy taken under the lock.
        let claim = claim_checkpoint(&dir, &a).unwrap();
        let (claim, cp) = resume_from_claim(&dir, &a, text, claim, &shown).unwrap();
        assert_eq!(cp.state, "A");
        keep_checkpoint(&dir, &a, text, false, &wf, &at("A"), claim.as_ref());
        // Another run saved a newer one after it was shown: refused, the newer one put back, the slot free.
        let (shown, _) = load_checkpoint_raw(&dir, &a, text).unwrap().unwrap();
        keep_checkpoint(&dir, &a, text, false, &wf, &at("B"), None);
        let claim = claim_checkpoint(&dir, &a).unwrap();
        assert!(resume_from_claim(&dir, &a, text, claim, &shown).unwrap_err().contains("changed"));
        assert_eq!(load_checkpoint(&dir, &a, text).unwrap().map(|c| c.state), Some("B".into()));
        assert!(claim_checkpoint(&dir, &a).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_set_up_copy_is_written_new_and_rewritten_only_unchanged() {
        let dir = std::env::temp_dir().join(format!("kvmit-setup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let values: BTreeMap<String, String> = [("tenant".to_string(), "fabrikam".to_string())].into();
        assert_eq!(copy_file_name("w.toml", &values), "w-fabrikam.toml");
        assert_eq!(copy_file_name("w.toml", &BTreeMap::new()), "w-copy.toml");
        for bad in ["", ".toml", "x", "x.txt", "../x.toml", "a/b.toml", "a\\b.toml", ".hidden.toml", "c:x.toml", "x .toml", "con.toml", "NUL.x.toml", "com1.toml", "Lpt9.toml"] {
            assert!(check_file_name(bad).is_err(), "{bad:?}");
        }
        let p = write_new_workflow(&dir, "w-fabrikam.toml", "one").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one");
        assert!(write_new_workflow(&dir, "w-fabrikam.toml", "two").unwrap_err().contains("already exists"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one", "never replaced");
        assert!(rewrite_workflow(&p, "not what it holds", "two").unwrap_err().contains("changed since"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "one");
        rewrite_workflow(&p, "one", "two").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        // A save in progress (its lock) refuses a second one, and the lock is left for its owner.
        let lock = p.with_extension("toml.saving");
        std::fs::write(&lock, "").unwrap();
        assert!(rewrite_workflow(&p, "two", "three").unwrap_err().contains("in progress"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two");
        std::fs::remove_file(&lock).unwrap();
        assert!(check_file_name("console.toml").is_ok() && check_file_name("com10.toml").is_ok());
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "no temporary file left: {left:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_or_stuck_checkpoint_is_reported() {
        let dir = std::env::temp_dir().join(format!("kvmit-cp-stuck-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Slot::new(Source::File(dir.join("a.toml")), "AA:01");
        let path = checkpoint_path(&dir, &a).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_checkpoint(&dir, &a, "x").is_err(), "a corrupt checkpoint is reported, not ignored");
        // A checkpoint that cannot be set aside stops the run before it starts, and does not keep the slot locked.
        std::fs::create_dir_all(path.with_extension("json.claimed").join("in-the-way")).unwrap();
        assert!(claim_checkpoint(&dir, &a).unwrap_err().contains("cannot be set aside"));
        assert!(!path.with_extension("json.lock").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn declining_or_aborting_is_not_a_failure() {
        assert_eq!(outcome(&Ok(())), RunOutcome::Finished);
        assert!(matches!(outcome(&Err(WorkflowError::Cancelled)), RunOutcome::Aborted(_)));
        assert!(matches!(outcome(&Err(WorkflowError::GateDeclined { state: "A".into() })), RunOutcome::Aborted(_)));
        assert!(matches!(outcome(&Err(WorkflowError::NoFrameNumbers)), RunOutcome::Failed(_)));
    }
}
