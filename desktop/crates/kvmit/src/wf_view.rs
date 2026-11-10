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
                WorkflowEvent::PicturesMissing { states, stops } => (
                    format!(
                        "Not recognised this run (picture not saved): {}. If one appears, the run usually stops there (it may be taken for a similar screen).{}",
                        states.join(", "),
                        match stops.is_empty() {
                            true => String::new(),
                            false => format!(" This profile's stop at {} may not happen.", stops.join(", ")),
                        }
                    ),
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
                WorkflowEvent::TypedVerified { state, key } => (format!("{state}: {key} shows as typed"), Health::Good),
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
    // Windows drops a trailing dot or space from each name (`pics.` is `pics`): two spellings of one file.
    if name.split(['/', '\\']).any(|c| !c.is_empty() && c != "." && (c.ends_with('.') || c.ends_with(' '))) {
        return Err(format!("reference image {name:?}: a folder or file name may not end with a dot or a space"));
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

/// Whether this computer reads text off the screen (each operating system's own recogniser), probed once: the error
/// says why not. Workflows recognise screens with phrases by their text wherever it can be read.
pub fn text_reader() -> Result<(), String> {
    static READER: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    READER
        .get_or_init(|| {
            let blank = kvmit_script::Frame::new(64, 36, vec![255; 64 * 36]).expect("valid size");
            kvmit_video::text::read_text(&blank).map(|_| ())
        })
        .clone()
}

/// The states recognised by their text in a run here (they have phrases, and `reads_text`): their pictures are not used.
pub fn by_text(wf: &kvmit_script::Workflow, reads_text: bool) -> Vec<String> {
    wf.states.iter().filter(|s| reads_text && !s.detect.text.is_empty()).map(|s| s.name.clone()).collect()
}

/// Whether a run here uses this picture: a state that is not recognised by text shows it, or the tenant check compares
/// it (when it cannot read the page).
pub fn picture_needed(wf: &kvmit_script::Workflow, image: &str, reads_text: bool) -> bool {
    let text = by_text(wf, reads_text);
    wf.states.iter().any(|s| s.detect.screens.iter().any(|i| i == image) && !text.contains(&s.name))
        || (tenant_by_picture(wf, reads_text) && wf.tenants.values().any(|i| i == image))
}

/// The tenant check compares pictures unless it can read the page (text is read here and the profile names its
/// sign-in domain).
fn tenant_by_picture(wf: &kvmit_script::Workflow, reads_text: bool) -> bool {
    wf.states.iter().any(|s| s.verify_tenant) && !(reads_text && wf.config.contains_key(kvmit_script::workflow::TENANT_DOMAIN))
}

/// Every reference image of a workflow, in file order (states, then tenants), each listed once.
/// The states not recognisable in a run here: not by text, and none of their pictures present in `refs`.
pub fn blind_states(wf: &kvmit_script::Workflow, refs: &[Reference], reads_text: bool) -> Vec<String> {
    let present = |i: &String| refs.iter().any(|r| &r.image == i && r.present);
    let text = by_text(wf, reads_text);
    wf.states.iter().filter(|s| !text.contains(&s.name) && !s.detect.screens.iter().any(present)).map(|s| s.name.clone()).collect()
}

/// The tenant pictures missing while the tenant check compares pictures: a run will not start without them.
pub fn missing_tenant_pictures(wf: &kvmit_script::Workflow, refs: &[Reference], reads_text: bool) -> Vec<String> {
    if !tenant_by_picture(wf, reads_text) {
        return Vec::new();
    }
    wf.tenants.values().filter(|i| !refs.iter().any(|r| &r.image == *i && r.present)).cloned().collect()
}

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
/// Screen wording's save: `state`'s phrases set to `sets` in the workflow file at `path` (whose text was `was`), only
/// if the file still holds that text, and never with a phrase holding the value of a secret entered for this
/// workflow (a line read off the screen can be the sign-in typed): that is refused without repeating it.
pub fn save_wording(
    path: &std::path::Path,
    was: &str,
    state: &str,
    sets: &[Vec<String>],
    entered: &BTreeMap<String, String>,
) -> Result<(), String> {
    // First, before anything can repeat a phrase (a parse error quotes it): none may hold an entered secret.
    let holds_secret = sets.iter().flatten().any(|p| entered.values().any(|v| !v.is_empty() && p.contains(v.as_str())));
    if holds_secret {
        return Err("a phrase holds the value of a secret entered above (the sign-in or password)".into());
    }
    let text = kvmit_script::setup::set_phrases(was, state, sets)?;
    let new = kvmit_script::Workflow::parse(&text)?;
    let entered: kvmit_script::Vars = entered.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
    if kvmit_script::workflow::check_secrets(&new, &entered, None, "").is_err() {
        return Err("a phrase holds the value of a secret entered above (the sign-in or password)".into());
    }
    rewrite_workflow(path, was, &text)
}

/// The editor's wordings as the file will hold them: each phrase trimmed, empty phrases and wordings dropped, and
/// nothing split or joined (a phrase may itself hold `+`).
pub fn wording_sets(rows: &[Vec<String>]) -> Vec<Vec<String>> {
    rows.iter()
        .map(|r| r.iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect::<Vec<_>>())
        .filter(|s| !s.is_empty())
        .collect()
}

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
    // Renaming over a link would replace the link, not the file it points to.
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{} is a link: save the file it points to instead", path.display()));
    }
    let now = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if now != was {
        return Err(format!("{} changed since it was opened: reload it and set it up again", path.display()));
    }
    use std::io::Write;
    // The new file gets the old one's permissions before it takes its place (a private file stays private), and is
    // created owner-only meanwhile.
    let perms = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.permissions();
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = path.with_extension(format!("toml.{}-{nanos}.tmp", std::process::id()));
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    let written = open
        .open(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|_| f.sync_all()))
        .and_then(|_| std::fs::set_permissions(&tmp, perms))
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
    /// The laptop the stopped run was on, as the operator named it (empty: not named). A resume must name the same
    /// one: the adapter moves from laptop to laptop, and a checkpoint is only good for the laptop it was saved on.
    #[serde(default)]
    pub laptop: String,
    /// When it was saved (seconds since 1970), to say how long ago.
    #[serde(default)]
    pub saved_unix: Option<u64>,
    /// In a journal only: the run completed (it is finishing up), so there is nothing to resume.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ended: bool,
}

