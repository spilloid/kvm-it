//! Workflow engine tests against a simulated target: synthetic frames and a mocked [`Host`]. Host-tested only;
//! nothing here has run against a real OOBE screen.

use super::*;
use kvmit_hid::Key;
use kvmit_layout::UsAnsi;
use std::collections::VecDeque;

const ENTER: u8 = 0x28;

fn solid(v: u8) -> Frame {
    Frame::new(64, 36, vec![v; 64 * 36]).unwrap()
}

/// A simulated target: its screens advance one step per Enter, like OOBE's "Next".
#[derive(Default)]
struct Sim {
    screens: Vec<Frame>,
    pos: usize,
    /// Enter presses the target swallows, by their 1-based position among all Enters.
    swallow: Vec<usize>,
    enters_seen: usize,
    /// On Enter at screen `.0`, go to screen `.1` instead of the next one.
    jump: Option<(usize, usize)>,
    /// Dry-run mode: frames advance by themselves, one per look.
    timeline: Option<Vec<Frame>>,
    looks: usize,
    keys: Vec<String>,
    typed: usize,
    confirms: VecDeque<bool>,
    asked: Vec<String>,
    /// When the operator approves a manual recovery, the target is moved to this screen.
    recovery_to: Option<usize>,
    cancel_after_keys: Option<usize>,
    /// The capture's frame counter: every look is a new captured frame unless the capture is stalled.
    seq: u64,
    /// The capture is stuck on this frame (and its number) from the start.
    frozen: bool,
    /// The capture stalls, showing what it showed just before, from the Nth Enter on.
    stall_at_enter: Option<usize>,
    stalled: Option<Frame>,
    /// While a question is open, the target changes to this screen.
    during_confirm: Option<(usize, Frame)>,
    /// After the Nth Enter and M more looks, the capture freezes on its last frame while the target shows an unknown
    /// dialog behind it.
    freeze_after_enter: Option<(usize, usize)>,
    freeze_countdown: Option<usize>,
    last_frame: Option<Frame>,
    /// Frames come without capture numbers.
    no_seq: bool,
    /// Pressing Enter fails with this host error.
    fail_enter_with: Option<String>,
    /// Each look really takes this long (classifying, a slow host).
    look_cost: Duration,
    /// Pictures still queued in the capture: delivered next, with new frame numbers, though they show the past.
    queued: VecDeque<Frame>,
    /// When the question is answered, this many pictures of the screen from before it are still queued.
    queue_on_confirm: usize,
    /// After the queue empties, the capture freezes on the last queued picture (same frame number).
    freeze_on_queued: bool,
    /// Queued pictures are stamped when delivered, not when queued: old pictures released late from the driver's own
    /// queue, which only the latency window can catch.
    queued_received_late: bool,
    frozen_queued: Option<Frame>,
    events: Vec<crate::exec::RunEvent>,
    /// Reference pictures not saved yet.
    missing: Vec<&'static str>,
    /// The text each screen shows, by its grey level (`None`: this host cannot read text). Lines are `|`-separated.
    texts: Option<Vec<(u8, &'static str)>>,
    /// Once anything is typed, the current screen also shows this line (what the field displays).
    echo: Option<&'static str>,
    /// Each reading of the screen's text really takes this long (a slow recogniser).
    read_delay: Duration,
    /// Every record the run made of its progress, with how many key presses had been sent by then.
    persisted: Vec<(Checkpoint, usize)>,
    /// Recording progress fails from the Nth record on (1-based): a full disk.
    fail_persist_at: Option<usize>,
}

impl Host for Sim {
    fn key_down(&mut self, k: Key) -> Result<(), String> {
        self.keys.push(format!("dn {:02x}", k.0));
        Ok(())
    }
    fn key_up(&mut self, k: Key) -> Result<(), String> {
        self.keys.push(format!("up {:02x}", k.0));
        self.typed += 1;
        if k.0 == ENTER {
            if let Some(msg) = &self.fail_enter_with {
                return Err(msg.clone());
            }
            self.enters_seen += 1;
            if let Some((n, m)) = self.freeze_after_enter {
                if n == self.enters_seen {
                    self.freeze_countdown = Some(m);
                }
            }
            if self.stall_at_enter == Some(self.enters_seen) {
                self.stalled = self.screens.get(self.pos).cloned().map(|f| f.with_seq(Some(self.seq)));
            }
            if self.swallow.contains(&self.enters_seen) {
                return Ok(());
            }
            match self.jump {
                Some((from, to)) if from == self.pos => self.pos = to,
                _ => self.pos = (self.pos + 1).min(self.screens.len() - 1),
            }
        }
        Ok(())
    }
    fn release_all(&mut self) -> Result<(), String> {
        self.keys.push("release_all".into());
        Ok(())
    }
    fn mouse_move(&mut self, _: i32, _: i32) -> Result<(), String> {
        Ok(())
    }
    fn mouse_button(&mut self, _: u8, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn sleep(&mut self, _: Duration) {}
    fn screen(&mut self) -> Option<Frame> {
        self.looks += 1;
        if !self.look_cost.is_zero() {
            std::thread::sleep(self.look_cost);
        }
        if let Some(f) = &self.stalled {
            return Some(f.clone());
        }
        if let Some(f) = self.queued.pop_front() {
            self.seq += 1;
            let f = f.with_seq(Some(self.seq));
            let f = if self.queued_received_late { f.with_captured_at(Some(std::time::Instant::now())) } else { f };
            if self.freeze_on_queued && self.queued.is_empty() {
                self.frozen_queued = Some(f.clone());
            }
            return Some(f);
        }
        if let Some(f) = &self.frozen_queued {
            return Some(f.clone());
        }
        match self.freeze_countdown {
            Some(0) => {
                // Frozen: the target has moved on to an unknown dialog, the capture still shows the old frame.
                let last = self.screens.len() - 1;
                self.screens[last.min(self.pos)] = solid(45);
                return self.last_frame.clone();
            }
            Some(n) => self.freeze_countdown = Some(n - 1),
            None => {}
        }
        if !self.frozen || self.seq == 0 {
            self.seq += 1;
        }
        let seq = Some(self.seq);
        if let Some(t) = &self.timeline {
            return t.get((self.looks - 1).min(t.len() - 1)).cloned().map(|f| f.with_seq(seq));
        }
        let seq = if self.no_seq { None } else { seq };
        let f = self.screens.get(self.pos).cloned().map(|f| f.with_seq(seq).with_captured_at(Some(std::time::Instant::now())));
        self.last_frame = f.clone();
        f
    }
    fn reference(&mut self, name: &str) -> Result<Frame, String> {
        let v = match name {
            "region.png" => 20,
            "region-alt.png" => 26,
            "keyboard.png" => 70,
            "network.png" => 120,
            "autopilot.png" | "contoso.png" => 170,
            "fabrikam.png" => 186,
            "done.png" => 225,
            _ => return Err(format!("no reference {name}")),
        };
        Ok(solid(v))
    }
    fn reference_missing(&mut self, name: &str) -> bool {
        self.missing.contains(&name)
    }
    fn read_text(&mut self, frame: &Frame) -> Result<Vec<crate::text::TextLine>, String> {
        if !self.read_delay.is_zero() {
            std::thread::sleep(self.read_delay);
        }
        let texts = self.texts.as_ref().ok_or("no recogniser")?;
        let shown = texts.iter().find(|(v, _)| *v == frame.gray[0]).map_or("", |(_, t)| *t);
        let echo = self.echo.filter(|_| self.typed > 0 && self.enters_seen == 0).unwrap_or("");
        Ok(shown
            .split('|')
            .chain([echo])
            .filter(|l| !l.is_empty())
            .enumerate()
            .map(|(i, l)| crate::text::TextLine { text: l.into(), rect: [0.55, 0.15 + 0.05 * i as f64, 0.3, 0.03] })
            .collect())
    }
    fn persist(&mut self, cp: &Checkpoint) -> Result<(), String> {
        if self.fail_persist_at.is_some_and(|n| self.persisted.len() + 1 >= n) {
            return Err("disk full".into());
        }
        self.persisted.push((cp.clone(), self.typed));
        Ok(())
    }
    fn confirm(&mut self, message: &str) -> bool {
        self.asked.push(message.to_string());
        if let Some((at, f)) = self.during_confirm.take() {
            for _ in 0..self.queue_on_confirm {
                // Received now, while the question is still open: pictures of the screen before the decision.
                self.queued.push_back(self.screens[at].clone().with_captured_at(Some(std::time::Instant::now())));
            }
            self.screens[at] = f;
        }
        let yes = self.confirms.pop_front().unwrap_or(false);
        if yes {
            if let Some(to) = self.recovery_to.take() {
                self.pos = to;
            }
        }
        yes
    }
    fn cancelled(&self) -> bool {
        self.cancel_after_keys.is_some_and(|n| self.typed >= n)
    }
    fn event(&mut self, e: crate::exec::RunEvent) {
        self.events.push(e);
    }
}

const OOBE: &str = r#"
workflow_format = 1
name = "test-oobe"
start = ["REGION"]
# The simulated screens are flat greys, which have no pattern to correlate: compare brightness.
match = "luma"

[config]
region = "United States"
deployment_mode = "autopilot"
tenant = "contoso"

[secrets]
wifi = { prompt = "Wi-Fi password" }

[tenants]
contoso = "contoso.png"
fabrikam = "fabrikam.png"

[[states]]
name = "REGION"
detect = { screen = "region.png" }
next = ["KEYBOARD"]
actions = [ { text = "{{region}}" }, { key = "ENTER" } ]
timeout = "2s"
derive = { region_set = "yes" }

[[states]]
name = "KEYBOARD"
detect = { screen = "keyboard.png" }
next = ["NETWORK"]
actions = [ { key = "ENTER" } ]
retries = 1
retry_safe = true
timeout = "2s"

[[states]]
name = "NETWORK"
detect = { screen = "network.png" }
next = ["AUTOPILOT"]
actions = [ { secret_text = "{{wifi}}" }, { key = "ENTER" } ]
timeout = "2s"
derive = { network_status = "connected" }

[[states]]
name = "AUTOPILOT"
detect = { screen = "autopilot.png" }
require = { deployment_mode = "autopilot", network_status = "connected" }
verify_tenant = true
gate = "Enroll this device into the tenant shown?"
next = ["DONE"]
actions = [ { key = "ENTER" } ]
timeout = "2s"

[[states]]
name = "DONE"
detect = { screen = "done.png" }
terminal = true
"#;

fn wf() -> Workflow {
    Workflow::parse(OOBE).unwrap()
}
fn secrets() -> Vars {
    [("wifi".to_string(), "hunter2-wifi".to_string())].into()
}
fn screens() -> Vec<Frame> {
    [20, 70, 120, 170, 225].into_iter().map(solid).collect()
}
fn opts() -> WorkflowOptions {
    WorkflowOptions {
        run: RunOptions { key_hold: Duration::ZERO, key_gap: Duration::ZERO, ..Default::default() },
        observe_timeout: Duration::from_secs(2),
        ..Default::default()
    }
}
fn go(wf: &Workflow, sim: &mut Sim, o: &WorkflowOptions, resume: Option<&Checkpoint>) -> WorkflowReport {
    run_workflow(wf, &secrets(), &UsAnsi, sim, o, resume, &mut |_, _| {})
}
fn events(r: &WorkflowReport) -> Vec<&WorkflowEvent> {
    r.trace.iter().map(|t| &t.event).collect()
}
fn enters(sim: &Sim) -> usize {
    sim.keys.iter().filter(|k| *k == "up 28").count()
}

#[test]
fn a_simulated_oobe_runs_through_every_state_with_verified_transitions() {
    let w = wf();
    let mut seen = 0;
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = run_workflow(&w, &secrets(), &UsAnsi, &mut sim, &opts(), None, &mut |_, _| seen += 1);
    assert_eq!(r.result, Ok(()));
    assert_eq!(seen, r.trace.len(), "the live sink saw every entry");
    let path: Vec<_> = r.context.history.iter().map(|t| (t.from.as_str(), t.to.as_str())).collect();
    assert_eq!(
        path,
        [("REGION", "KEYBOARD"), ("KEYBOARD", "NETWORK"), ("NETWORK", "AUTOPILOT"), ("AUTOPILOT", "DONE")]
    );
    assert_eq!(r.context.current.as_deref(), Some("DONE"));
    assert_eq!(r.context.derived["network_status"].0, "connected");
    let ev = events(&r);
    assert_eq!(ev.iter().filter(|e| matches!(e, WorkflowEvent::TransitionVerified { .. })).count(), 4);
    assert!(ev.iter().any(|e| matches!(e, WorkflowEvent::TenantVerified { tenant, .. } if tenant == "contoso")));
    assert!(ev.iter().any(|e| matches!(e, WorkflowEvent::ContextChanged { key, .. } if key == "derived.network_status")));
    assert!(matches!(ev.last(), Some(WorkflowEvent::RunCompleted { state }) if state == "DONE"));
    assert_eq!(sim.asked, ["Enroll this device into the tenant shown?"]);
    assert_eq!(sim.keys.last().map(String::as_str), Some("release_all"));
    // The inspector shows where each value came from.
    let v = r.context.inspect();
    assert!(v.iter().any(|v| v.key == "tenant" && v.provenance == Provenance::Configured));
    assert!(v
        .iter()
        .any(|v| v.key == "network_status" && v.provenance == Provenance::Verified && v.changed_at_ms.is_some()));
    assert!(v.iter().any(|v| v.namespace == "observed" && v.provenance == Provenance::Observed));
}

#[test]
fn an_unrecognised_screen_types_nothing_and_offers_a_manual_fix() {
    let w = wf();
    // Mid-grey, between REGION and KEYBOARD: matches neither with confidence.
    let mut stuck = screens();
    stuck.insert(0, solid(45));
    let mut sim = Sim { screens: stuck.clone(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(sim.typed, 0, "nothing typed on an unknown screen");
    assert_eq!(sim.keys, ["release_all"], "the run's own cleanup released everything");
    assert_eq!(sim.asked.len(), 1, "a manual fix was offered");
    // The operator brings the target to the first screen and continues: the run proceeds.
    let mut sim = Sim { screens: stuck, confirms: [true, true].into(), recovery_to: Some(1), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()));
}

#[test]
fn an_ambiguous_screen_or_a_flicker_is_not_a_classification() {
    let mut w = wf();
    w.ambiguity_margin = 0.05;
    let mut o = opts();
    o.offer_manual_recovery = false;
    // 45 sits exactly between 20 and 70: both are equally close.
    let mut sim = Sim { screens: vec![solid(45)], ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })));
    // A screen that is REGION only every other frame never settles.
    let flicker: Vec<Frame> = (0..40).map(|i| if i % 2 == 0 { solid(20) } else { solid(45) }).collect();
    let mut sim = Sim { timeline: Some(flicker), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(sim.typed, 0);

    // Clearly closest to one state, but not close enough to it: white is nearest DONE (225) yet under its
    // threshold.
    let mut sim = Sim { screens: vec![solid(255)], ..Default::default() };
    assert!(matches!(go(&w, &mut sim, &o, None).result, Err(WorkflowError::UnknownScreen { .. })));

    // Close enough to two states at once: both pass their threshold, neither wins by the margin.
    let twin = OOBE.to_string()
        + "
[[states]]
name = \"REGION_ALT\"
detect = { screen = \"region-alt.png\" }
terminal = true
";
    let mut twin = Workflow::parse(&twin).unwrap();
    twin.ambiguity_margin = 0.05;
    let mut sim = Sim { screens: vec![solid(23)], ..Default::default() };
    let r = go(&twin, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(sim.typed, 0);
    // An exact tie never wins, even with a zero margin set in code (the parser refuses one in a file).
    twin.ambiguity_margin = 0.0;
    let mut sim = Sim { screens: vec![solid(23)], ..Default::default() };
    let r = go(&twin, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
}

#[test]
fn an_unexpected_screen_after_acting_stops_the_run() {
    let w = wf();
    // Enter on REGION jumps straight to NETWORK, skipping KEYBOARD.
    let mut sim = Sim { screens: screens(), jump: Some((0, 2)), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Unexpected { saw, .. }) if saw == "NETWORK"), "{:?}", r.result);
    assert_eq!(enters(&sim), 1, "nothing typed after the surprise");
    assert!(r.context.derived.is_empty(), "no fact is recorded without a verified transition");
}

#[test]
fn retries_are_bounded_and_only_where_marked_safe() {
    let w = wf();
    // KEYBOARD (retry_safe, one retry) swallows its first Enter (the second Enter overall): the retry moves on.
    let mut sim = Sim { screens: screens(), swallow: vec![2], confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()), "{:?}", r.result);
    assert_eq!(r.context.attempts["KEYBOARD"], 2);
    assert_eq!(r.context.history[1].attempts, 2);

    // Swallowing both of KEYBOARD's Enters exhausts the one retry.
    let mut sim = Sim { screens: screens(), swallow: vec![2, 3], ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::TimedOut { state: "KEYBOARD".into(), attempts: 2 }));
    assert_eq!(enters(&sim), 3);

