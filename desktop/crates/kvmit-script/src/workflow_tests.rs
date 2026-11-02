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
            self.enters_seen += 1;
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
        if let Some(t) = &self.timeline {
            return t.get((self.looks - 1).min(t.len() - 1)).cloned();
        }
        self.screens.get(self.pos).cloned()
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
    fn event(&mut self, _: crate::exec::RunEvent) {}
}

const OOBE: &str = r#"
workflow_format = 1
name = "test-oobe"
start = ["REGION"]

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
    let timeline: Vec<Frame> = [20, 20, 70, 70, 120, 120, 170, 170, 225, 225].into_iter().map(solid).collect();
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
}