/// A laptop's name as the operator gives it (an asset tag, a serial): trimmed, at most 40 letters, digits, spaces and
/// `-_.#`. Empty is allowed (not named). It names run records, so it carries nothing else.
pub fn laptop_name(s: &str) -> Result<String, String> {
    let t = s.trim();
    if t.chars().count() > 40 || !t.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '#')) {
        return Err("a laptop's name is at most 40 letters, digits, spaces and - _ . #".into());
    }
    Ok(t.to_string())
}

/// Who a saved checkpoint is for and how old it is, from its file's text.
pub fn checkpoint_laptop(raw: &str) -> (String, Option<u64>) {
    serde_json::from_str::<SavedCheckpoint>(raw).map(|s| (s.laptop, s.saved_unix)).unwrap_or_default()
}

/// Why a saved checkpoint must not be resumed on `laptop` (the name entered now): it was saved for another one, or
/// one of the two is not named. Names compare without regard to case.
pub fn resume_refusal(raw: &str, laptop: &str) -> Option<String> {
    let (saved, _) = checkpoint_laptop(raw);
    if saved.eq_ignore_ascii_case(laptop.trim()) {
        return None;
    }
    Some(match (saved.is_empty(), laptop.trim().is_empty()) {
        (true, _) => "The saved run was on a laptop that was not named: resume it only by naming none, on that same laptop.".into(),
        (false, true) => format!("The saved run was on laptop {saved}: name it to resume there."),
        (false, false) => format!("The saved run was on laptop {saved}, not {}: run this one from the start.", laptop.trim()),
    })
}

/// "3 min ago", "2 h ago": how long since `unix`.
pub fn ago(unix: Option<u64>) -> String {
    let Some(t) = unix else { return "earlier".into() };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(t);
    match now.saturating_sub(t) {
        s if s < 90 => "just now".into(),
        s if s < 90 * 60 => format!("{} min ago", s / 60),
        s if s < 48 * 3600 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

/// Every live run leaves a record: its trace (JSONL) and notes (Markdown) in `runs/` beside the workflows, named by the
/// time and the laptop, with the laptop, the adapter and the workflow at the top of the notes. No secrets: the trace
/// never holds them. Returns where, or why not.
pub fn save_run_record(
    dir: &std::path::Path,
    laptop: &str,
    adapter: &str,
    wf: &kvmit_script::Workflow,
    report: &kvmit_script::WorkflowReport,
) -> Result<String, String> {
    use kvmit_script::WorkflowError;
    let jsonl = kvmit_script::workflow::trace_jsonl(report);
    // A run refused because the file, or the laptop's name, holds a secret's value repeats neither, anywhere.
    // The file is checked first, so a refusal over the file says nothing about the laptop's name: withheld too.
    let (file_withheld, name_withheld) = match &report.result {
        Err(WorkflowError::SecretInFile { .. }) => (true, true),
        Err(WorkflowError::SecretInName) => (false, true),
        _ => (false, false),
    };
    let laptop = if name_withheld { "" } else { laptop };
    let named = match (name_withheld, laptop.is_empty()) {
        (true, _) => "(name withheld: the run was refused over a secret's value)",
        (false, true) => "(not named)",
        (false, false) => laptop,
    };
    let md = format!(
        "Laptop: {named}  \nAdapter: {adapter}  \nWorkflow: {}  \nResult: {}\n\n{}",
        match (file_withheld, wf.name.is_empty()) {
            (true, _) => "(name withheld: the file contains a secret's value)",
            (false, true) => "(unnamed)",
            (false, false) => wf.name.as_str(),
        },
        match &report.result {
            Ok(()) => "completed".to_string(),
            Err(e) => format!("stopped: {e}"),
        },
        kvmit_script::workflow::trace_markdown(wf, report)
    );
    let tag: String = laptop.chars().map(|c| if c.is_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let stamp = match tag.is_empty() {
        true => kvmit_record::naming::stamp_now(),
        false => format!("{}-{tag}", kvmit_record::naming::stamp_now()),
    };
    let runs = dir.join("runs");
    save_exports(&runs, &stamp, &jsonl, &md).map(|stem| format!("Run record saved: {} (.trace.jsonl, .notes.md)", runs.join(stem).display())).map_err(|e| format!("Could not save the run record: {e}"))
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
    laptop: &str,
) -> std::io::Result<std::path::PathBuf> {
    let unnamed = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "the workflow's file name is not Unicode");
    let path = checkpoint_path(dir, slot).ok_or_else(unnamed)?;
    write_saved(&path, slot, text, checkpoint, laptop, false)
}

/// Write a saved checkpoint (or a run's journal) to `path`: a temporary file renamed over it.
fn write_saved(
    path: &std::path::Path,
    slot: &Slot,
    text: &str,
    checkpoint: &kvmit_script::Checkpoint,
    laptop: &str,
    ended: bool,
) -> std::io::Result<std::path::PathBuf> {
    let unnamed = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "the workflow's file name is not Unicode");
    let path = path.to_path_buf();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).ok();
    let saved = SavedCheckpoint {
        slot: slot.clone(),
        definition: fnv(text.as_bytes()),
        checkpoint: checkpoint.clone(),
        laptop: laptop.to_string(),
        saved_unix: now,
        ended,
    };
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
        .and_then(|_| durable_rename(&tmp, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map(|_| path)
}

/// Rename `from` over `to` so that the rename itself survives a power cut or a crash of the computer once this
/// returns: Windows writes it through (`MOVEFILE_WRITE_THROUGH`), elsewhere the folder is synced.
fn durable_rename(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};
        let wide = |p: &std::path::Path| p.as_os_str().encode_wide().chain([0]).collect::<Vec<u16>>();
        let (f, t) = (wide(from), wide(to));
        // SAFETY: both are NUL-terminated UTF-16 strings that live across the call.
        unsafe {
            MoveFileExW(windows::core::PCWSTR(f.as_ptr()), windows::core::PCWSTR(t.as_ptr()), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH)
        }
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(from, to)?;
        sync_dir(to)
    }
}