    // REGION is not retry_safe: a screen that does not move stops after one attempt, typing nothing more.
    let mut sim = Sim { screens: screens(), swallow: (1..100).collect(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::TimedOut { state: "REGION".into(), attempts: 1 }));
    assert_eq!(enters(&sim), 1);
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::StateTimedOut { .. })));
}

#[test]
fn the_wrong_tenant_a_failed_guard_or_a_declined_gate_stops_before_enrolling() {
    let w = wf();
    let mut fabrikam = screens();
    fabrikam[3] = solid(186);
    let mut sim = Sim { screens: fabrikam, confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(r.result, Err(WorkflowError::TenantMismatch { .. })), "{:?}", r.result);
    assert!(sim.asked.is_empty(), "stopped before the enrollment gate");
    assert_eq!(enters(&sim), 3, "nothing typed on the enrollment screen");

    let mut local = wf();
    local.config.insert("deployment_mode".into(), "local".into());
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = go(&local, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Guard { state, .. }) if state == "AUTOPILOT"));
    assert_eq!(enters(&sim), 3);

    let mut sim = Sim { screens: screens(), confirms: [false].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::GateDeclined { state: "AUTOPILOT".into() }));
    assert_eq!(enters(&sim), 3);
    assert_eq!(sim.keys.last().map(String::as_str), Some("release_all"));
}

#[test]
fn abort_releases_every_key() {
    let w = wf();
    let mut sim = Sim { screens: screens(), cancel_after_keys: Some(5), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::Cancelled));
    assert_eq!(sim.keys.last().map(String::as_str), Some("release_all"));
    assert!(matches!(events(&r).last(), Some(WorkflowEvent::Stopped { .. })));
}

#[test]
fn dry_run_follows_synthetic_frames_and_sends_nothing() {
    let w = wf();
    let timeline: Vec<Frame> =
        [20, 70, 120, 170, 225].into_iter().flat_map(|v| std::iter::repeat_n(solid(v), 12)).collect();
    let mut sim = Sim { timeline: Some(timeline), screens: screens(), ..Default::default() };
    let mut o = opts();
    o.run.dry_run = true;
    let r = go(&w, &mut sim, &o, None);
    assert_eq!(r.result, Ok(()), "{:?}", r.trace);
    assert!(sim.keys.is_empty(), "no HID at all, not even release_all: {:?}", sim.keys);
    assert!(sim.asked.is_empty(), "gates are not asked in a dry run");
    assert!(events(&r).iter().all(|e| !matches!(e, WorkflowEvent::ActionAttempted { dry_run: false, .. })));
}

#[test]
fn secrets_never_reach_the_trace_the_exports_or_a_checkpoint() {
    let w = wf();
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()));
    let all = [
        trace_jsonl(&r),
        trace_markdown(&w, &r),
        format!("{:?}", r.trace),
        format!("{:?}", r.context),
        r.checkpoint(&w).unwrap().to_json(),
        format!("{w:?}"),
    ];
    for text in &all {
        assert!(!text.contains("hunter2"), "secret leaked: {text}");
    }
    // Every line of the export is one JSON object with a timestamp and an event name.
    for line in trace_jsonl(&r).lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["at_ms"].is_u64() && v["event"].is_string(), "{line}");
    }
    assert!(trace_markdown(&w, &r).contains("| 4 | AUTOPILOT | DONE |"));
}

