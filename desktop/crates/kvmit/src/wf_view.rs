//! What the workflow panel shows, derived from a run's trace (no egui widgets, no I/O), so it is unit-tested like
//! `uistate`: the state chip, the transition timeline, and the names of exported files. `gui.rs` only draws these.
use crate::uistate::Health;
use kvmit_script::workflow::{TraceEntry, WorkflowEvent};

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

/// A stopped run's checkpoint, saved so a later run (in the app or `kvmit run`) can resume it. It holds no secrets
/// and no images (see [`kvmit_script::Checkpoint`]), and names the very file and text it came from: a checkpoint of
/// another file, or of this file before it was edited, is never offered.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedCheckpoint {
    pub source: Source,
    /// The workflow file's exact text, hashed.
    pub definition: String,
    pub checkpoint: kvmit_script::Checkpoint,
}

/// Where a source's checkpoint lives: one file per source, under `<scripts>/checkpoints/`. `None` when the source
/// cannot be named exactly (a file name that is not Unicode): such a run is not saved for resuming.
pub fn checkpoint_path(dir: &std::path::Path, source: &Source) -> Option<std::path::PathBuf> {
    let id = serde_json::to_string(source).ok()?;
    Some(dir.join("checkpoints").join(format!("{}.checkpoint.json", fnv(id.as_bytes()))))
}

/// Save (or replace) the checkpoint for this source. Written to a temporary file and renamed, so a reader never sees
/// half a checkpoint.
pub fn save_checkpoint(
    dir: &std::path::Path,
    source: &Source,
    text: &str,
    checkpoint: &kvmit_script::Checkpoint,
) -> std::io::Result<std::path::PathBuf> {
    let unnamed = || std::io::Error::new(std::io::ErrorKind::InvalidInput, "the workflow's file name is not Unicode");
    let path = checkpoint_path(dir, source).ok_or_else(unnamed)?;
    let saved = SavedCheckpoint { source: source.clone(), definition: fnv(text.as_bytes()), checkpoint: checkpoint.clone() };
    let json = serde_json::to_string_pretty(&saved).map_err(|_| unnamed())?;
    std::fs::create_dir_all(path.parent().expect("under checkpoints/"))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// The saved checkpoint for exactly this source and text, if there is one. A checkpoint of an earlier version of the
/// file is not returned (`Ok(None)`); an unreadable one is an error, so it is reported rather than silently ignored.
pub fn load_checkpoint(dir: &std::path::Path, source: &Source, text: &str) -> Result<Option<kvmit_script::Checkpoint>, String> {
    let Some(path) = checkpoint_path(dir, source) else { return Ok(None) };
    let json = match std::fs::read_to_string(&path) {
        Ok(j) => j,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let saved: SavedCheckpoint = serde_json::from_str(&json).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((saved.source == *source && saved.definition == fnv(text.as_bytes())).then_some(saved.checkpoint))
}

/// Forget this source's checkpoint (its run completed). A checkpoint that is not there is not an error.
pub fn clear_checkpoint(dir: &std::path::Path, source: &Source) -> std::io::Result<()> {
    match checkpoint_path(dir, source).map(std::fs::remove_file) {
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// After a run: a stopped live run with a known state saves its checkpoint, a completed one clears it, and a dry run
/// changes nothing (it sent nothing, so its attempt counts mean nothing to a live run). Returns what to tell the user.
pub fn keep_checkpoint(
    dir: &std::path::Path,
    source: &Source,
    text: &str,
    dry: bool,
    wf: &kvmit_script::Workflow,
    report: &kvmit_script::WorkflowReport,
) -> Option<String> {
    if dry {
        return None;
    }
    if report.result.is_ok() {
        return clear_checkpoint(dir, source).err().map(|e| format!("Could not remove the old checkpoint: {e}"));
    }
    let cp = report.checkpoint(wf)?;
    Some(match save_checkpoint(dir, source, text, &cp) {
        Ok(_) => format!("Saved a checkpoint at {}: Resume continues from there after looking at the screen.", cp.state),
        Err(e) => format!("Could not save a checkpoint: {e}"),
    })
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
        let _ = std::fs::remove_dir_all(&dir);
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
    fn a_saved_checkpoint_resumes_only_the_same_file_and_text() {
        use kvmit_script::workflow::Context;
        use kvmit_script::{Workflow, WorkflowReport};
        let dir = std::env::temp_dir().join(format!("kvmit-cp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "workflow_format = 1
name = \"w\"
start = [\"A\"]
match = \"luma\"
[[states]]
name = \"A\"
detect = { screen = \"a.png\" }
terminal = true
";
        let wf = Workflow::parse(text).unwrap();
        let (a, b) = (Source::File(dir.join("a.toml")), Source::Builtin("[built-in] a.toml".into()));
        let stopped = WorkflowReport {
            result: Err(WorkflowError::Cancelled),
            trace: vec![],
            context: Context { current: Some("A".into()), ..Default::default() },
        };
        assert_eq!(keep_checkpoint(&dir, &a, text, true, &wf, &stopped), None, "a dry run saves nothing");
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None));
        assert!(keep_checkpoint(&dir, &a, text, false, &wf, &stopped).unwrap().starts_with("Saved a checkpoint at A"));
        assert_eq!(load_checkpoint(&dir, &a, text).unwrap().map(|c| c.state), Some("A".into()));
        assert_eq!(load_checkpoint(&dir, &b, text), Ok(None), "another source");
        assert_eq!(load_checkpoint(&dir, &a, &format!("{text}# edited
")), Ok(None), "the file changed since");
        let done = WorkflowReport { result: Ok(()), ..stopped.clone() };
        assert_eq!(keep_checkpoint(&dir, &a, text, false, &wf, &done), None);
        assert_eq!(load_checkpoint(&dir, &a, text), Ok(None), "a completed run clears it");
        std::fs::create_dir_all(dir.join("checkpoints")).unwrap();
        std::fs::write(checkpoint_path(&dir, &a).unwrap(), "{ not json").unwrap();
        assert!(load_checkpoint(&dir, &a, text).is_err(), "a corrupt checkpoint is reported, not ignored");
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