/// Make a change to the folder holding `p` (a file made, renamed or removed) survive a crash of the computer.
fn sync_dir(p: &std::path::Path) -> std::io::Result<()> {
    let dir = p.parent().unwrap_or(std::path::Path::new("."));
    #[cfg(windows)]
    {
        // A folder is flushed through a handle opened for writing with backup semantics.
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new().write(true).custom_flags(FILE_FLAG_BACKUP_SEMANTICS).open(dir)?.sync_all()
    }
    #[cfg(not(windows))]
    {
        std::fs::File::open(dir)?.sync_all()
    }
}

/// Where a live run journals its checkpoint as it goes: if the run never ends cleanly (the app or the computer
/// stopped), this says where it got to. At the end of a run it holds the final state first, so a crash while finishing
/// leaves one record that says what is true.
pub fn journal_path(dir: &std::path::Path, slot: &Slot) -> Option<std::path::PathBuf> {
    checkpoint_path(dir, slot).map(|p| p.with_extension("json.journal"))
}

/// A live run's journal, for its host: every record the engine makes (a state entered, a wait begun, keys about to
/// be sent) is written durably before the engine goes on, and a failed write stops the run before the keys.
pub fn journal_writer(dir: std::path::PathBuf, slot: Slot, text: String, laptop: String) -> crate::host::JournalFn {
    Box::new(move |cp| {
        // A run whose file cannot be named is not saved for resuming at all: nothing to journal either.
        let Some(path) = journal_path(&dir, &slot) else { return Ok(()) };
        write_saved(&path, &slot, &text, cp, &laptop, false).map(|_| ()).map_err(|e| format!("{}: {e}", path.display()))
    })
}

const LOCK_MARK: &[u8] = b"kvm-it workflow run lock\n";
const IN_PROGRESS: &str = "another run of this workflow on this adapter is in progress (in this or another window, or a terminal)";

/// The slot's lock, held by this process until the returned file is dropped (the operating system releases it if
/// the process dies, so a lock never outlives its run). The file itself stays. `legacy`: an empty lock file left by an
/// earlier kvm-it's run that did not end cleanly. `create`: make the file if there is none (a run); else `Ok(None)`.
fn take_lock(lock: &std::path::Path, create: bool) -> Result<Option<(std::fs::File, bool)>, String> {
    use std::io::Write;
    let cannot = |e: std::io::Error| format!("cannot lock this workflow's checkpoint ({}): {e}", lock.display());
    if create && !lock.exists() {
        // Written whole under a name of this process's own, then linked into place (which fails if another got there
        // first): no one ever sees this version's lock file empty, which is what 0.5 left behind.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let tmp = lock.with_extension(format!("lock.{}-{nanos}.tmp", std::process::id()));
        let made = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .and_then(|mut f| f.write_all(LOCK_MARK).and_then(|_| f.sync_all()))
            .and_then(|_| match std::fs::hard_link(&tmp, lock) {
                Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => Err(e),
                _ => Ok(()),
            });
        let _ = std::fs::remove_file(&tmp);
        made.and_then(|_| sync_dir(lock)).map_err(cannot)?;
    }
    let file = match std::fs::OpenOptions::new().read(true).write(true).open(lock) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
        Err(e) => return Err(cannot(e)),
    };
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(IN_PROGRESS.into()),
        Err(std::fs::TryLockError::Error(e)) => return Err(cannot(e)),
    }
    let legacy = file.metadata().is_ok_and(|m| m.len() == 0);
    Ok(Some((file, legacy)))
}

/// The files of a slot's runs, beside its checkpoint.
struct RunFiles {
    path: std::path::PathBuf,
    lock: std::path::PathBuf,
    /// There while a run is going on, or after one that did not end cleanly. Made and removed only under the lock.
    running: std::path::PathBuf,
    held: std::path::PathBuf,
    journal: std::path::PathBuf,
}

fn run_files(dir: &std::path::Path, slot: &Slot) -> Option<RunFiles> {
    let path = checkpoint_path(dir, slot)?;
    Some(RunFiles {
        lock: path.with_extension("json.lock"),
        running: path.with_extension("json.running"),
        held: path.with_extension("json.claimed"),
        journal: path.with_extension("json.journal"),
        path,
    })
}

/// Remove `p` if it is there, durably (see [`sync_dir`]).
fn remove_if_there(p: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", p.display())),
        Ok(()) => sync_dir(p).map_err(|e| format!("{}: {e}", p.display())),
    }
}

/// A run of this workflow on this adapter that did not end cleanly, and none is going on now (its lock is free). With
/// its journal, the state it reached and when; `Some((None, _))` without one (it stopped before recording anything).
pub fn interrupted(dir: &std::path::Path, slot: &Slot, text: &str) -> Option<(Option<String>, Option<u64>)> {
    let f = run_files(dir, slot)?;
    let (_lock, legacy) = take_lock(&f.lock, false).ok().flatten()?;
    if !legacy && !f.running.exists() {
        return None;
    }
    let journal = std::fs::read_to_string(&f.journal).ok();
    Some(match journal.as_deref().and_then(|raw| read_journal(raw, slot, text).map(|s| (s, raw))) {
        Some((state, raw)) => (Some(state), checkpoint_laptop(raw).1),
        None => (None, None),
    })
}