#[test]
fn resuming_re_observes_the_screen_and_never_replays_blindly() {
    let w = wf();
    let cp = Checkpoint {
        workflow: "test-oobe".into(),
        state: "NETWORK".into(),
        derived: [("region_set".to_string(), "yes".to_string())].into(),
        attempts: BTreeMap::new(),
        attempted: Some(0),
        waiting: false,
        history: vec![],
    };
    let cp = Checkpoint::from_json(&cp.to_json()).unwrap();
    // The target is on NETWORK: resume there.
    let mut sim = Sim { screens: screens(), pos: 2, confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Ok(()));
    assert_eq!(r.context.history.first().map(|t| t.from.as_str()), Some("NETWORK"));
    // The target went back to REGION (it rebooted): refuse, type nothing.
    let mut sim = Sim { screens: screens(), pos: 0, ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Err(WorkflowError::ResumeMismatch { checkpoint: "NETWORK".into() }));
    assert_eq!(sim.typed, 0);
    // A checkpoint from another workflow is refused before looking.
    let other = Checkpoint { workflow: "other".into(), ..cp };
    let mut sim = Sim { screens: screens(), pos: 2, ..Default::default() };
    assert!(matches!(go(&w, &mut sim, &opts(), Some(&other)).result, Err(WorkflowError::ResumeMismatch { .. })));
}

#[test]
fn a_missing_secret_stops_before_the_first_key() {
    let w = wf();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = run_workflow(&w, &Vars::new(), &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(&r.result, Err(WorkflowError::Compile { state, .. }) if state == "NETWORK"), "{:?}", r.result);
    assert_eq!(sim.typed, 0);
    assert_eq!(sim.keys, ["release_all"]);
}

#[test]
fn malformed_workflows_are_refused() {
    let bad = |from: &str, to: &str| {
        assert!(OOBE.contains(from), "{from}");
        Workflow::parse(&OOBE.replace(from, to)).unwrap_err()
    };
    assert!(bad("retries = 1\nretry_safe = true", "retries = 1").contains("retry_safe"));
    assert!(bad("next = [\"KEYBOARD\"]", "next = [\"NOWHERE\"]").contains("unknown state"));
    assert!(bad("tenant = \"contoso\"", "tenant = \"initech\"").contains("not in the workflow's allowed tenants"));
    assert!(bad("workflow_format = 1", "workflow_format = 2").contains("unsupported"));
    assert!(bad("terminal = true", "").contains("needs `next`"));
    assert!(bad("{ secret_text = \"{{wifi}}\" }", "{ secret_text = \"literal\" }").contains("secret_text must reference"));
    assert!(bad("require = { deployment_mode", "require = { wifi = \"x\", deployment_mode").contains("cannot compare a secret"));
    // A state that may "transition" to itself would verify a screen that never changed.
    assert!(bad("next = [\"KEYBOARD\"]", "next = [\"REGION\"]").contains("lists itself"));
    // Blocking steps belong to states, not inside actions.
    assert!(bad("{ key = \"ENTER\" } ]\nretries", "{ confirm = \"ok?\" } ]\nretries").contains("not allowed in workflow actions"));
    // Secret text only from prompted secrets: a config value is on disk and in every export.
    assert!(bad("{ secret_text = \"{{wifi}}\" }", "{ secret_text = \"{{region}}\" }").contains("not under [secrets]"));
    // A tie must never pick a state.
    assert!(bad("start = [\"REGION\"]", "start = [\"REGION\"]\nambiguity_margin = 0.0").contains("ambiguity_margin"));
    assert!(bad("derive = { region_set = \"yes\" }", "derive = { region_set = \"a\\nb\" }").contains("one line"));
    assert!(bad("timeout = \"2s\"\nderive = { region_set", "timeout = \"100ms\"\nderive = { region_set").contains("at least 500ms"));
}

#[test]
fn an_unknown_screen_after_acting_is_never_retried_into() {
    let w = wf();
    // KEYBOARD is retry_safe, but after its Enter an unrecognised dialog appears: stop, do not press Enter again.
    let mut screens = screens();
    screens[2] = solid(45);
    let mut sim = Sim { screens, ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 2, "REGION's Enter and KEYBOARD's one; no retry into the unknown screen");
}

#[test]
fn a_frozen_or_stalled_capture_never_counts_as_a_look() {
    let w = wf();
    let mut o = opts();
    o.offer_manual_recovery = false;
    // The same captured frame, read again and again, never settles.
    let mut sim = Sim { screens: screens(), frozen: true, ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(sim.typed, 0);
    // The capture stalls when KEYBOARD's Enter is pressed: the frame it keeps showing predates the action, so it
    // neither verifies nor refutes it. The run stops; it does not retry blind.
    let mut sim = Sim { screens: screens(), stall_at_enter: Some(2), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 2, "no retry on a stalled capture");
    // Even when one look is enough to settle, a frame captured before the action cannot be that look.
    let mut one = o.clone();
    one.settle_frames = 1;
    // No latency window either: the frame number boundary alone must keep the pre-action frame out.
    one.capture_latency = Duration::ZERO;
    let mut sim = Sim { screens: screens(), stall_at_enter: Some(2), ..Default::default() };
    let r = go(&w, &mut sim, &one, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 2, "the pre-action frame did not count as 'stayed put'");
}

#[test]
fn an_unchanged_screen_is_never_a_transition_even_if_a_state_lists_itself() {
    // The parser refuses self-edges; the engine refuses them too, for workflows built in code.
    let mut w = wf();
    w.states[0].next = vec!["REGION".into(), "KEYBOARD".into()];
    let mut sim = Sim { screens: screens(), swallow: (1..100).collect(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::TimedOut { state: "REGION".into(), attempts: 1 }));
    assert!(r.context.history.is_empty() && r.context.derived.is_empty());
}

#[test]
fn approval_is_followed_by_a_fresh_look_before_typing() {
    let w = wf();
    // While the enrollment question is open, the target shows another tenant's sign-in.
    let mut sim = Sim { screens: screens(), confirms: [true].into(), during_confirm: Some((3, solid(186))), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(r.result, Err(WorkflowError::TenantMismatch { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 3, "nothing typed on the enrollment screen");
    // The trace never says keys went that the last look held back (it is the run's record for the ticket).
    assert!(
        !events(&r).iter().any(|e| matches!(e, WorkflowEvent::ActionAttempted { state, .. } if state == "AUTOPILOT")),
        "{:?}",
        r.trace
    );
    assert_eq!(r.checkpoint(&w).unwrap().attempted, Some(0));
    // Pictures from before the decision still queued in the capture (new frame numbers, old screen) are not the fresh
    // look: they arrive within the capture latency and are skipped.
    let mut sim = Sim {
        screens: screens(),
        confirms: [true].into(),
        during_confirm: Some((3, solid(186))),
        queue_on_confirm: 2,
        ..Default::default()
    };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(r.result, Err(WorkflowError::TenantMismatch { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 3, "the queued pictures did not authorise typing");
    // ... or moved to a different screen altogether.
    let mut sim = Sim { screens: screens(), confirms: [true].into(), during_confirm: Some((3, solid(120))), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Unexpected { saw, .. }) if saw == "NETWORK"), "{:?}", r.result);
    assert_eq!(enters(&sim), 3);
}

#[test]
fn resuming_keeps_spent_attempts_and_asks_before_resending() {
    let w = wf();
    let cp = |state: &str, attempts: u32| Checkpoint {
        workflow: "test-oobe".into(),
        state: state.into(),
        derived: BTreeMap::new(),
        attempts: [(state.to_string(), attempts)].into(),
        attempted: Some(attempts),
        waiting: false,
        history: vec![],
    };
    // KEYBOARD's whole budget (1 + 1 retry) was spent: resuming does not grant a new one.
    let mut sim = Sim { screens: screens(), pos: 1, ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp("KEYBOARD", 2)));
    assert_eq!(r.result, Err(WorkflowError::TimedOut { state: "KEYBOARD".into(), attempts: 2 }));
    assert_eq!(sim.typed, 0);
    // REGION is not retry_safe and was attempted: its text is not typed again unless the operator says so.
    let mut sim = Sim { screens: screens(), pos: 0, confirms: [false].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp("REGION", 1)));
    assert_eq!(r.result, Err(WorkflowError::ResumeDeclined { state: "REGION".into() }));
    assert_eq!(sim.typed, 0);
    let mut sim = Sim { screens: screens(), pos: 0, confirms: [true, true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp("REGION", 1)));
    assert_eq!(r.result, Ok(()));
}

#[test]
fn resuming_on_the_next_screen_records_the_pending_transition() {
    let w = wf();
    // Stopped in NETWORK; the target reached AUTOPILOT meanwhile. NETWORK's fact must be recorded so AUTOPILOT's
    // guard (network_status = connected) passes.
    let cp = Checkpoint {
        workflow: "test-oobe".into(),
        state: "NETWORK".into(),
        derived: BTreeMap::new(),
        attempts: [("NETWORK".to_string(), 1)].into(),
        attempted: Some(1),
        waiting: false,
        history: vec![],
    };
    let mut sim = Sim { screens: screens(), pos: 3, confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Ok(()), "{:?}", r.result);
    assert_eq!(r.context.derived["network_status"].0, "connected");
    assert_eq!((r.context.history[0].from.as_str(), r.context.history[0].to.as_str()), ("NETWORK", "AUTOPILOT"));
}

#[test]
fn a_secret_pasted_into_free_text_is_scrubbed_from_everything_published() {
    // An author wrote the secret's value into a gate message by mistake.
    let leaky = OOBE.replace("Enroll this device into the tenant shown?", "Wi-Fi was hunter2-wifi. Enroll?");
    let w = Workflow::parse(&leaky).unwrap();
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "state 4 (in file order)".into() }));
    assert_eq!(sim.typed, 0);
    // The same for a recorded fact, which would otherwise sit in the context, the inspector and checkpoints.
    let leaky = OOBE.replace("derive = { region_set = \"yes\" }", "derive = { region_set = \"hunter2-wifi\" }");
    let w = Workflow::parse(&leaky).unwrap();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "state 1 (in file order)".into() }));
    for text in [trace_jsonl(&r), trace_markdown(&w, &r), format!("{r:?}")] {
        assert!(!text.contains("hunter2"), "{text}");
    }

    // A host diagnostic carrying the secret is scrubbed everywhere, the nested runner's own events included.
    let w = wf();
    let mut sim = Sim { screens: screens(), fail_enter_with: Some("adapter said: hunter2-wifi".into()), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(r.result, Err(WorkflowError::Action { .. })), "{:?}", r.result);
    for text in [trace_jsonl(&r), trace_markdown(&w, &r), format!("{r:?}"), format!("{:?}", sim.events)] {
        assert!(!text.contains("hunter2"), "{text}");
    }
}

