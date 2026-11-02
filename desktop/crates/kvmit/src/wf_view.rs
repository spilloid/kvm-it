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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let a = Source::File(std::ffi::OsStr::from_bytes(b"a-.toml").into());
        let b = Source::File(std::ffi::OsStr::from_bytes(b"a-.toml").into());
        if let (Source::File(pa), Source::File(pb)) = (&a, &b) {
            assert_eq!(pa.display().to_string(), pb.display().to_string(), "the display strings collide");
        }
        assert_ne!(a, b);
    }

    #[test]
    fn declining_or_aborting_is_not_a_failure() {
        assert_eq!(outcome(&Ok(())), RunOutcome::Finished);
        assert!(matches!(outcome(&Err(WorkflowError::Cancelled)), RunOutcome::Aborted(_)));
        assert!(matches!(outcome(&Err(WorkflowError::GateDeclined { state: "A".into() })), RunOutcome::Aborted(_)));
        assert!(matches!(outcome(&Err(WorkflowError::NoFrameNumbers)), RunOutcome::Failed(_)));
    }
}