fn read_journal(raw: &str, slot: &Slot, text: &str) -> Option<String> {
    let saved: SavedCheckpoint = serde_json::from_str(raw).ok()?;
    (saved.slot == *slot && saved.definition == fnv(text.as_bytes())).then_some(saved.checkpoint.state)
}

/// Recover a run that did not end cleanly. Refused while a run holds the slot's lock (it is going on); under the
/// lock, so no run starts meanwhile. The journal is the authority: its last record becomes the saved checkpoint
/// (Resume then looks at the screen first, and asks before sending again what may have been sent), or, if the run
/// had completed, nothing is left to resume. Without a journal the run sent nothing (every burst of keys is journalled
/// first), so the checkpoint it had set aside is put back.
pub fn recover(dir: &std::path::Path, slot: &Slot) -> Result<String, String> {
    let f = run_files(dir, slot).ok_or("no checkpoint name")?;
    let none = "no run of this workflow on this adapter was interrupted";
    let (lock, legacy) = take_lock(&f.lock, false)
        .map_err(|e| if e == IN_PROGRESS { format!("{e}: not recovering while it runs") } else { e })?
        .ok_or(none)?;
    if !legacy && !f.running.exists() {
        return Err(none.into());
    }
    let journal = match std::fs::read_to_string(&f.journal) {
        Ok(raw) => Some(raw),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", f.journal.display())),
    };
    let msg = match journal {
        Some(raw) if serde_json::from_str::<SavedCheckpoint>(&raw).is_ok_and(|s| s.ended) => {
            remove_if_there(&f.held)?;
            remove_if_there(&f.journal)?;
            "Recovered: the interrupted run had completed, so there is nothing to resume."
        }
        Some(_) => {
            remove_if_there(&f.held)?;
            durable_rename(&f.journal, &f.path).map_err(|e| format!("{}: {e}", f.journal.display()))?;
            "Recovered: the interrupted run's last step is now its checkpoint. Check the laptop, then Resume (it looks at the screen first, and asks before sending anything again)."
        }
        None if f.held.exists() && f.path.exists() => {
            // The newer one is kept.
            remove_if_there(&f.held)?;
            "Recovered: the interrupted run recorded nothing more; its checkpoint is kept."
        }
        None if f.held.exists() => {
            durable_rename(&f.held, &f.path).map_err(|e| format!("{}: {e}", f.held.display()))?;
            if legacy {
                "Recovered: the checkpoint from before the interrupted run is back. That run was by an earlier kvm-it, which recorded nothing as it went: it may have sent keys, so check the laptop before you Resume."
            } else {
                "Recovered: the interrupted run recorded nothing (it sent nothing), so the checkpoint from before it is back."
            }
        }
        // The run had saved its checkpoint and stopped while finishing: that checkpoint stands.
        None if f.path.exists() => "Recovered: the interrupted run had saved its checkpoint, which is kept.",
        None => "Recovered: the interrupted run recorded nothing, and there was no earlier checkpoint.",
    };
    remove_if_there(&f.running)?;
    if legacy {
        use std::io::Write;
        (&lock).write_all(LOCK_MARK).map_err(|e| format!("{}: {e}", f.lock.display()))?;
    }
    drop(lock);
    Ok(msg.into())
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

/// A live run's hold on its slot, from before it sends anything until it is dropped (after [`keep_checkpoint`]): the
/// slot's lock, held by the operating system for as long as this process has it, the run's marker, and the checkpoint
/// it set aside.
#[derive(Debug)]
pub struct Claim {
    _lock: std::fs::File,
    running: std::path::PathBuf,
    /// The saved checkpoint, renamed so it is never resumed while this run goes on.
    held: Option<std::path::PathBuf>,
}

/// Before a live run sends anything: take the slot's lock (a second run of this workflow on this adapter, in any window
/// or process, is refused), refuse if a run before it did not end cleanly (Recover it first), mark this run as going
/// on, and set its saved checkpoint aside (renamed to `.claimed`, never resumed). From then on no checkpoint older
/// than this run can be resumed. If any step fails, the run must not start.
pub fn claim_checkpoint(dir: &std::path::Path, slot: &Slot) -> Result<Option<Claim>, String> {
    let Some(f) = run_files(dir, slot) else { return Ok(None) };
    std::fs::create_dir_all(f.path.parent().expect("under checkpoints/")).map_err(|e| e.to_string())?;
    let (lock, legacy) = take_lock(&f.lock, true)?.expect("created if missing");
    if legacy || f.running.exists() {
        return Err(format!(
            "a run of this workflow on this adapter did not end cleanly (the app or the computer stopped): Recover it \
             (in the app, or `kvmit run <file> --recover`), then check the laptop ({})",
            f.running.display()
        ));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&f.running)
        .and_then(|r| r.sync_all())
        .and_then(|_| sync_dir(&f.running))
        .map_err(|e| format!("cannot mark this workflow's run as going on ({}): {e}", f.running.display()))?;
    let claim = |held| Claim { _lock: lock, running: f.running.clone(), held };
    match durable_rename(&f.path, &f.held) {
        Ok(()) => Ok(Some(claim(Some(f.held.clone())))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(claim(None))),
        Err(e) => {
            let _ = std::fs::remove_file(&f.running);
            Err(format!(
                "the workflow's saved checkpoint {} cannot be set aside ({e}); fix that (or delete it) before running",
                f.path.display()
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
    if let Err(e) = std::fs::remove_file(&claim.running) {
        failed.push(format!("{} could not be removed ({e}): Recover the run before running again", claim.running.display()));
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
/// recognising a screen), it is put back, so resuming is still possible. Then the run's marker is removed (the lock is
/// released when the claim is dropped). A dry run changes nothing. Returns what to tell the user.
///
/// In an order a crash cannot get wrong: the final state goes into the journal first (a completed run's marked as
/// such), and from then on it is what [`recover`] goes by; the set-aside checkpoint is removed before the journal
/// becomes the checkpoint; the marker goes last. If a step fails, the run is left as interrupted, to Recover.
#[allow(clippy::too_many_arguments)]
pub fn keep_checkpoint(
    dir: &std::path::Path,
    slot: &Slot,
    text: &str,
    dry: bool,
    wf: &kvmit_script::Workflow,
    report: &kvmit_script::WorkflowReport,
    claim: Option<&Claim>,
    laptop: &str,
) -> Option<String> {
    if dry {
        return None;
    }
    let f = run_files(dir, slot)?;
    let held = claim.and_then(|c| c.held.as_ref());
    let left = |why: String| match claim {
        Some(_) => format!("{why}: the run is left as interrupted. Once that is fixed, Recover it (it keeps what the run recorded)."),
        None => format!("{why}, so this run cannot be resumed: run it again from the start."),
    };
    let finished = (|| -> Result<Option<String>, String> {
        let cp = report.checkpoint(wf);
        match (&report.result, cp) {
            (Ok(()), cp) => {
                if let Some(cp) = cp {
                    write_saved(&f.journal, slot, text, &cp, laptop, true).map_err(|e| format!("Could not record the end of the run ({e})"))?;
                }
                if let Some(h) = held {
                    remove_if_there(h)?;
                }
                remove_if_there(&f.journal)?;
                Ok(None)
            }
            (Err(_), Some(cp)) => {
                write_saved(&f.journal, slot, text, &cp, laptop, false).map_err(|e| format!("Could not save a checkpoint ({e})"))?;
                if let Some(h) = held {
                    remove_if_there(h)?;
                }
                durable_rename(&f.journal, &f.path).map_err(|e| format!("Could not save a checkpoint ({}: {e})", f.path.display()))?;
                Ok(Some(format!("Saved a checkpoint at {}: Resume continues from there after looking at the screen.", cp.state)))
            }
            // Stopped before recognising any screen: if nothing was sent, the earlier checkpoint is back; keys sent with
            // no state to resume from discard it (it is never resumed past them).
            (Err(_), None) => {
                match held {
                    Some(h) if !sent_keys(report) => durable_rename(h, &f.path)
                        .map_err(|e| format!("The earlier checkpoint {} could not be put back ({e})", h.display()))?,
                    Some(h) => remove_if_there(h)?,
                    None => {}
                }
                remove_if_there(&f.journal)?;
                Ok(None)
            }
        }
    })();
    match finished {
        Ok(msg) => match claim.map(|c| remove_if_there(&c.running)) {
            Some(Err(e)) => Some(left(format!("Could not finish the run's record ({e})"))),
            _ => msg,
        },
        Err(e) => Some(left(e)),
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
        assert_eq!(keep_checkpoint(&dir, &a, text, true, &wf, &stopped, None, ""), None, "a dry run saves nothing");
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None));
        assert!(keep_checkpoint(&dir, &a, text, false, &wf, &stopped, None, "").unwrap().starts_with("Saved a checkpoint at A"));
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
        keep_checkpoint(&dir, &other_adapter, text, false, &wf, &stopped, Some(&elsewhere), "");
        // That run stopped before recognising a screen and sent nothing: the claimed checkpoint is put back, and the
        // slot is free again.
        let nothing = WorkflowReport { result: Err(WorkflowError::Cancelled), trace: vec![], context: Context::default() };
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &nothing, Some(&claim), ""), None);
        drop((claim, elsewhere));
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
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &sent, Some(&claim), ""), None);
        drop(claim);
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None), "never resumed past keys that run sent");
        let claim = claim_checkpoint(&dir, &a).unwrap().unwrap();
        let done = WorkflowReport { result: Ok(()), ..stopped.clone() };
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &done, Some(&claim), ""), None, "a completed run leaves none");
        drop(claim);
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
        keep_checkpoint(&dir, &a, text, false, &wf, &at("A"), None, "");
        let (shown, _) = load_checkpoint_raw(&dir, &a, text).unwrap().unwrap();
        // Unchanged: the resume starts from the copy taken under the lock.
        let claim = claim_checkpoint(&dir, &a).unwrap();
        let (claim, cp) = resume_from_claim(&dir, &a, text, claim, &shown).unwrap();
        assert_eq!(cp.state, "A");
        keep_checkpoint(&dir, &a, text, false, &wf, &at("A"), claim.as_ref(), "");
        drop(claim);
        // Another run saved a newer one after it was shown: refused, the newer one put back, the slot free.
        let (shown, _) = load_checkpoint_raw(&dir, &a, text).unwrap().unwrap();
        keep_checkpoint(&dir, &a, text, false, &wf, &at("B"), None, "");
        let claim = claim_checkpoint(&dir, &a).unwrap();
        assert!(resume_from_claim(&dir, &a, text, claim, &shown).unwrap_err().contains("changed"));
        assert_eq!(load_checkpoint(&dir, &a, text).unwrap().map(|c| c.state), Some("B".into()));
        assert!(claim_checkpoint(&dir, &a).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_checkpoint_resumes_only_on_the_laptop_it_names_and_every_live_run_leaves_a_record() {
        assert_eq!(laptop_name("  LT-0042 ").unwrap(), "LT-0042");
        assert_eq!(laptop_name("").unwrap(), "");
        for bad in ["a/b", "x\ny", "jane@contoso.com", &"x".repeat(41)] {
            assert!(laptop_name(bad).is_err(), "{bad:?}");
        }
        let dir = std::env::temp_dir().join(format!("kvmit-laptop-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "workflow_format = 1\nname = \"w\"\nstart = [\"A\"]\n[secrets]\npw = { prompt = \"Password\" }\n[[states]]\nname = \"A\"\ndetect = { text = [\"Name your device\"] }\nnext = [\"B\"]\nactions = [ { secret_text = \"{{pw}}\" } ]\n[[states]]\nname = \"B\"\ndetect = { text = [\"Done here\"] }\nterminal = true\n";
        let wf = kvmit_script::Workflow::parse(text).unwrap();
        let slot = Slot::new(Source::Builtin("w.toml".into()), "aa:bb");
        let cp = kvmit_script::Checkpoint {
            workflow: "w".into(),
            state: "A".into(),
            derived: BTreeMap::new(),
            attempts: BTreeMap::new(),
            attempted: Some(1),
            waiting: false,
            history: Vec::new(),
        };
        save_checkpoint(&dir, &slot, text, &cp, "LT-0042").unwrap();
        let (raw, _) = load_checkpoint_raw(&dir, &slot, text).unwrap().unwrap();
        assert_eq!(checkpoint_laptop(&raw).0, "LT-0042");
        assert!(checkpoint_laptop(&raw).1.is_some());
        assert_eq!(resume_refusal(&raw, "lt-0042"), None, "names compare without case");
        assert!(resume_refusal(&raw, "LT-0043").unwrap().contains("run this one from the start"));
        assert!(resume_refusal(&raw, "").unwrap().contains("name it to resume"));
        save_checkpoint(&dir, &slot, text, &cp, "").unwrap();
        let (raw, _) = load_checkpoint_raw(&dir, &slot, text).unwrap().unwrap();
        assert_eq!(resume_refusal(&raw, ""), None);
        assert!(resume_refusal(&raw, "LT-1").is_some(), "an unnamed checkpoint is not claimed by a named laptop");
        // The record: under runs/, named by the laptop, its notes headed by laptop, adapter and result; no secret.
        let report = kvmit_script::WorkflowReport {
            result: Err(kvmit_script::WorkflowError::UnknownScreen { best: None }),
            trace: Vec::new(),
            context: Default::default(),
        };
        let msg = save_run_record(&dir, "LT 0042", "AA:BB", &wf, &report).unwrap();
        assert!(msg.contains("runs") && msg.contains("LT_0042"), "{msg}");
        let notes = std::fs::read_dir(dir.join("runs")).unwrap().map(|e| e.unwrap().path()).find(|p| p.to_string_lossy().ends_with(".notes.md")).unwrap();
        let md = std::fs::read_to_string(notes).unwrap();
        assert!(md.starts_with("Laptop: LT 0042  \nAdapter: AA:BB  \nWorkflow: w  \nResult: stopped:"), "{md}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn journal_fixture(tag: &str) -> (std::path::PathBuf, &'static str, kvmit_script::Workflow, Slot) {
        let dir = std::env::temp_dir().join(format!("kvmit-journal-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "workflow_format = 1\nname = \"w\"\nstart = [\"A\"]\n[[states]]\nname = \"A\"\ndetect = { text = [\"Name your device\"] }\nnext = [\"B\"]\nactions = [ { key = \"ENTER\" } ]\n[[states]]\nname = \"B\"\ndetect = { text = [\"Done here\"] }\nterminal = true\n";
        let wf = kvmit_script::Workflow::parse(text).unwrap();
        (dir, text, wf, Slot::new(Source::Builtin("w.toml".into()), "aa:bb"))
    }

    fn cp_at(state: &str, attempted: u32) -> kvmit_script::Checkpoint {
        kvmit_script::Checkpoint {
            workflow: "w".into(),
            state: state.into(),
            derived: BTreeMap::new(),
            attempts: BTreeMap::new(),
            attempted: Some(attempted),
            waiting: false,
            history: Vec::new(),
        }
    }

    #[test]
    fn a_run_that_never_ends_cleanly_is_recovered_from_its_journal() {
        let (dir, text, wf, slot) = journal_fixture("crash");
        // A live run claims the slot: while it holds it, it is not "interrupted", and Recover is refused.
        let claim = claim_checkpoint(&dir, &slot).unwrap().unwrap();
        assert_eq!(interrupted(&dir, &slot, text), None, "a run going on is not interrupted");
        assert!(recover(&dir, &slot).unwrap_err().contains("in progress"), "never pulled from under a live run");
        assert!(claim_checkpoint(&dir, &slot).unwrap_err().contains("in progress"));
        let lock = checkpoint_path(&dir, &slot).unwrap().with_extension("json.lock");
        // Its size: the content cannot be read past the lock on Windows.
        assert_eq!(std::fs::metadata(&lock).unwrap().len(), LOCK_MARK.len() as u64, "never left empty (that would read as 0.5's crash)");
        // It records keys about to be sent in A, then the app dies (the claim is dropped without finishing).
        let mut write = journal_writer(dir.clone(), slot.clone(), text.into(), "LT-7".into());
        write(&cp_at("A", 1)).unwrap();
        drop(claim);
        let (state, at) = interrupted(&dir, &slot, text).unwrap();
        assert_eq!(state.as_deref(), Some("A"));
        assert!(at.is_some());
        assert!(claim_checkpoint(&dir, &slot).unwrap_err().contains("Recover it"));
        // Recovered: the journal is the checkpoint (keys were sent: a resume asks before sending again).
        assert!(recover(&dir, &slot).unwrap().starts_with("Recovered: the interrupted run's last step"));
        assert_eq!(interrupted(&dir, &slot, text), None);
        assert!(recover(&dir, &slot).unwrap_err().contains("was interrupted"), "nothing left to recover");
        let (raw, cp) = load_checkpoint_raw(&dir, &slot, text).unwrap().unwrap();
        assert_eq!((cp.state.as_str(), cp.attempted), ("A", Some(1)));
        assert_eq!(checkpoint_laptop(&raw).0, "LT-7");
        // A run that ends cleanly leaves no journal and no marker behind.
        let claim = claim_checkpoint(&dir, &slot).unwrap();
        write(&cp_at("B", 0)).unwrap();
        let report = kvmit_script::WorkflowReport {
            result: Ok(()),
            trace: Vec::new(),
            context: kvmit_script::workflow::Context { current: Some("B".into()), ..Default::default() },
        };
        assert_eq!(keep_checkpoint(&dir, &slot, text, false, &wf, &report, claim.as_ref(), ""), None);
        drop(claim);
        assert!(!journal_path(&dir, &slot).unwrap().exists());
        assert_eq!(interrupted(&dir, &slot, text), None);
        assert_eq!(load_checkpoint(&dir, &slot, text), Ok(None), "a completed run leaves nothing to resume");
        assert!(claim_checkpoint(&dir, &slot).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_while_a_run_finishes_recovers_what_the_run_ended_with() {
        let (dir, text, _wf, slot) = journal_fixture("finish");
        save_checkpoint(&dir, &slot, text, &cp_at("A", 0), "").unwrap();
        // A completed run wrote its end into the journal, then died before tidying up: nothing to resume, and the
        // checkpoint it had set aside does not come back.
        let claim = claim_checkpoint(&dir, &slot).unwrap().unwrap();
        write_saved(&journal_path(&dir, &slot).unwrap(), &slot, text, &cp_at("B", 0), "", true).unwrap();
        drop(claim);
        assert!(recover(&dir, &slot).unwrap().contains("had completed"));
        assert_eq!(load_checkpoint(&dir, &slot, text), Ok(None));
        // A stopped run saved its final checkpoint, then died before removing its marker: the older one set aside
        // never replaces it.
        save_checkpoint(&dir, &slot, text, &cp_at("A", 0), "").unwrap();
        let claim = claim_checkpoint(&dir, &slot).unwrap().unwrap();
        save_checkpoint(&dir, &slot, text, &cp_at("B", 1), "").unwrap();
        let held = checkpoint_path(&dir, &slot).unwrap().with_extension("json.claimed");
        assert!(held.exists());
        drop(claim);
        assert!(recover(&dir, &slot).unwrap().contains("checkpoint is kept"));
        assert_eq!(load_checkpoint(&dir, &slot, text).unwrap().map(|c| c.state), Some("B".into()));
        assert!(!held.exists());
        // A stopped run finished for real, then died before removing its marker: its own checkpoint stands, and Recover
        // says so.
        let claim = claim_checkpoint(&dir, &slot).unwrap();
        let stopped = kvmit_script::WorkflowReport {
            result: Err(WorkflowError::Cancelled),
            trace: Vec::new(),
            context: kvmit_script::workflow::Context { current: Some("A".into()), visit_attempts: 1, ..Default::default() },
        };
        assert!(keep_checkpoint(&dir, &slot, text, false, &_wf, &stopped, claim.as_ref(), "").unwrap().starts_with("Saved"));
        let running = checkpoint_path(&dir, &slot).unwrap().with_extension("json.running");
        std::fs::write(&running, "").unwrap();
        drop(claim);
        assert!(recover(&dir, &slot).unwrap().contains("had saved its checkpoint"));
        assert_eq!(load_checkpoint(&dir, &slot, text).unwrap().map(|c| (c.state, c.attempted)), Some(("A".into(), Some(1))));
        // A run that recorded nothing sent nothing: the checkpoint from before it is back.
        let claim = claim_checkpoint(&dir, &slot).unwrap().unwrap();
        drop(claim);
        assert!(recover(&dir, &slot).unwrap().contains("sent nothing"));
        assert_eq!(load_checkpoint(&dir, &slot, text).unwrap().map(|c| c.state), Some("A".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_left_by_an_earlier_kvm_it_is_recovered_with_a_warning() {
        let (dir, text, _wf, slot) = journal_fixture("legacy");
        save_checkpoint(&dir, &slot, text, &cp_at("A", 0), "").unwrap();
        let path = checkpoint_path(&dir, &slot).unwrap();
        // 0.5's run: an empty lock file, the checkpoint set aside, then a crash.
        std::fs::rename(&path, path.with_extension("json.claimed")).unwrap();
        std::fs::write(path.with_extension("json.lock"), "").unwrap();
        assert!(interrupted(&dir, &slot, text).is_some());
        assert!(claim_checkpoint(&dir, &slot).unwrap_err().contains("Recover it"));
        assert!(recover(&dir, &slot).unwrap().contains("may have sent keys"));
        assert_eq!(load_checkpoint(&dir, &slot, text).unwrap().map(|c| c.state), Some("A".into()));
        assert!(claim_checkpoint(&dir, &slot).is_ok(), "the lock file is this version's now");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_record_repeats_no_name_a_refusal_says_holds_a_secret() {
        let dir = std::env::temp_dir().join(format!("kvmit-record-withheld-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "workflow_format = 1\nname = \"hunter2 setup\"\nstart = [\"A\"]\n[secrets]\npw = { prompt = \"Password\" }\n[[states]]\nname = \"A\"\ndetect = { text = [\"Name your device\"] }\nterminal = true\n";
        let wf = kvmit_script::Workflow::parse(text).unwrap();
        let secrets: kvmit_script::Vars = [("pw".to_string(), "hunter2".to_string())].into();
        // Refused for real by the engine, as the app runs it: the file's name holds the password.
        struct Idle;
        impl kvmit_script::Host for Idle {
            fn key_down(&mut self, _: kvmit_hid::Key) -> Result<(), String> { Err("no keys in this test".into()) }
            fn key_up(&mut self, _: kvmit_hid::Key) -> Result<(), String> { Err("no keys in this test".into()) }
            fn release_all(&mut self) -> Result<(), String> { Ok(()) }
            fn mouse_move(&mut self, _: i32, _: i32) -> Result<(), String> { Ok(()) }
            fn mouse_button(&mut self, _: u8, _: bool) -> Result<(), String> { Ok(()) }
            fn sleep(&mut self, _: std::time::Duration) {}
            fn screen(&mut self) -> Option<kvmit_script::Frame> { None }
            fn reference(&mut self, _: &str) -> Result<kvmit_script::Frame, String> { Err("none".into()) }
            fn confirm(&mut self, _: &str) -> bool { false }
            fn cancelled(&self) -> bool { false }
            fn event(&mut self, _: kvmit_script::RunEvent) {}
        }
        let mut host = Idle;
        let report = kvmit_script::run_workflow(&wf, &secrets, &kvmit_layout::UsAnsi, &mut host, &Default::default(), None, &mut |_, _| {});
        assert!(matches!(report.result, Err(WorkflowError::SecretInFile { .. })), "{:?}", report.result);
        save_run_record(&dir, "LT-1", "AA:BB", &wf, &report).unwrap();
        // Both at once: the file is checked first, and the laptop's name (holding it too) is still withheld.
        let opts = kvmit_script::WorkflowOptions { laptop: "LT hunter2".into(), ..Default::default() };
        let report = kvmit_script::run_workflow(&wf, &secrets, &kvmit_layout::UsAnsi, &mut host, &opts, None, &mut |_, _| {});
        assert!(matches!(report.result, Err(WorkflowError::SecretInFile { .. })), "{:?}", report.result);
        save_run_record(&dir, "LT hunter2", "AA:BB", &wf, &report).unwrap();
        // The laptop's name holds it.
        let plain = kvmit_script::Workflow::parse(&text.replace("hunter2 setup", "setup")).unwrap();
        let opts = kvmit_script::WorkflowOptions { laptop: "LT hunter2".into(), ..Default::default() };
        let report = kvmit_script::run_workflow(&plain, &secrets, &kvmit_layout::UsAnsi, &mut host, &opts, None, &mut |_, _| {});
        assert_eq!(report.result, Err(WorkflowError::SecretInName));
        save_run_record(&dir, "LT hunter2", "AA:BB", &plain, &report).unwrap();
        for e in std::fs::read_dir(dir.join("runs")).unwrap() {
            let p = e.unwrap().path();
            assert!(!p.to_string_lossy().contains("hunter2"), "{}", p.display());
            assert!(!std::fs::read_to_string(&p).unwrap().contains("hunter2"), "{}", p.display());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saving_a_wording_keeps_plus_signs_and_never_writes_a_secret() {
        let dir = std::env::temp_dir().join(format!("kvmit-wording-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("w.toml");
        let text = "workflow_format = 1
name = \"w\"
start = [\"A\"]
account = \"username\"
[secrets]
username = { prompt = \"Sign-in\" }
[[states]]
name = \"A\"
detect = { text = [\"Terms + conditions\"] }
terminal = true
";
        std::fs::write(&p, text).unwrap();
        let entered: BTreeMap<String, String> = [("username".to_string(), "jane@contoso.com".to_string())].into();
        // The sign-in, read off the screen and clicked as a wording: refused, the file unchanged, the value not repeated.
        let with_account = vec![vec!["Terms + conditions".to_string()], vec!["jane@contoso.com".to_string()]];
        let err = save_wording(&p, text, "A", &with_account, &entered).unwrap_err();
        assert!(err.contains("secret") && !err.contains("jane"), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), text);
        // A phrase too long to be valid that holds the sign-in: refused as a secret, never quoted by the parse error.
        let long = vec![vec![format!("{} jane@contoso.com", "x".repeat(220))]];
        let err = save_wording(&p, text, "A", &long, &entered).unwrap_err();
        assert!(!err.contains("jane"), "{err}");
        // The editor's rows, unchanged, give back the file's wordings: a literal + stays inside its phrase.
        let rows = vec![vec!["  Terms + conditions ".to_string(), String::new()], vec![String::new()]];
        assert_eq!(wording_sets(&rows), vec![vec!["Terms + conditions".to_string()]]);
        // A literal + stays inside its phrase.
        let two = vec![vec!["Terms + conditions".to_string()], vec!["Licence terms".to_string()]];
        save_wording(&p, text, "A", &two, &entered).unwrap();
        let saved = kvmit_script::Workflow::parse(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(saved.states[0].detect.text, two);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_picture_name_windows_would_read_as_another_is_refused() {
        let base = std::path::Path::new("base");
        for bad in ["pics./a.png", "pics /a.png", "a.png.", "a.png ", "pics\\x./a.png"] {
            assert!(reference_path(base, bad).unwrap_err().contains("may not end with a dot or a space"), "{bad}");
        }
        for ok in ["pics/a.png", "./pics/a.png", "a.b.png", "pics/.hidden.png"] {
            assert!(reference_path(base, ok).is_ok(), "{ok}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn saving_a_profile_keeps_the_files_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("kvmit-perm-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.toml");
        std::fs::write(&path, "a = 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        replace_if_unchanged(&path, "a = 1\n", "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "still private");
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
        assert!(!path.with_extension("json.running").exists(), "not marked as going on");
        std::fs::remove_dir_all(path.with_extension("json.claimed")).unwrap();
        assert!(claim_checkpoint(&dir, &a).is_ok(), "and the slot is free");
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