#[test]
fn overlapping_secrets_are_redacted_whole() {
    let s = Secrets(vec!["abc".into(), "abcSECRET".into()]);
    let mut t = "x abcSECRET y abc".to_string();
    s.scrub(&mut t);
    assert_eq!(t, "x <redacted> y <redacted>");
    let s = Secrets(vec!["cdef".into(), "abcd".into()]);
    let mut t = "abcdef!".to_string();
    s.scrub(&mut t);
    assert_eq!(t, "<redacted>!");
}

#[test]
fn a_capture_that_freezes_after_settling_does_not_authorise_a_retry() {
    let w = wf();
    let mut o = opts();
    o.offer_manual_recovery = false;
    // KEYBOARD swallows its first Enter (the second overall). The verify look settles on KEYBOARD, then the capture
    // freezes while an unknown dialog appears. The retry needs a fresh look, which a frozen capture cannot give.
    let mut sim = Sim { screens: screens(), swallow: vec![2], freeze_after_enter: Some((2, 6)), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 2, "no retry on evidence from before the freeze");

    // One fresh frame after REGION's Enter, then the capture freezes on it: one frame is not a settled look, however
    // often it is read, so the transition is not verified.
    let mut sim = Sim { screens: screens(), freeze_after_enter: Some((1, 4)), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert!(r.context.history.is_empty(), "nothing verified from one frame read many times: {:?}", r.context.history);
}

#[test]
fn frames_without_capture_numbers_are_refused() {
    let w = wf();
    let mut sim = Sim { screens: screens(), no_seq: true, ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::NoFrameNumbers));
    assert_eq!(sim.typed, 0);
}

#[test]
fn a_checkpoint_that_sent_nothing_has_no_pending_transition() {
    let w = wf();
    // Stopped at NETWORK before sending anything; the target is now on AUTOPILOT. NETWORK's fact is not invented, so
    // AUTOPILOT's guard (network_status = connected) stops the run.
    let cp = Checkpoint {
        workflow: "test-oobe".into(),
        state: "NETWORK".into(),
        derived: BTreeMap::new(),
        attempts: [("NETWORK".to_string(), 3)].into(),
        attempted: Some(0),
        waiting: false,
        history: vec![],
    };
    let mut sim = Sim { screens: screens(), pos: 3, confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert!(matches!(&r.result, Err(WorkflowError::Guard { state, .. }) if state == "AUTOPILOT"), "{:?}", r.result);
    assert!(r.context.derived.is_empty());
    assert_eq!(sim.typed, 0);
    // Attempts from earlier visits are history, not this visit's budget: resuming on NETWORK itself just runs.
    let cp = Checkpoint { state: "NETWORK".into(), ..cp };
    let mut sim = Sim { screens: screens(), pos: 2, confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Ok(()), "{:?}", r.result);
    assert!(sim.asked.iter().all(|q| !q.contains("already sent")), "{:?}", sim.asked);
}

#[test]
fn long_pauses_inside_actions_are_refused() {
    let bad = |from: &str, to: &str| Workflow::parse(&OOBE.replace(from, to)).unwrap_err();
    assert!(bad("actions = [ { key = \"ENTER\" } ]\nretries", "actions = [ { delay = \"3s\" }, { key = \"ENTER\" } ]\nretries")
        .contains("pause for more than 2s"));
    assert!(bad(
        "actions = [ { key = \"ENTER\" } ]\nretries",
        "actions = [ { repeat = { count = 5, steps = [ { delay = \"500ms\" } ] } }, { key = \"ENTER\" } ]\nretries"
    )
    .contains("pause for more than 2s"));
    let w = wf();
    let mut o = opts();
    o.run.default_delay = Duration::from_secs(5);
    let mut sim = Sim { screens: screens(), ..Default::default() };
    assert!(matches!(go(&w, &mut sim, &o, None).result, Err(WorkflowError::Options(_))));
    assert_eq!(sim.typed, 0);
}

#[test]
fn evidence_arriving_after_the_real_deadline_is_not_accepted() {
    let w = wf();
    let mut o = opts();
    o.offer_manual_recovery = false;
    o.observe_timeout = Duration::from_millis(500);
    // Each look really takes 400 ms: the second settling frame arrives after the 500 ms deadline, even though the
    // poll count (whose sleeps return at once here) would still be inside it.
    let mut sim = Sim { screens: screens(), look_cost: Duration::from_millis(400), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(sim.typed, 0);
    assert!(
        !events(&r).iter().any(|e| matches!(e, WorkflowEvent::StateEntered { .. })),
        "the late frame did not even start the run: {:#?}", r.trace
    );
}

#[test]
fn a_queued_picture_seen_inside_the_latency_window_never_counts_later() {
    let w = wf();
    let mut o = opts();
    o.settle_frames = 1;
    o.offer_manual_recovery = false;
    // After approval one queued picture of the old screen arrives inside the window, then the capture freezes on it
    // while the target shows another tenant. Once the window has passed, that same picture is still not evidence.
    let mut sim = Sim {
        screens: screens(),
        confirms: [true].into(),
        during_confirm: Some((3, solid(186))),
        // Two: the fresh look's own boundary read takes the first; the second arrives inside the window.
        queue_on_confirm: 2,
        freeze_on_queued: true,
        queued_received_late: true,
        ..Default::default()
    };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 3, "nothing typed on the enrollment screen");
}

#[test]
fn the_pause_budget_counts_default_delays_and_key_timing() {
    let w = wf();
    // REGION has two steps: a 3 s default delay after each makes 6 s with nothing watched.
    let mut o = opts();
    o.run.default_delay = Duration::from_secs(3);
    let mut sim = Sim { screens: screens(), ..Default::default() };
    assert!(matches!(go(&w, &mut sim, &o, None).result, Err(WorkflowError::Options(_))));
    let mut o = opts();
    o.run.key_gap = Duration::from_secs(1);
    let mut sim = Sim { screens: screens(), ..Default::default() };
    assert!(matches!(go(&w, &mut sim, &o, None).result, Err(WorkflowError::Options(_))));
    assert_eq!(sim.typed, 0);
}

#[test]
fn an_older_checkpoint_without_attempted_counts_as_sent() {
    let w = wf();
    // Written before `attempted` existed: REGION had been attempted once. Its text is not typed again unasked.
    let old = r#"{"workflow":"test-oobe","state":"REGION","derived":{},"attempts":{"REGION":1},"history":[]}"#;
    let cp = Checkpoint::from_json(old).unwrap();
    assert_eq!(cp.attempted, None);
    let mut sim = Sim { screens: screens(), confirms: [false].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Err(WorkflowError::ResumeDeclined { state: "REGION".into() }));
    assert_eq!(sim.typed, 0);
}

#[test]
fn a_secret_anywhere_in_the_definition_is_refused_before_anything_is_published() {
    let leaky = OOBE.replace("name = \"test-oobe\"", "name = \"hunter2-wifi\"");
    let w = Workflow::parse(&leaky).unwrap();
    let mut seen: Vec<String> = Vec::new();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = run_workflow(&w, &secrets(), &UsAnsi, &mut sim, &opts(), None, &mut |t, c| {
        seen.push(format!("{t:?} {c:?}"));
    });
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "the workflow's name, description or account".into() }));
    assert_eq!(r.context, Context::default(), "no context was built");
    for text in seen.iter().chain([trace_jsonl(&r), trace_markdown(&w, &r), format!("{r:?}")].iter()) {
        assert!(!text.contains("hunter2"), "{text}");
    }
    assert_eq!(sim.typed, 0);
    // In a key name, too (it would surface in a compile error).
    let leaky = OOBE.replace("actions = [ { key = \"ENTER\" } ]\nretries", "actions = [ { key = \"hunter2-wifi\" } ]\nretries");
    let w = Workflow::parse(&leaky).unwrap();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "state 2 (in file order)".into() }));
}

#[test]
fn a_frame_arriving_just_after_the_deadline_is_not_accepted() {
    let w = wf();
    let mut o = opts();
    o.offer_manual_recovery = false;
    o.observe_timeout = Duration::from_millis(500);
    // 300 ms per look: the second settling frame lands at about 600 ms, after the 500 ms deadline.
    let mut sim = Sim { screens: screens(), look_cost: Duration::from_millis(300), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert!(!events(&r).iter().any(|e| matches!(e, WorkflowEvent::StateEntered { .. })));
}

#[test]
fn a_workflow_must_say_how_it_compares_screens() {
    let e = Workflow::parse(&OOBE.replace("match = \"luma\"\n", "")).unwrap_err();
    assert!(e.contains("`match` is required"), "{e}");
    assert!(Workflow::parse(&OOBE.replace("match = \"luma\"", "match = \"pixels\"")).is_err());
}

#[test]
fn many_short_gaps_add_up_against_the_action_time_limit() {
    // 600 taps with a 100 ms gap each: every timing is within its own limit, together a minute unwatched.
    let long = OOBE.replace(
        "actions = [ { key = \"ENTER\" } ]\nretries",
        "actions = [ { repeat = { count = 600, steps = [ { key = \"SHIFT\" } ] } }, { key = \"ENTER\" } ]\nretries",
    );
    let w = Workflow::parse(&long).unwrap();
    let mut o = opts();
    o.run.key_gap = Duration::from_millis(100);
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(&r.result, Err(WorkflowError::Options(m)) if m.contains("state 2")), "{:?}", r.result);
    assert_eq!(sim.typed, 0, "refused before the first key");
}

#[test]
fn the_secret_check_comes_first_and_never_names_the_secret() {
    // A secret in the name and a bad timing option: the secret is what is reported, so nothing echoes the name.
    let leaky = OOBE.replace("name = \"test-oobe\"", "name = \"hunter2-wifi\"");
    let w = Workflow::parse(&leaky).unwrap();
    let mut o = opts();
    o.run.key_gap = Duration::from_secs(1);
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::SecretInFile { .. })), "{:?}", r.result);
    for text in [trace_jsonl(&r), trace_markdown(&w, &r), format!("{r:?}")] {
        assert!(!text.contains("hunter2"), "{text}");
    }
    // A config *key* equal to the secret: the location says where by position, not by the key.
    let leaky = OOBE.replace("tenant = \"contoso\"\n", "tenant = \"contoso\"\nhunter2_wifi = \"ordinary\"\n");
    let w = Workflow::parse(&leaky).unwrap();
    let key_secret: Vars = [("wifi".to_string(), "hunter2_wifi".to_string())].into();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = run_workflow(&w, &key_secret, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(&r.result, Err(WorkflowError::SecretInFile { place }) if place.starts_with("config entry")), "{:?}", r.result);
    for text in [trace_jsonl(&r), trace_markdown(&w, &r), format!("{r:?}")] {
        assert!(!text.contains("hunter2"), "{text}");
    }
}

