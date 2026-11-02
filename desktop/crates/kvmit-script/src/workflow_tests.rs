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
    frozen_queued: Option<Frame>,
    events: Vec<crate::exec::RunEvent>,
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
        let f = self.screens.get(self.pos).cloned().map(|f| f.with_seq(seq));
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
    fn confirm(&mut self, message: &str) -> bool {
        self.asked.push(message.to_string());
        if let Some((at, f)) = self.during_confirm.take() {
            for _ in 0..self.queue_on_confirm {
                self.queued.push_back(self.screens[at].clone());
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
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "state AUTOPILOT".into() }));
    assert_eq!(sim.typed, 0);
    // The same for a recorded fact, which would otherwise sit in the context, the inspector and checkpoints.
    let leaky = OOBE.replace("derive = { region_set = \"yes\" }", "derive = { region_set = \"hunter2-wifi\" }");
    let w = Workflow::parse(&leaky).unwrap();
    let mut sim = Sim { screens: screens(), ..Default::default() };
    let r = go(&w, &mut sim, &opts(), None);
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "state REGION".into() }));
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
        ..Default::default()
    };
    let r = go(&w, &mut sim, &o, None);
    assert!(matches!(r.result, Err(WorkflowError::UnknownScreen { .. })), "{:?}", r.result);
    assert_eq!(enters(&sim), 3, "nothing typed on the enrollment screen");
}

#[test]
fn the_pause_budget_counts_default_delays_and_key_timing() {
    let w = wf();
    // REGION has two steps: a 1.5 s default delay after each makes 3 s of pauses.
    let mut o = opts();
    o.run.default_delay = Duration::from_millis(1500);
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
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "name".into() }));
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
    assert_eq!(r.result, Err(WorkflowError::SecretInFile { place: "state KEYBOARD".into() }));
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