#[test]
fn a_secret_with_quotes_is_found_in_a_checkpoint() {
    // A formatted (Debug) rendering would escape the quote and miss it; the real values are scanned.
    let w = wf();
    let quoted: Vars = [("wifi".to_string(), "pa\"ss".to_string())].into();
    let cp = Checkpoint {
        workflow: "test-oobe".into(),
        state: "NETWORK".into(),
        derived: [("note".to_string(), "pa\"ss".to_string())].into(),
        attempts: BTreeMap::new(),
        attempted: Some(0),
        waiting: false,
        history: vec![],
    };
    let mut seen = Vec::new();
    let mut sim = Sim { screens: screens(), pos: 2, ..Default::default() };
    let r = run_workflow(&w, &quoted, &UsAnsi, &mut sim, &opts(), Some(&cp), &mut |t, c| seen.push(format!("{t:?}{c:?}")));
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "the checkpoint".into() }));
    assert!(seen.iter().all(|t| !t.contains("pa\\\"ss") && !t.contains("pa\"ss")), "{seen:?}");
    assert_eq!(r.context, Context::default());
}

#[test]
fn no_capture_is_started_after_the_deadline() {
    let w = wf();
    let mut o = opts();
    o.offer_manual_recovery = false;
    o.observe_timeout = Duration::from_millis(500);
    // Unknown screen, 250 ms polls: looks at 0 and 250 ms; at 500 ms the deadline has passed and no capture starts.
    let mut sim = Sim { screens: vec![solid(45)], ..Default::default() };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { best: Some(_) })), "{:?}", r.result);
    assert_eq!(sim.looks, 2);
}

#[test]
fn a_picture_received_before_a_decision_is_never_evidence_after_it() {
    let w = wf();
    let mut o = opts();
    // No latency window at all: only the capture time can tell these pictures apart.
    o.capture_latency = Duration::ZERO;
    // While the enrollment question is open, the target switches tenant, and three pictures of the old (approved)
    // tenant are already queued. They arrive after the approval with new frame numbers, but were received before it.
    let mut sim = Sim {
        screens: screens(),
        confirms: [true].into(),
        during_confirm: Some((3, solid(186))),
        queue_on_confirm: 3,
        ..Default::default()
    };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::TenantMismatch { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 3, "nothing typed on the enrollment screen");
}

const MANUAL: &str = r#"
workflow_format = 1
name = "manual"
start = ["REGION"]
match = "luma"

[[states]]
name = "REGION"
detect = { screen = "region.png" }
manual = "Connect the network cable, then click Next on the target"
next = ["DONE"]
timeout = "3s"
derive = { network = "connected by hand" }

[[states]]
name = "DONE"
detect = { screen = "done.png" }
terminal = true
"#;

#[test]
fn a_manual_state_types_nothing_and_waits_for_the_operator() {
    let w = Workflow::parse(MANUAL).unwrap();
    // The operator acts on the target: the screen moves on by itself after a while.
    let timeline: Vec<Frame> = std::iter::repeat_n(solid(20), 8).chain(std::iter::repeat_n(solid(225), 20)).collect();
    let mut sim = Sim { timeline: Some(timeline), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()), "{:?}", r.trace);
    assert_eq!(sim.keys, ["release_all"], "nothing typed, only the run's own cleanup");
    assert!(sim.asked.is_empty(), "a manual step is not a question: the screen moving on is the answer");
    let ev = events(&r);
    assert!(ev.iter().any(|e| matches!(e, WorkflowEvent::ManualStepRequested { state, timeout_ms: 3000, .. } if state == "REGION")));
    assert!(!ev.iter().any(|e| matches!(e, WorkflowEvent::ActionAttempted { .. })));
    assert_eq!(r.context.derived["network"].0, "connected by hand", "a verified transition records its facts");
    // Nothing was sent, so a stop in a manual state never asks to "send again" on resume.
    assert_eq!(r.context.history[0].from, "REGION");
}

#[test]
fn a_manual_step_that_never_happens_times_out_without_typing() {
    let w = Workflow::parse(MANUAL).unwrap();
    let mut sim = Sim { timeline: Some(vec![solid(20)]), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::TimedOut { state, .. }) if state == "REGION"), "{:?}", r.result);
    assert_eq!(sim.keys, ["release_all"]);
    let cp = r.checkpoint(&w).unwrap();
    assert_eq!(cp.attempted, Some(0), "a manual step sends nothing, so a resume has nothing to replay");
}

#[test]
fn manual_states_cannot_type_ask_first_or_retry() {
    let bad = |from: &str, to: &str| {
        assert!(MANUAL.contains(from), "{from}");
        Workflow::parse(&MANUAL.replace(from, to)).unwrap_err()
    };
    let with = "timeout = \"3s\"\nderive";
    assert!(bad(with, "actions = [ { key = \"ENTER\" } ]\ntimeout = \"3s\"\nderive").contains("types nothing"));
    assert!(bad(with, "gate = \"sure?\"\ntimeout = \"3s\"\nderive").contains("types nothing"));
    assert!(bad(with, "retry_safe = true\nretries = 1\ntimeout = \"3s\"\nderive").contains("types nothing"));
    assert!(bad("manual = \"Connect the network cable, then click Next on the target\"", "manual = \"  \"").contains("must say"));
}

#[test]
fn a_secret_in_a_manual_message_is_refused() {
    let src = MANUAL.replace("start = [\"REGION\"]", "start = [\"REGION\"]\n[secrets]\nwifi = { prompt = \"Wi-Fi\" }\n")
        .replace("Connect the network cable", "Type hunter2-wifi");
    // [secrets] must come after the top-level keys: move it below match.
    let src = src.replace("[secrets]\nwifi = { prompt = \"Wi-Fi\" }\n\nmatch = \"luma\"", "match = \"luma\"\n[secrets]\nwifi = { prompt = \"Wi-Fi\" }");
    let w = Workflow::parse(&src).unwrap();
    let mut sim = Sim { timeline: Some(vec![solid(20)]), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::SecretInFile { .. })), "{:?}", r.result);
}

#[test]
fn a_manual_step_done_before_the_engine_looks_again_still_counts() {
    let w = Workflow::parse(MANUAL).unwrap();
    // The operator was quick: the screen moves on right after it was recognised, before any further look.
    let timeline: Vec<Frame> = std::iter::repeat_n(solid(20), 2).chain(std::iter::repeat_n(solid(225), 30)).collect();
    let mut sim = Sim { timeline: Some(timeline), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()), "{:?}", r.trace);
    assert_eq!(r.context.history[0].to, "DONE");
}

#[test]
fn waiting_in_a_state_that_types_nothing_is_never_a_send_to_replay() {
    let src = MANUAL.replace("manual = \"Connect the network cable, then click Next on the target\"\n", "");
    let w = Workflow::parse(&src).unwrap();
    let mut sim = Sim { timeline: Some(vec![solid(20)]), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::TimedOut { .. })), "{:?}", r.result);
    assert_eq!(r.checkpoint(&w).unwrap().attempted, Some(0), "nothing was sent: a resume asks nothing");
}

#[test]
fn resuming_after_a_manual_step_finished_while_stopped_keeps_its_facts() {
    let w = Workflow::parse(MANUAL).unwrap();
    // The manual step timed out; the operator then did it, and the target now shows DONE.
    let mut sim = Sim { timeline: Some(vec![solid(20)]), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    let cp = r.checkpoint(&w).unwrap();
    assert!(cp.waiting && cp.attempted == Some(0), "{cp:?}");
    let cp = Checkpoint::from_json(&cp.to_json()).unwrap();
    let mut sim = Sim { timeline: Some(vec![solid(225)]), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Ok(()), "{:?}", r.trace);
    assert_eq!(r.context.derived["network"].0, "connected by hand", "the pending transition is reconciled with its facts");
    assert_eq!(sim.keys, ["release_all"], "and nothing is sent");
    // An older checkpoint without the field reads as not waiting (nothing to reconcile), as before.
    let old = cp.to_json().replace("\"waiting\": true,", "");
    assert!(!Checkpoint::from_json(&old).unwrap().waiting);
}

#[test]
fn an_interrupted_resume_keeps_a_pending_wait() {
    let w = Workflow::parse(MANUAL).unwrap();
    let mut sim = Sim { timeline: Some(vec![solid(20)]), screens: screens(), ..Default::default() };
    let cp = go(&w, &mut sim, &opts(), None).checkpoint(&w).unwrap();
    assert!(cp.waiting);
    // Resumed on the same screen, then stopped again before the step was done: the wait is still pending.
    let mut sim = Sim { timeline: Some(vec![solid(20)]), screens: screens(), ..Default::default() };
    let again = go(&w, &mut sim, &opts(), Some(&cp)).checkpoint(&w).unwrap();
    assert!(again.waiting, "{again:?}");
}

#[test]
fn the_secret_check_reads_decoded_fields() {
    // A TOML unicode escape (backslash, u, 0073) decodes to `s`: the file's text does not contain the secret, its name does.
    let src = MANUAL.replace("name = \"manual\"", "name = \"pa\\u0073s-word\"").replace(
        "match = \"luma\"",
        "match = \"luma\"\n[secrets]\nwifi = { prompt = \"Wi-Fi\" }",
    );
    let w = Workflow::parse(&src).unwrap();
    let s: Vars = [("wifi".to_string(), "pass-word".to_string())].into();
    assert!(!src.contains("pass-word"));
    assert!(matches!(check_secrets(&w, &s, None, ""), Err(WorkflowError::SecretInFile { .. })));
    let s: Vars = [("wifi".to_string(), "other".to_string())].into();
    assert_eq!(check_secrets(&w, &s, None, ""), Ok(()));
}

#[test]
fn a_missing_picture_makes_only_its_state_unrecognised() {
    // The keyboard screen's picture is not saved: REGION is done, then that screen is unknown and nothing is typed on it.
    // Refused unless allowed: such a screen may be taken for a similar state.
    let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec!["keyboard.png"], ..Default::default() };
    let r = go(&wf(), &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Reference(m)) if m.contains("KEYBOARD")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing sent");
    let allow = WorkflowOptions { allow_missing_pictures: true, ..opts() };
    let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec!["keyboard.png"], ..Default::default() };
    let r = go(&wf(), &mut sim, &allow, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::PicturesMissing { states, stops } if states == &["KEYBOARD"] && stops.is_empty())));
    assert!(r.context.history.is_empty(), "REGION's move cannot be verified onto a screen nobody can recognise");
    let sent = events(&r).iter().filter(|e| matches!(e, WorkflowEvent::ActionAttempted { .. })).count();
    assert_eq!(sent, 1, "keys went to REGION only");

    // A dry run sends nothing: it runs with pictures missing, unasked.
    let dry = WorkflowOptions { run: RunOptions { dry_run: true, ..opts().run }, ..opts() };
    let mut sim = Sim { timeline: Some(screens()), missing: vec!["keyboard.png"], ..Default::default() };
    let r = go(&wf(), &mut sim, &dry, None);
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::PicturesMissing { .. })), "{:?}", r.result);
}

#[test]
fn a_missing_stop_screen_is_named_before_it_is_accepted() {
    // A work-account profile stops at AUTOPILOT (its guard wants autopilot): without its picture that stop is lost, and
    // the refusal says so.
    let w = Workflow::parse(&OOBE.replace("deployment_mode = \"autopilot\"\ntenant", "deployment_mode = \"work_account\"\ntenant")).unwrap();
    assert_eq!(w.stop_states(&["AUTOPILOT".to_string(), "KEYBOARD".to_string()]), ["AUTOPILOT"]);
    let mut sim = Sim { screens: screens(), missing: vec!["autopilot.png"], ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Reference(m)) if m.contains("stops the run at AUTOPILOT")), "{:?}", r.result);
    let allow = WorkflowOptions { allow_missing_pictures: true, ..opts() };
    let mut sim = Sim { screens: screens(), missing: vec!["autopilot.png"], ..Default::default() };
    let r = go(&w, &mut sim, &allow, None);
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::PicturesMissing { stops, .. } if stops == &["AUTOPILOT"])));
}

#[test]
fn the_configured_tenants_picture_or_any_picture_at_all_is_needed_before_starting() {
    // Every allowed tenant's picture: the configured one, and a competitor (without it the wrong page could pass).
    for gone in ["contoso.png", "fabrikam.png"] {
        let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec![gone], ..Default::default() };
        let allow = WorkflowOptions { allow_missing_pictures: true, ..opts() };
        let r = go(&wf(), &mut sim, &allow, None);
        assert!(matches!(&r.result, Err(WorkflowError::Reference(m)) if m.contains(gone)), "{:?}", r.result);
        assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing sent: {:?}", sim.keys);
    }
    let all = vec!["region.png", "keyboard.png", "network.png", "autopilot.png", "done.png"];
    let mut sim = Sim { screens: screens(), missing: all, ..Default::default() };
    let r = go(&wf(), &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Reference(m)) if m.contains("no reference picture")), "{:?}", r.result);
}

// ---------- recognised by text ----------

const BY_TEXT: &str = r#"
workflow_format = 1
name = "by-text"
start = ["REGION"]

[[states]]
name = "REGION"
detect = { text = ["Is this the right country or region?"], region = [0.52, 0.12, 0.40, 0.75] }
next = ["KEYBOARD"]
actions = [ { key = "ENTER" } ]
timeout = "2s"

[[states]]
name = "KEYBOARD"
detect = { text = ["Is this the right keyboard layout or input method?"], region = [0.52, 0.12, 0.40, 0.75] }
next = ["DEVICE_NAME"]
actions = [ { key = "ENTER" } ]
timeout = "2s"

[[states]]
name = "DEVICE_NAME"
detect = { text = ["Name your device", "Make this device yours"] }
terminal = true
"#;

fn oobe_text() -> Vec<(u8, &'static str)> {
    vec![
        (20, "Is this the right country or region?|United States|Afghanistan"),
        // Wrapped onto two lines, as Windows shows it, and one letter misread.
        (70, "Is this the right keybaard layout or input|method?|US"),
        (120, "Name your device|Make this device yours with unique names.|Skip"),
    ]
}

#[test]
fn screens_are_recognised_by_their_text_with_no_pictures_at_all() {
    let w = Workflow::parse(BY_TEXT).expect("a text-only workflow needs no pictures and no `match`");
    assert!(w.states.iter().all(|s| s.detect.screens.is_empty()));
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(oobe_text()), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()));
    let path: Vec<_> = r.context.history.iter().map(|t| (t.from.as_str(), t.to.as_str())).collect();
    assert_eq!(path, [("REGION", "KEYBOARD"), ("KEYBOARD", "DEVICE_NAME")]);
    assert!(!events(&r).iter().any(|e| matches!(e, WorkflowEvent::PicturesMissing { .. })), "nothing is missing");
    let seen = events(&r).iter().find_map(|e| match e {
        WorkflowEvent::StateObserved { observation } if observation.state.as_deref() == Some("KEYBOARD") => Some(observation.clone()),
        _ => None,
    });
    let seen = seen.expect("KEYBOARD observed");
    assert!(seen.confidence > 0.95 && seen.confidence < 1.0, "one misread letter: {}", seen.confidence);
    assert_eq!(seen.reference.as_deref(), Some("text: Is this the right keyboard layout or input method?"));
}

#[test]
fn a_screen_worded_differently_matches_any_one_of_its_phrase_sets() {
    let src = BY_TEXT.replace(
        "detect = { text = [\"Name your device\", \"Make this device yours\"] }",
        "detect = { text = [[\"Checking for updates\"], [\"Please keep your PC on\"]] }",
    );
    let w = Workflow::parse(&src).unwrap();
    assert_eq!(w.states[2].detect.text, [vec!["Checking for updates".to_string()], vec!["Please keep your PC on".to_string()]]);
    let mut text = oobe_text();
    text[2] = (120, "Please keep your PC on");
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(text), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()));
    assert!(Workflow::parse(&BY_TEXT.replacen("text = [\"Name your device\", \"Make this device yours\"]", "text = [[]]", 1))
        .unwrap_err()
        .contains("at least one phrase"));
}

#[test]
fn a_screen_whose_phrases_are_not_all_there_is_unknown_and_nothing_is_typed_on_it() {
    // The device-name screen needs both its phrases: here only the heading is shown.
    let mut text = oobe_text();
    text[2] = (120, "Name your device|Skip");
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(text), ..Default::default() };
    let r = go(&Workflow::parse(BY_TEXT).unwrap(), &mut sim, &opts(), None);
    assert!(r.result.is_err(), "{:?}", r.result);
    assert_ne!(r.context.current.as_deref(), Some("DEVICE_NAME"));
    assert_eq!(enters(&sim), 2, "only REGION and KEYBOARD were acted on");
}

#[test]
fn without_a_text_reader_a_text_only_workflow_says_why_before_sending_anything() {
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: None, ..Default::default() };
    let r = go(&Workflow::parse(BY_TEXT).unwrap(), &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Reference(m)) if m.contains("text cannot be read here (no recogniser)")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing sent");
}

#[test]
fn a_state_with_phrases_needs_no_picture_while_text_can_be_read_and_falls_back_to_it_otherwise() {
    // KEYBOARD has phrases and a picture that is not saved: with a reader it is not missing; without one it is blind.
    let src = OOBE.replace(
        "detect = { screen = \"keyboard.png\" }",
        "detect = { screen = \"keyboard.png\", text = [\"keyboard layout or input method\"] }",
    );
    let w = Workflow::parse(&src).unwrap();
    let text = vec![(70, "Is this the right keyboard layout or input method?")];
    let allow = WorkflowOptions { allow_missing_pictures: true, ..opts() };
    let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec!["keyboard.png"], texts: Some(text), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()), "the keyboard screen was read, not compared");
    let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec!["keyboard.png"], ..Default::default() };
    let r = go(&w, &mut sim, &allow, None);
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::PicturesMissing { states, .. } if states == &["KEYBOARD"])), "{:?}", r.result);
}

#[test]
fn detect_phrases_are_checked_when_the_file_is_read() {
    let bad = |from: &str, to: &str| Workflow::parse(&BY_TEXT.replacen(from, to, 1)).unwrap_err();
    assert!(bad("text = [\"Is this the right country or region?\"]", "text = [\"?!\"]").contains("3 to 200 characters"));
    assert!(bad("text = [\"Is this the right country or region?\"]", "text = []").contains("needs detect text or at least one detect screen"));
    assert!(bad("region = [0.52, 0.12, 0.40, 0.75] }
next = [\"KEYBOARD\"]", "region = [0.52, 0.12, 0.40, 0.75], text_threshold = 0.3 }
next = [\"KEYBOARD\"]")
        .contains("text_threshold"));
    // Pictures still need a comparison method.
    assert!(bad("detect = { text = [\"Name your device\", \"Make this device yours\"] }", "detect = { screen = \"done.png\" }").contains("`match` is required"));
}

// ---------- the tenant check reads the password page ----------

const SIGN_IN: &str = r#"
workflow_format = 1
name = "sign-in"
start = ["WORK_SIGNIN"]
account = "username"

[config]
tenant_domain = "contoso.com"

[secrets]
username = { prompt = "Sign-in" }
password = { prompt = "Password" }

[[states]]
name = "WORK_SIGNIN"
detect = { text = ["Let's set things up for your work or school"] }
next = ["PASSWORD"]
actions = [ { secret_text = "{{username}}" }, { key = "ENTER" } ]
timeout = "2s"

[[states]]
name = "PASSWORD"
detect = { text = ["Enter password"] }
verify_tenant = true
next = ["DONE"]
actions = [ { secret_text = "{{password}}" }, { key = "ENTER" } ]
timeout = "2s"

[[states]]
name = "DONE"
detect = { text = ["Setting up your device for work"] }
terminal = true
"#;

fn sign_in(page: &'static str, user: &str, dry: bool) -> (WorkflowReport, Sim) {
    let w = Workflow::parse(SIGN_IN).unwrap();
    let texts = vec![(20, "Let's set things up for your work or school|Sign in"), (70, page), (120, "Setting up your device for work")];
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(texts), ..Default::default() };
    if dry {
        sim.timeline = Some(sim.screens.iter().flat_map(|f| std::iter::repeat_n(f.clone(), 12)).collect());
    }
    let secrets: Vars = [("username".to_string(), user.to_string()), ("password".to_string(), "Hunter2!pw".to_string())].into();
    let o = WorkflowOptions { run: RunOptions { dry_run: dry, ..opts().run }, ..opts() };
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &o, None, &mut |_, _| {});
    (r, sim)
}

#[test]
fn the_password_is_typed_only_on_a_page_showing_the_sign_in_entered() {
    let (r, sim) = sign_in("\u{2190} Jane@Contoso.com|Enter password|Forgot my password", "jane@contoso.com", false);
    assert_eq!(r.result, Ok(()));
    assert_eq!(enters(&sim), 2, "sign-in and password");
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::TenantVerified { tenant, .. } if tenant == "contoso.com")));
    let trace = trace_jsonl(&r);
    assert!(!trace.contains("jane@contoso.com") && !trace.contains("Hunter2"), "secrets stay out of the trace");
}

#[test]
fn a_page_showing_another_account_or_organisation_stops_before_the_password() {
    for page in ["bob@contoso.com|Enter password", "jane@fabrikam.com|Enter password", "Enter password"] {
        let (r, sim) = sign_in(page, "jane@contoso.com", false);
        assert!(matches!(&r.result, Err(WorkflowError::AccountNotShown { state, domain }) if state == "PASSWORD" && domain == "contoso.com"), "{page}: {:?}", r.result);
        assert_eq!(enters(&sim), 1, "{page}: only the sign-in was sent");
    }
}

#[test]
fn a_sign_in_outside_the_profiles_domain_is_refused_before_anything_is_sent() {
    let (r, sim) = sign_in("jane@fabrikam.com|Enter password", "jane@fabrikam.com", false);
    assert!(matches!(&r.result, Err(WorkflowError::Options(m)) if m.contains("not an account in contoso.com")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"));
    // A dry run has no sign-in: the page needs only to show an account in the domain.
    let (r, _) = sign_in("someone@contoso.com|Enter password", "placeholder", true);
    assert_eq!(r.result, Ok(()), "{:?}", r.result);
    let (r, _) = sign_in("someone@fabrikam.com|Enter password", "placeholder", true);
    assert!(matches!(r.result, Err(WorkflowError::AccountNotShown { .. })), "{:?}", r.result);
}

#[test]
fn without_text_the_tenant_check_needs_a_picture_or_refuses_to_start() {
    let w = Workflow::parse(SIGN_IN).unwrap();
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: None, ..Default::default() };
    let secrets: Vars = [("username".to_string(), "jane@contoso.com".to_string()), ("password".to_string(), "x".to_string())].into();
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(&r.result, Err(WorkflowError::Text(_))), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"));
    for (from, to, why) in [
        ("tenant_domain = \"contoso.com\"", "tenant_domain = \"contoso\"", "must be a domain"),
        ("account = \"username\"\n", "", "needs `account`"),
        ("account = \"username\"", "account = \"nobody\"", "must name a secret"),
    ] {
        let e = Workflow::parse(&SIGN_IN.replacen(from, to, 1)).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
    assert!(valid_domain("mail.contoso.co.uk") && !valid_domain("-bad.com") && !valid_domain("contoso") && !valid_domain("a..b"));
}

// ---------- what was typed is read back before Enter ----------

fn typed_back(echo: Option<&'static str>, dry: bool) -> (WorkflowReport, Sim) {
    let src = SIGN_IN.replace(
        "actions = [ { secret_text = \"{{username}}\" }, { key = \"ENTER\" } ]",
        "actions = [ { secret_text = \"{{username}}\" } ]
verify_typed = \"username\"
submit = [ { key = \"ENTER\" } ]",
    );
    let w = Workflow::parse(&src).unwrap();
    let texts = vec![
        (20, "Let's set things up for your work or school|Sign in"),
        (70, "jane@contoso.com|Enter password"),
        (120, "Setting up your device for work"),
    ];
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(texts), echo, ..Default::default() };
    if dry {
        sim.timeline = Some(sim.screens.iter().flat_map(|f| std::iter::repeat_n(f.clone(), 12)).collect());
    }
    let secrets: Vars = [("username".to_string(), "jane@contoso.com".to_string()), ("password".to_string(), "Hunter2!pw".to_string())].into();
    let o = WorkflowOptions { run: RunOptions { dry_run: dry, ..opts().run }, ..opts() };
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &o, None, &mut |_, _| {});
    (r, sim)
}

#[test]
fn enter_is_pressed_on_the_sign_in_only_once_it_shows_as_typed() {
    let (r, sim) = typed_back(Some("jane@contoso.com"), false);
    assert_eq!(r.result, Ok(()));
    assert_eq!(enters(&sim), 2);
    assert!(events(&r).iter().any(|e| matches!(e, WorkflowEvent::TypedVerified { state, key } if state == "WORK_SIGNIN" && key == "username")));
    assert!(!trace_jsonl(&r).contains("jane@contoso.com"));
}

#[test]
fn caps_lock_or_another_layout_at_the_target_stops_before_enter() {
    for (echo, caps) in [(Some("JANE@CONTOSO.COM"), true), (Some("jane\"contoso.com"), false), (None, false)] {
        let (r, sim) = typed_back(echo, false);
        assert!(matches!(&r.result, Err(WorkflowError::TypedNotShown { state, caps_lock }) if state == "WORK_SIGNIN" && *caps_lock == caps), "{echo:?}: {:?}", r.result);
        assert_eq!(enters(&sim), 0, "{echo:?}: Enter was not pressed");
        let msg = r.result.unwrap_err().to_string();
        assert!(!msg.contains("jane"), "the value is never in the message: {msg}");
    }
    // A dry run typed nothing to read back.
    let (r, _) = typed_back(None, true);
    assert_eq!(r.result, Ok(()), "{:?}", r.result);
}

#[test]
fn where_text_cannot_be_read_a_workflow_that_checks_the_sign_in_does_not_start() {
    // Recognised by pictures, but its sign-in is read back: never sent unchecked. A dry run types nothing and runs.
    let src = OOBE.replace(
        "actions = [ { text = \"{{region}}\" }, { key = \"ENTER\" } ]",
        "actions = [ { text = \"{{region}}\" } ]\nverify_typed = \"region\"\nsubmit = [ { key = \"ENTER\" } ]",
    );
    let w = Workflow::parse(&src).unwrap();
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(
        matches!(&r.result, Err(WorkflowError::Text(m)) if m.contains("checks the sign-in by reading the screen") && m.contains("no recogniser")),
        "{:?}",
        r.result
    );
    assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing sent");
    let dry = WorkflowOptions { run: RunOptions { dry_run: true, ..opts().run }, ..opts() };
    let timeline: Vec<Frame> = [20, 70, 120, 170, 225].into_iter().flat_map(|v| std::iter::repeat_n(solid(v), 12)).collect();
    let mut sim = Sim { timeline: Some(timeline), screens: screens(), ..Default::default() };
    assert_eq!(go(&w, &mut sim, &dry, None).result, Ok(()));
    // The same for the account check: the SIGN_IN workflow without a reader.
    let w = Workflow::parse(SIGN_IN).unwrap();
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), ..Default::default() };
    let secrets: Vars = [("username".to_string(), "jane@contoso.com".to_string()), ("password".to_string(), "x".to_string())].into();
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(&r.result, Err(WorkflowError::Text(m)) if m.contains("checks the sign-in")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"));
}

#[test]
fn a_near_miss_account_or_a_sign_in_with_control_characters_is_refused() {
    // Another organisation's account that only looks close (contoso.com for contoso.co) never passes.
    let w = Workflow::parse(&SIGN_IN.replace("contoso.com\"", "contoso.co\"")).unwrap();
    let texts = vec![
        (20, "Let's set things up for your work or school|Sign in"),
        (70, "alexander.smith@contoso.com|Enter password"),
        (120, "Setting up your device for work"),
    ];
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(texts), ..Default::default() };
    let secrets: Vars =
        [("username".to_string(), "alexander.smith@contoso.co".to_string()), ("password".to_string(), "pw".to_string())].into();
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(r.result, Err(WorkflowError::AccountNotShown { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 1, "the password was not typed");
    // A newline would be typed as Enter before any check; a tab, a space or a second @ is not a sign-in either.
    for bad in ["jane@contoso.com\n", "jane@contoso.com\t", "jane doe@contoso.com", "jane@x@contoso.com", "@contoso.com"] {
        let (r, sim) = sign_in("jane@contoso.com|Enter password", bad, false);
        assert!(matches!(&r.result, Err(WorkflowError::Options(m)) if m.contains("not an account in contoso.com")), "{bad:?}: {:?}", r.result);
        assert!(sim.keys.iter().all(|k| k == "release_all"), "{bad:?}");
    }
    // Punctuation in the domain is read as such.
    let w = Workflow::parse(&SIGN_IN.replace("contoso.com\"", "contoso-it.com\"")).unwrap();
    let texts = vec![
        (20, "Let's set things up for your work or school|Sign in"),
        (70, "\u{2190} jane@contoso-it.com|Enter password"),
        (120, "Setting up your device for work"),
    ];
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(texts), ..Default::default() };
    let secrets: Vars = [("username".to_string(), "jane@contoso-it.com".to_string()), ("password".to_string(), "pw".to_string())].into();
    assert_eq!(run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {}).result, Ok(()));
}

#[test]
fn a_detect_phrase_holding_a_secret_is_refused_before_anything_is_shown() {
    let src = SIGN_IN.replace("detect = { text = [\"Enter password\"] }", "detect = { text = [\"Enter password\", \"jane@contoso.com\"] }");
    let w = Workflow::parse(&src).unwrap();
    let mut sim = Sim { screens: [20, 70, 120].into_iter().map(solid).collect(), texts: Some(vec![]), ..Default::default() };
    let secrets: Vars = [("username".to_string(), "jane@contoso.com".to_string()), ("password".to_string(), "pw".to_string())].into();
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(r.result, Err(WorkflowError::SecretInFile { .. })), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"));
}

#[test]
fn verify_typed_needs_its_actions_and_submit() {
    let base = SIGN_IN.replace(
        "actions = [ { secret_text = \"{{username}}\" }, { key = \"ENTER\" } ]",
        "actions = [ { secret_text = \"{{username}}\" } ]
verify_typed = \"username\"
submit = [ { key = \"ENTER\" } ]",
    );
    assert!(Workflow::parse(&base).is_ok());
    for (from, to, why) in [
        ("verify_typed = \"username\"", "verify_typed = \"nobody\"", "must name a secret or a config value"),
        ("submit = [ { key = \"ENTER\" } ]", "", "needs `actions`"),
        ("verify_typed = \"username\"
", "", "`submit` is sent after verify_typed"),
    ] {
        let e = Workflow::parse(&base.replacen(from, to, 1)).unwrap_err();
        assert!(e.contains(why), "{e}");
    }
}

// ---------- round 69: evidence age, the reader probe, control characters ----------

#[test]
fn keys_wait_for_recent_evidence_and_a_slow_reader_stops_the_run_before_typing() {
    let w = Workflow::parse(SIGN_IN).unwrap();
    let texts = vec![(20, "Let's set things up for your work or school|Sign in"), (70, "jane@contoso.com|Enter password")];
    let mut sim = Sim { screens: [20, 70].into_iter().map(solid).collect(), texts: Some(texts), read_delay: Duration::from_millis(120), ..Default::default() };
    let secrets: Vars = [("username".to_string(), "jane@contoso.com".to_string()), ("password".to_string(), "pw".to_string())].into();
    let o = WorkflowOptions { evidence_age: Duration::from_millis(50), ..opts() };
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &o, None, &mut |_, _| {});
    assert!(matches!(&r.result, Err(WorkflowError::Text(m)) if m.contains("ms old by the time it was read")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing typed on old evidence");
    // The option is bounded: it cannot waive the check.
    for age in [Duration::ZERO, Duration::from_secs(31)] {
        let o = WorkflowOptions { evidence_age: age, ..opts() };
        let mut sim = Sim { screens: screens(), ..Default::default() };
        assert!(matches!(go(&wf(), &mut sim, &o, None).result, Err(WorkflowError::Options(_))));
    }
}

#[test]
fn a_picture_recognised_workflow_whose_tenant_check_reads_the_page_does_not_start_without_a_reader() {
    // Screens by picture, but a sign-in domain: the account check reads the password page, so the reader is probed.
    let src = OOBE
        .replace("match = \"luma\"\n", "match = \"luma\"\naccount = \"wifi\"\n")
        .replace("[config]\n", "[config]\ntenant_domain = \"contoso.com\"\n");
    let w = Workflow::parse(&src).unwrap();
    let secrets: Vars = [("wifi".to_string(), "a@contoso.com".to_string())].into();
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
    assert!(matches!(&r.result, Err(WorkflowError::Text(m)) if m.contains("checks the sign-in")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing sent, the sign-in included");
}

#[test]
fn a_read_back_value_with_a_line_break_is_refused_before_anything_is_sent() {
    let src = OOBE.replace(
        "actions = [ { secret_text = \"{{wifi}}\" }, { key = \"ENTER\" } ]",
        "actions = [ { secret_text = \"{{wifi}}\" } ]\nverify_typed = \"wifi\"\nsubmit = [ { key = \"ENTER\" } ]",
    );
    let w = Workflow::parse(&src).unwrap();
    for bad in ["hunter2\n", "hun\tter2"] {
        let secrets: Vars = [("wifi".to_string(), bad.to_string())].into();
        let mut sim = Sim { screens: screens(), texts: Some(vec![]), ..Default::default() };
        let r = run_workflow(&w, &secrets, &UsAnsi, &mut sim, &opts(), None, &mut |_, _| {});
        assert!(matches!(&r.result, Err(WorkflowError::Options(m)) if m.contains("control character")), "{bad:?}: {:?}", r.result);
        assert!(sim.keys.iter().all(|k| k == "release_all"));
    }
}

#[test]
fn accepting_missing_pictures_covers_only_the_states_named_when_it_was_accepted() {
    // Review round 67: the app names KEYBOARD when the box is ticked; by the time the run starts AUTOPILOT's picture is
    // gone too. That one was never accepted (and the profile's stop there would be lost): refused.
    let allow = WorkflowOptions {
        allow_missing_pictures: true,
        allow_missing_only: Some(["KEYBOARD".to_string()].into()),
        ..opts()
    };
    let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec!["keyboard.png", "autopilot.png"], ..Default::default() };
    let r = go(&wf(), &mut sim, &allow, None);
    assert!(matches!(&r.result, Err(WorkflowError::Reference(m)) if m.contains("AUTOPILOT also cannot be recognised")), "{:?}", r.result);
    assert!(sim.keys.iter().all(|k| k == "release_all"), "nothing sent");
    // Exactly what was accepted still runs (to the unknown keyboard screen).
    let mut sim = Sim { screens: screens(), confirms: [true].into(), missing: vec!["keyboard.png"], ..Default::default() };
    let r = go(&wf(), &mut sim, &allow, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
}

#[test]
fn every_burst_of_keys_is_recorded_before_it_is_sent_and_a_failed_record_sends_nothing() {
    let w = wf();
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()));
    // Every state entered, then every attempt, recorded before its keys: REGION's attempt with nothing typed yet.
    let region: Vec<_> = sim.persisted.iter().filter(|(cp, _)| cp.state == "REGION").collect();
    assert_eq!(region.iter().map(|(cp, typed)| (cp.attempted, *typed)).collect::<Vec<_>>(), [(Some(0), 0), (Some(1), 0)]);
    let typed_before_network = sim.persisted.iter().find(|(cp, _)| cp.state == "NETWORK" && cp.attempted == Some(1)).unwrap().1;
    assert!(typed_before_network > 0 && typed_before_network < sim.typed, "recorded after REGION's keys, before NETWORK's");
    // Recording the first attempt fails (the second record): nothing at all is sent.
    let mut sim = Sim { screens: screens(), fail_persist_at: Some(2), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert!(matches!(&r.result, Err(WorkflowError::Journal(m)) if m == "disk full"), "{:?}", r.result);
    assert_eq!(sim.typed, 0);
    assert!(r.result.unwrap_err().to_string().contains("stopped before sending anything more"));
    // A dry run records nothing.
    let mut sim = Sim { timeline: Some(screens().into_iter().flat_map(|f| std::iter::repeat_n(f, 12)).collect()), screens: screens(), ..Default::default() };
    let o = WorkflowOptions { run: RunOptions { dry_run: true, ..opts().run }, ..opts() };
    let _ = go(&w, &mut sim, &o, None);
    assert!(sim.persisted.is_empty());
}

#[test]
fn a_resumed_visit_is_recorded_with_what_it_already_sent_before_anything_is_asked() {
    let w = wf();
    let cp = Checkpoint {
        workflow: "test-oobe".into(),
        state: "REGION".into(),
        derived: BTreeMap::new(),
        attempts: [("REGION".to_string(), 1)].into(),
        attempted: Some(1),
        waiting: false,
        history: vec![],
    };
    // The run crashes while "Send them again?" is open: what it recorded must still say the keys were sent.
    let mut sim = Sim { screens: screens(), pos: 0, confirms: [false].into(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), Some(&cp));
    assert_eq!(r.result, Err(WorkflowError::ResumeDeclined { state: "REGION".into() }));
    assert_eq!(sim.asked.len(), 1);
    assert!(!sim.persisted.is_empty());
    assert!(sim.persisted.iter().all(|(c, _)| c.state == "REGION" && c.attempted == Some(1)), "{:?}", sim.persisted);
}

#[test]
fn a_manual_steps_wait_is_recorded_before_the_operator_does_it() {
    let w = Workflow::parse(MANUAL).unwrap();
    let timeline: Vec<Frame> = std::iter::repeat_n(solid(20), 8).chain(std::iter::repeat_n(solid(225), 20)).collect();
    let mut sim = Sim { timeline: Some(timeline), screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Ok(()));
    let region: Vec<bool> = sim.persisted.iter().filter(|(c, _)| c.state == "REGION").map(|(c, _)| c.waiting).collect();
    assert_eq!(region, [false, true], "entered, then waiting before the step is handed to the operator");
    let at = |pred: &dyn Fn(&WorkflowEvent) -> bool| r.trace.iter().position(|t| pred(&t.event)).unwrap();
    assert!(at(&|e| matches!(e, WorkflowEvent::ManualStepRequested { .. })) < at(&|e| matches!(e, WorkflowEvent::TransitionVerified { .. })));
}

#[test]
fn a_stall_after_the_last_look_stops_before_the_keys() {
    // The trace sink blocks (a slow terminal, a full pipe) right after the tenant check that ends the last look
    // before AUTOPILOT's Enter: by the time the keys would go, the picture is older than the evidence allows.
    let w = wf();
    let mut sim = Sim { screens: screens(), confirms: [true].into(), ..Default::default() };
    let o = WorkflowOptions { evidence_age: Duration::from_millis(150), ..opts() };
    let mut seen = 0;
    let r = run_workflow(&w, &secrets(), &UsAnsi, &mut sim, &o, None, &mut |t, _| {
        if matches!(t.event, WorkflowEvent::TenantVerified { .. }) {
            seen += 1;
            // The first check is before the gate; the second ends the last look.
            if seen == 2 {
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    });
    assert!(matches!(&r.result, Err(WorkflowError::Text(m)) if m.contains("ms old")), "{:?}", r.result);
    assert_eq!(enters(&sim), 3, "nothing sent on the enrolment screen");
}

#[test]
fn an_address_inside_another_word_is_not_the_account_shown() {
    for page in ["other!jane@contoso.com|Enter password", "-jane@contoso.com|Enter password", "x'jane@contoso.com|Enter password"] {
        let (r, sim) = sign_in(page, "jane@contoso.com", false);
        assert!(matches!(&r.result, Err(WorkflowError::AccountNotShown { .. })), "{page}: {:?}", r.result);
        assert_eq!(enters(&sim), 1, "{page}: no password");
    }
    for page in ["(jane@contoso.com)|Enter password", "Signed in as jane@contoso.com.|Enter password"] {
        let (r, _) = sign_in(page, "jane@contoso.com", false);
        assert_eq!(r.result, Ok(()), "{page}");
    }
    // An entered sign-in that could never be read as one address is refused before anything is sent.
    let (r, sim) = sign_in("jane@contoso.com|Enter password", "jane!x@contoso.com", false);
    assert!(matches!(&r.result, Err(WorkflowError::Options(_))), "{:?}", r.result);
    assert_eq!(sim.typed, 0);
}

#[test]
fn a_laptop_name_holding_a_secret_is_refused_before_anything() {
    let w = wf();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let o = WorkflowOptions { laptop: "rig hunter2-wifi".into(), ..opts() };
    let r = go(&w, &mut sim, &o, None);
    assert_eq!(r.result, Err(WorkflowError::SecretInName));
    assert_eq!(sim.typed, 0);
    assert!(sim.persisted.is_empty());
    assert!(!trace_jsonl(&r).contains("hunter2"));
    assert_eq!(check_secrets(&w, &secrets(), None, "hunter2-wifi"), Err(WorkflowError::SecretInName));
    assert_eq!(check_secrets(&w, &secrets(), None, "LT-0042"), Ok(()));
}
