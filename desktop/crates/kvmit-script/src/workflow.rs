//! Stateful workflows (issue #46): named screens, verified transitions, and a trace of everything that happened.
//!
//! A linear script types and hopes. A workflow knows which screen it is on: each state names the reference screens
//! that identify it, what to type there, and which states may follow. The engine loops
//! observe → classify → validate → act → verify → transition, and **never acts on a screen it cannot classify with
//! confidence**: an unknown, ambiguous or unexpected screen stops the run (after an optional human check), it does not
//! type blindly. A HID acknowledgement is never taken as success; only the next screen is.
//!
//! The run context keeps separate namespaces: `config` (the approved profile, immutable), `observed` (what the
//! capture card showed, with confidence and evidence), `derived` (facts set only when a transition was verified),
//! `runtime` (current state, attempts, history) and `secrets` (prompted at run time, used only inside compiled key
//! strokes, never serialized, never in events, exports or checkpoints).
//!
//! Firmware is untouched: a workflow is compiled into the same [`Op`]s a script uses and run through the same
//! [`Host`]. Nothing here touches files, the network or a shell.

use crate::exec::{compile, run, Host, Op, RunError, RunOptions, Vars};
use crate::frame::Frame;
use crate::model::{parse_duration, step_from_raw, RawStep, Script, Step, VarDef};
use kvmit_layout::Layout;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

pub const WORKFLOW_FORMAT: u32 = 1;

// ---------- definition ----------

/// How a state is recognised on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Detect {
    /// Reference images (relative to the workflow file); the best match counts.
    pub screens: Vec<String>,
    /// Minimum similarity to count as this state.
    pub threshold: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateDef {
    pub name: String,
    pub description: String,
    pub detect: Detect,
    /// States this one may lead to. A verified transition must land on one of these.
    pub next: Vec<String>,
    /// Entry guard: each key must equal this value in `config` or `derived`, or the run stops.
    pub require: BTreeMap<String, String>,
    /// Ask the operator before acting (the message is shown as written; it must not contain secrets).
    pub gate: Option<String>,
    /// Before acting, the screen must show the configured tenant's branding (and not another allowed tenant's).
    pub verify_tenant: bool,
    pub actions: Vec<Step>,
    /// How long to wait, after acting, for the screen to move on.
    pub timeout: Duration,
    /// Re-run the actions this many times if the screen did not move on. Only honoured with `retry_safe`.
    pub retries: u32,
    /// Typing the actions again is harmless (e.g. pressing Enter on a "Next" screen).
    pub retry_safe: bool,
    /// Facts recorded in `derived` once the transition *out of* this state is verified.
    pub derive: BTreeMap<String, String>,
    /// Reaching this state completes the run.
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Workflow {
    pub name: String,
    pub description: String,
    /// States the run may begin in.
    pub start: Vec<String>,
    /// The approved deployment profile. Immutable during a run.
    pub config: BTreeMap<String, String>,
    /// Secrets prompted at run time (name → prompt). Never given defaults, never stored.
    pub secrets: BTreeMap<String, String>,
    /// Allowed tenants and the reference image of each one's sign-in branding. `config.tenant` must be one of them.
    pub tenants: BTreeMap<String, String>,
    /// A classification must beat the runner-up by at least this much similarity.
    pub ambiguity_margin: f64,
    pub states: Vec<StateDef>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDetect {
    #[serde(default)]
    screen: Option<String>,
    #[serde(default)]
    screens: Vec<String>,
    #[serde(default)]
    threshold: Option<f64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawState {
    name: String,
    #[serde(default)]
    description: String,
    detect: RawDetect,
    #[serde(default)]
    next: Vec<String>,
    #[serde(default)]
    require: BTreeMap<String, String>,
    #[serde(default)]
    gate: Option<String>,
    #[serde(default)]
    verify_tenant: bool,
    #[serde(default)]
    actions: Vec<RawStep>,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    retries: u32,
    #[serde(default)]
    retry_safe: bool,
    #[serde(default)]
    derive: BTreeMap<String, String>,
    #[serde(default)]
    terminal: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecret {
    prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawWorkflow {
    workflow_format: u32,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    start: Vec<String>,
    #[serde(default)]
    config: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, RawSecret>,
    #[serde(default)]
    tenants: BTreeMap<String, String>,
    #[serde(default)]
    ambiguity_margin: Option<f64>,
    states: Vec<RawState>,
}

const MAX_STATES: usize = 64;
const MAX_RETRIES: u32 = 5;

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Workflow {
    pub fn parse(toml_text: &str) -> Result<Workflow, String> {
        let raw: RawWorkflow = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        if raw.workflow_format != WORKFLOW_FORMAT {
            return Err(format!(
                "unsupported workflow format {} (this build reads workflow_format {WORKFLOW_FORMAT})",
                raw.workflow_format
            ));
        }
        if raw.states.is_empty() || raw.states.len() > MAX_STATES {
            return Err(format!("a workflow needs 1..={MAX_STATES} states"));
        }
        let margin = raw.ambiguity_margin.unwrap_or(0.03);
        if !(0.0..=0.5).contains(&margin) {
            return Err("ambiguity_margin must be within 0..=0.5".into());
        }
        for k in raw.config.keys().chain(raw.secrets.keys()) {
            if !valid_name(k) {
                return Err(format!("bad variable name {k:?} (letters, digits and _ only)"));
            }
        }
        if let Some(k) = raw.config.keys().find(|k| raw.secrets.contains_key(*k)) {
            return Err(format!("{k:?} is both a config value and a secret"));
        }
        if !raw.tenants.is_empty() {
            match raw.config.get("tenant") {
                Some(t) if raw.tenants.contains_key(t) => {}
                Some(t) => return Err(format!("config tenant {t:?} is not in the workflow's allowed tenants")),
                None => return Err("the workflow lists tenants but config has no `tenant`".into()),
            }
        }
        let mut states = Vec::new();
        let mut names = BTreeSet::new();
        for r in raw.states {
            if !valid_name(&r.name) {
                return Err(format!("bad state name {:?}", r.name));
            }
            if !names.insert(r.name.clone()) {
                return Err(format!("state {:?} is defined twice", r.name));
            }
            let mut screens = r.detect.screens;
            screens.extend(r.detect.screen);
            if screens.is_empty() {
                return Err(format!("state {:?} needs at least one detect screen", r.name));
            }
            let threshold = r.detect.threshold.unwrap_or(0.92);
            if !(0.5..=1.0).contains(&threshold) {
                return Err(format!("state {:?}: threshold must be within 0.5..=1", r.name));
            }
            if r.retries > MAX_RETRIES {
                return Err(format!("state {:?}: at most {MAX_RETRIES} retries", r.name));
            }
            if r.retries > 0 && !r.retry_safe {
                return Err(format!(
                    "state {:?}: retries need `retry_safe = true` (typing the actions twice must be harmless)",
                    r.name
                ));
            }
            if r.terminal && !r.next.is_empty() {
                return Err(format!("state {:?} is terminal and cannot have `next` states", r.name));
            }
            if !r.terminal && r.next.is_empty() {
                return Err(format!("state {:?} needs `next` states or `terminal = true`", r.name));
            }
            if r.verify_tenant && raw.tenants.is_empty() {
                return Err(format!("state {:?} verifies the tenant but the workflow lists no tenants", r.name));
            }
            for k in r.derive.keys() {
                if !valid_name(k) || raw.config.contains_key(k) || raw.secrets.contains_key(k) {
                    return Err(format!("state {:?}: bad derived name {k:?}", r.name));
                }
            }
            if r.require.keys().any(|k| raw.secrets.contains_key(k)) {
                return Err(format!("state {:?}: a guard cannot compare a secret", r.name));
            }
            states.push(StateDef {
                timeout: parse_duration(r.timeout.as_deref().unwrap_or("60s"))?,
                actions: r.actions.into_iter().map(step_from_raw).collect::<Result<_, _>>()?,
                name: r.name,
                description: r.description,
                detect: Detect { screens, threshold },
                next: r.next,
                require: r.require,
                gate: r.gate,
                verify_tenant: r.verify_tenant,
                retries: r.retries,
                retry_safe: r.retry_safe,
                derive: r.derive,
                terminal: r.terminal,
            });
        }
        for s in &states {
            if let Some(n) = s.next.iter().chain(&raw.start).find(|n| !names.contains(*n)) {
                return Err(format!("unknown state {n:?}"));
            }
        }
        if raw.start.is_empty() {
            return Err("`start` must name at least one state".into());
        }
        Ok(Workflow {
            name: raw.name,
            description: raw.description,
            start: raw.start,
            config: raw.config,
            secrets: raw.secrets.into_iter().map(|(k, v)| (k, v.prompt)).collect(),
            tenants: raw.tenants,
            ambiguity_margin: margin,
            states,
        })
    }

    pub fn state(&self, name: &str) -> Option<&StateDef> {
        self.states.iter().find(|s| s.name == name)
    }
}

// ---------- context ----------

/// Where a value came from, for the variable inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// From the approved profile.
    Configured,
    /// Read off the screen.
    Observed,
    /// Recorded when a transition was verified.
    Verified,
    /// The engine's own bookkeeping.
    Runtime,
}

/// The latest screen classification, with its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Observation {
    /// `None`: no state matched with enough confidence and margin.
    pub state: Option<String>,
    pub confidence: f64,
    /// The runner-up and its similarity, when there was one.
    pub runner_up: Option<(String, f64)>,
    /// The reference image that matched best.
    pub reference: Option<String>,
    /// A short hash of the captured frame's comparison thumbnail (evidence, not the image).
    pub frame_hash: String,
    pub at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub from: String,
    pub to: String,
    pub at_ms: u64,
    pub attempts: u32,
    pub confidence: f64,
}

/// Everything the run knows, except secrets.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    pub config: BTreeMap<String, String>,
    pub observed: Option<Observation>,
    pub derived: BTreeMap<String, (String, u64)>,
    pub current: Option<String>,
    pub previous: Option<String>,
    pub attempts: BTreeMap<String, u32>,
    pub history: Vec<Transition>,
}

/// One row of the variable inspector.
#[derive(Debug, Clone, PartialEq)]
pub struct VarView {
    pub namespace: &'static str,
    pub key: String,
    pub value: String,
    pub provenance: Provenance,
    pub changed_at_ms: Option<u64>,
}

impl Context {
    /// Every non-secret value, with its provenance.
    pub fn inspect(&self) -> Vec<VarView> {
        let mut v: Vec<VarView> = self
            .config
            .iter()
            .map(|(k, val)| VarView {
                namespace: "config",
                key: k.clone(),
                value: val.clone(),
                provenance: Provenance::Configured,
                changed_at_ms: None,
            })
            .collect();
        if let Some(o) = &self.observed {
            v.push(VarView {
                namespace: "observed",
                key: "screen".into(),
                value: format!("{} ({:.0}%)", o.state.as_deref().unwrap_or("unknown"), o.confidence * 100.0),
                provenance: Provenance::Observed,
                changed_at_ms: Some(o.at_ms),
            });
        }
        v.extend(self.derived.iter().map(|(k, (val, at))| VarView {
            namespace: "derived",
            key: k.clone(),
            value: val.clone(),
            provenance: Provenance::Verified,
            changed_at_ms: Some(*at),
        }));
        for (k, val) in [("state", &self.current), ("previous_state", &self.previous)] {
            if let Some(val) = val {
                v.push(VarView {
                    namespace: "runtime",
                    key: k.into(),
                    value: val.clone(),
                    provenance: Provenance::Runtime,
                    changed_at_ms: None,
                });
            }
        }
        v
    }
}

/// What a run may resume from: no secrets, no images. Resuming re-observes the screen first; it never replays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub workflow: String,
    pub state: String,
    pub derived: BTreeMap<String, String>,
    pub attempts: BTreeMap<String, u32>,
    pub history: Vec<Transition>,
}

impl Checkpoint {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("checkpoint serialises")
    }
    pub fn from_json(s: &str) -> Result<Checkpoint, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }
}

// ---------- events ----------

/// Structured trace events. None carries typed text or a secret value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event")]
pub enum WorkflowEvent {
    Started { workflow: String, dry_run: bool, resumed_from: Option<String> },
    StateObserved { observation: Observation },
    StateEntered { state: String, from: Option<String>, reason: String },
    GuardChecked { state: String, ok: bool, detail: String },
    TenantVerified { state: String, tenant: String, confidence: f64 },
    HumanGateRequested { state: String, message: String, approved: bool },
    ActionAttempted { state: String, attempt: u32, steps: usize, dry_run: bool },
    TransitionVerified { from: String, to: String, confidence: f64, duration_ms: u64 },
    ContextChanged { key: String, from: Option<String>, to: String },
    StateTimedOut { state: String, attempts: u32 },
    Stopped { state: Option<String>, reason: String },
    RunCompleted { state: String },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceEntry {
    pub at_ms: u64,
    #[serde(flatten)]
    pub event: WorkflowEvent,
}

// ---------- errors ----------

#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowError {
    /// A state's actions failed to compile (bad key, missing secret, untypable text). Nothing was sent.
    Compile { state: String, error: RunError },
    Reference(String),
    /// The screen could not be classified with confidence.
    UnknownScreen { best: Option<(String, f64)> },
    /// The screen showed a state the workflow does not allow here.
    Unexpected { expected: Vec<String>, saw: String },
    Guard { state: String, detail: String },
    TenantMismatch { state: String, expected: String },
    GateDeclined { state: String },
    TimedOut { state: String, attempts: u32 },
    ResumeMismatch { checkpoint: String },
    Action { state: String, error: RunError },
    Cancelled,
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::Compile { state, error } => write!(f, "state {state}: {error}"),
            WorkflowError::Reference(s) => write!(f, "cannot load reference image: {s}"),
            WorkflowError::UnknownScreen { best: Some((s, c)) } => write!(
                f,
                "the screen was not recognised with confidence (closest: {s} at {:.0}%); stopped without typing",
                c * 100.0
            ),
            WorkflowError::UnknownScreen { best: None } => {
                write!(f, "no video frames: cannot see the screen (is a capture device open?)")
            }
            WorkflowError::Unexpected { expected, saw } => {
                write!(f, "expected {} but the screen shows {saw}; stopped", expected.join(" or "))
            }
            WorkflowError::Guard { state, detail } => write!(f, "state {state}: entry check failed: {detail}"),
            WorkflowError::TenantMismatch { state, expected } => write!(
                f,
                "state {state}: the screen does not show tenant {expected}'s branding; stopped before enrolling"
            ),
            WorkflowError::GateDeclined { state } => write!(f, "state {state}: the operator stopped the run"),
            WorkflowError::TimedOut { state, attempts } => {
                write!(f, "state {state}: the screen did not move on after {attempts} attempt(s)")
            }
            WorkflowError::ResumeMismatch { checkpoint } => write!(
                f,
                "the screen does not match the checkpoint (state {checkpoint} or what may follow it); not resuming"
            ),
            WorkflowError::Action { state, error } => write!(f, "state {state}: {error}"),
            WorkflowError::Cancelled => write!(f, "aborted"),
        }
    }
}
impl std::error::Error for WorkflowError {}

// ---------- engine ----------

#[derive(Debug, Clone)]
pub struct WorkflowOptions {
    pub run: RunOptions,
    /// How long to look for a recognisable screen at the start (and after a human recovery).
    pub observe_timeout: Duration,
    /// A classification counts only when two consecutive frames agree (screens mid-animation do not).
    pub settle_frames: u32,
    /// When the screen is unknown, ask the operator to fix it by hand and look again (once per stop).
    pub offer_manual_recovery: bool,
}

impl Default for WorkflowOptions {
    fn default() -> Self {
        WorkflowOptions {
            run: RunOptions::default(),
            observe_timeout: Duration::from_secs(30),
            settle_frames: 2,
            offer_manual_recovery: true,
        }
    }
}

/// The outcome and full record of a run.
#[derive(Debug, Clone)]
pub struct WorkflowReport {
    pub result: Result<(), WorkflowError>,
    pub trace: Vec<TraceEntry>,
    pub context: Context,
}

impl WorkflowReport {
    /// What a run can resume from, if it stopped in a known state.
    pub fn checkpoint(&self, wf: &Workflow) -> Option<Checkpoint> {
        let state = self.context.current.clone()?;
        Some(Checkpoint {
            workflow: wf.name.clone(),
            state,
            derived: self.context.derived.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect(),
            attempts: self.context.attempts.clone(),
            history: self.context.history.clone(),
        })
    }
}

/// Compile every state's actions before anything runs: a missing secret or an untypable character stops the run
/// before the first key, as for scripts.
pub fn compile_workflow(
    wf: &Workflow,
    secrets: &Vars,
    layout: &dyn Layout,
) -> Result<BTreeMap<String, Vec<(usize, Op)>>, WorkflowError> {
    let mut vars: Vars = wf.config.clone();
    let mut defs: BTreeMap<String, VarDef> =
        wf.config.keys().map(|k| (k.clone(), VarDef::default())).collect();
    for (k, prompt) in &wf.secrets {
        defs.insert(k.clone(), VarDef { prompt: Some(prompt.clone()), default: None, secret: true });
        if let Some(v) = secrets.get(k) {
            vars.insert(k.clone(), v.clone());
        }
    }
    let mut out = BTreeMap::new();
    for s in &wf.states {
        let script = Script { name: s.name.clone(), description: String::new(), vars: defs.clone(), steps: s.actions.clone() };
        let ops = compile(&script, &vars, layout).map_err(|error| WorkflowError::Compile { state: s.name.clone(), error })?;
        out.insert(s.name.clone(), ops);
    }
    for v in vars.values_mut() {
        zeroize::Zeroize::zeroize(v);
    }
    Ok(out)
}

fn frame_hash(f: &Frame) -> String {
    // FNV-1a over the comparison thumbnail: enough to tell frames apart in a trace, reveals nothing readable.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in f.thumb() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

struct Engine<'a> {
    wf: &'a Workflow,
    host: &'a mut dyn Host,
    opts: &'a WorkflowOptions,
    refs: BTreeMap<String, Frame>,
    /// The frame behind the latest settled classification: later checks use the same evidence.
    last_frame: Option<Frame>,
    ctx: Context,
    trace: Vec<TraceEntry>,
    sink: &'a mut dyn FnMut(&TraceEntry, &Context),
    started: Instant,
}

impl Engine<'_> {
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn emit(&mut self, event: WorkflowEvent) {
        let entry = TraceEntry { at_ms: self.now(), event };
        (self.sink)(&entry, &self.ctx);
        self.trace.push(entry);
    }

    fn reference(&mut self, name: &str) -> Result<Frame, WorkflowError> {
        if let Some(f) = self.refs.get(name) {
            return Ok(f.clone());
        }
        let f = self.host.reference(name).map_err(WorkflowError::Reference)?;
        self.refs.insert(name.to_string(), f.clone());
        Ok(f)
    }

    /// Best and runner-up state for one frame.
    fn classify(&mut self, frame: &Frame) -> Result<Observation, WorkflowError> {
        let mut scores: Vec<(String, f64, String, f64)> = Vec::new(); // state, similarity, reference, threshold
        for s in &self.wf.states {
            let mut best: Option<(f64, String)> = None;
            for screen in &s.detect.screens {
                let r = self.reference(screen)?;
                let sim = frame.similarity(&r);
                if best.as_ref().is_none_or(|(b, _)| sim > *b) {
                    best = Some((sim, screen.clone()));
                }
            }
            let (sim, screen) = best.expect("every state has a screen");
            scores.push((s.name.clone(), sim, screen, s.detect.threshold));
        }
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        let top = &scores[0];
        let runner_up = scores.get(1).map(|r| (r.0.clone(), r.1));
        let clear = runner_up.as_ref().is_none_or(|(_, r)| top.1 - r >= self.wf.ambiguity_margin);
        let state = (top.1 >= top.3 && clear).then(|| top.0.clone());
        Ok(Observation {
            state,
            confidence: top.1,
            runner_up,
            reference: Some(top.2.clone()),
            frame_hash: frame_hash(frame),
            at_ms: self.now(),
        })
    }

    fn check(&self) -> Result<(), WorkflowError> {
        if self.host.cancelled() { Err(WorkflowError::Cancelled) } else { Ok(()) }
    }

    /// Look until the screen settles on a classification (the same answer for `settle_frames` frames in a row) or
    /// `timeout` passes. Returns the last observation; its `state` is `None` if nothing settled with confidence.
    fn observe(&mut self, timeout: Duration, stop_on: impl Fn(&Observation) -> bool) -> Result<Option<Observation>, WorkflowError> {
        let poll = self.opts.run.poll;
        let mut elapsed = Duration::ZERO;
        let mut last: Option<Observation> = None;
        let mut streak = 0u32;
        loop {
            self.check()?;
            if let Some(frame) = self.host.screen() {
                let o = self.classify(&frame)?;
                self.last_frame = Some(frame);
                streak = if last.as_ref().is_some_and(|l| l.state == o.state) { streak + 1 } else { 1 };
                let settled = streak >= self.opts.settle_frames.max(1);
                last = Some(o.clone());
                // A settled, recognised screen the caller is waiting for ends the look. An unknown screen keeps
                // being looked at until the timeout: it may be mid-transition.
                if settled && o.state.is_some() && stop_on(&o) {
                    self.ctx.observed = Some(o.clone());
                    self.emit(WorkflowEvent::StateObserved { observation: o.clone() });
                    return Ok(Some(o));
                }
            }
            if elapsed >= timeout {
                if let Some(o) = &last {
                    self.ctx.observed = Some(o.clone());
                    self.emit(WorkflowEvent::StateObserved { observation: o.clone() });
                }
                return Ok(last.map(|mut o| {
                    if streak < self.opts.settle_frames.max(1) {
                        o.state = None;
                    }
                    o
                }));
            }
            self.host.sleep(poll);
            elapsed += poll;
        }
    }

    fn unknown(o: Option<Observation>) -> WorkflowError {
        WorkflowError::UnknownScreen { best: o.map(|o| (o.state.unwrap_or_else(|| "unknown".into()), o.confidence)) }
    }

    /// Recognise the screen; on an unknown screen, offer the operator a manual fix and look once more.
    fn recognise(&mut self, timeout: Duration, allowed: &[String]) -> Result<Observation, WorkflowError> {
        let accept = |o: &Observation| o.state.is_some();
        let mut o = self.observe(timeout, accept)?;
        if o.as_ref().is_none_or(|o| o.state.is_none()) && self.opts.offer_manual_recovery && !self.opts.run.dry_run {
            let msg = format!(
                "The screen is not recognised. Bring the target to one of: {}. Then continue, or stop.",
                allowed.join(", ")
            );
            let approved = self.host.confirm(&msg);
            let state = self.ctx.current.clone().unwrap_or_else(|| "UNKNOWN".into());
            self.emit(WorkflowEvent::HumanGateRequested { state, message: msg, approved });
            if !approved {
                return Err(Self::unknown(o));
            }
            o = self.observe(timeout, accept)?;
        }
        match o {
            Some(o) if o.state.is_some() => Ok(o),
            other => Err(Self::unknown(other)),
        }
    }

    fn enter(&mut self, to: &str, reason: &str) {
        let from = self.ctx.current.replace(to.to_string());
        if from.is_some() {
            self.ctx.previous = from.clone();
        }
        self.emit(WorkflowEvent::StateEntered { state: to.to_string(), from, reason: reason.to_string() });
    }

    fn guard(&mut self, s: &StateDef) -> Result<(), WorkflowError> {
        for (k, want) in &s.require {
            let have = self.wf.config.get(k).cloned().or_else(|| self.ctx.derived.get(k).map(|(v, _)| v.clone()));
            if have.as_deref() != Some(want.as_str()) {
                let detail = format!("{k} must be {want:?}, is {}", have.map(|h| format!("{h:?}")).unwrap_or("unset".into()));
                self.emit(WorkflowEvent::GuardChecked { state: s.name.clone(), ok: false, detail: detail.clone() });
                return Err(WorkflowError::Guard { state: s.name.clone(), detail });
            }
        }
        if !s.require.is_empty() {
            self.emit(WorkflowEvent::GuardChecked { state: s.name.clone(), ok: true, detail: format!("{} condition(s)", s.require.len()) });
        }
        Ok(())
    }

    /// The configured tenant's branding must be on screen, and match better than any other allowed tenant's.
    fn verify_tenant(&mut self, s: &StateDef) -> Result<(), WorkflowError> {
        let expected = self.wf.config.get("tenant").cloned().unwrap_or_default();
        let mismatch = || WorkflowError::TenantMismatch { state: s.name.clone(), expected: expected.clone() };
        let frame = self.last_frame.clone().ok_or_else(mismatch)?;
        let mut scores = Vec::new();
        for (tenant, image) in &self.wf.tenants.clone() {
            let r = self.reference(image)?;
            scores.push((tenant.clone(), frame.similarity(&r)));
        }
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (best, conf) = scores[0].clone();
        let clear = scores.get(1).is_none_or(|(_, r)| conf - r >= self.wf.ambiguity_margin);
        if best != expected || conf < s.detect.threshold || !clear {
            return Err(mismatch());
        }
        self.emit(WorkflowEvent::TenantVerified { state: s.name.clone(), tenant: expected, confidence: conf });
        Ok(())
    }

    fn act(&mut self, s: &StateDef, ops: &[(usize, Op)], attempt: u32) -> Result<(), WorkflowError> {
        *self.ctx.attempts.entry(s.name.clone()).or_insert(0) = attempt;
        self.emit(WorkflowEvent::ActionAttempted {
            state: s.name.clone(),
            attempt,
            steps: ops.len(),
            dry_run: self.opts.run.dry_run,
        });
        if ops.is_empty() {
            return Ok(());
        }
        run(ops, self.host, &self.opts.run).map_err(|e| match e {
            RunError::Cancelled => WorkflowError::Cancelled,
            error => WorkflowError::Action { state: s.name.clone(), error },
        })
    }

    fn run_states(&mut self, compiled: &BTreeMap<String, Vec<(usize, Op)>>, resume: Option<&Checkpoint>) -> Result<(), WorkflowError> {
        // Where may the run begin? Fresh: the start states. Resumed: the checkpoint's state or what may follow it.
        let allowed: Vec<String> = match resume {
            None => self.wf.start.clone(),
            Some(cp) => {
                let mut a = vec![cp.state.clone()];
                a.extend(self.wf.state(&cp.state).map(|s| s.next.clone()).unwrap_or_default());
                a
            }
        };
        let first = self.recognise(self.opts.observe_timeout, &allowed)?;
        let first = first.state.expect("recognised");
        if !allowed.contains(&first) {
            return Err(match resume {
                Some(cp) => WorkflowError::ResumeMismatch { checkpoint: cp.state.clone() },
                None => WorkflowError::Unexpected { expected: allowed, saw: first },
            });
        }
        self.enter(&first, if resume.is_some() { "re-observed after resume" } else { "observed at start" });

        loop {
            self.check()?;
            let name = self.ctx.current.clone().expect("in a state");
            let s = self.wf.state(&name).expect("known state").clone();
            self.guard(&s)?;
            if s.terminal {
                self.emit(WorkflowEvent::RunCompleted { state: name });
                return Ok(());
            }
            if s.verify_tenant {
                self.verify_tenant(&s)?;
            }
            if let Some(msg) = &s.gate {
                let approved = self.opts.run.dry_run || self.host.confirm(msg);
                self.emit(WorkflowEvent::HumanGateRequested { state: name.clone(), message: msg.clone(), approved });
                if !approved {
                    return Err(WorkflowError::GateDeclined { state: name });
                }
            }
            let ops = &compiled[&name];
            let entered_at = self.now();
            let mut attempt = 0;
            let next = loop {
                attempt += 1;
                self.act(&s, ops, attempt)?;
                // Verify: the screen must settle on a permitted next state. Staying put is "not yet"; anything else
                // stops the run.
                let next_states = s.next.clone();
                let here = name.clone();
                let o = self.observe(s.timeout, |o| o.state.as_ref().is_some_and(|st| *st != here))?;
                match o.and_then(|o| o.state.map(|st| (st, o.confidence))) {
                    Some((st, conf)) if next_states.contains(&st) => break (st, conf),
                    Some((st, _)) if st != name => {
                        return Err(WorkflowError::Unexpected { expected: next_states, saw: st });
                    }
                    Some(_) | None => {
                        if attempt <= s.retries && s.retry_safe {
                            continue;
                        }
                        self.emit(WorkflowEvent::StateTimedOut { state: name.clone(), attempts: attempt });
                        return Err(WorkflowError::TimedOut { state: name, attempts: attempt });
                    }
                }
            };
            let (to, confidence) = next;
            let at = self.now();
            self.emit(WorkflowEvent::TransitionVerified {
                from: name.clone(),
                to: to.clone(),
                confidence,
                duration_ms: at.saturating_sub(entered_at),
            });
            self.ctx.history.push(Transition { from: name.clone(), to: to.clone(), at_ms: at, attempts: attempt, confidence });
            for (k, v) in &s.derive {
                let from = self.ctx.derived.insert(k.clone(), (v.clone(), at)).map(|(old, _)| old);
                if from.as_deref() != Some(v.as_str()) {
                    self.emit(WorkflowEvent::ContextChanged { key: format!("derived.{k}"), from, to: v.clone() });
                }
            }
            self.enter(&to, "transition verified");
        }
    }
}

/// Run a workflow. `secrets` holds the prompted secret values; they are compiled into key strokes and never kept
/// anywhere else. `sink` sees every trace entry as it happens (for a live view). Always releases every key at the
/// end, unless dry-running.
pub fn run_workflow(
    wf: &Workflow,
    secrets: &Vars,
    layout: &dyn Layout,
    host: &mut dyn Host,
    opts: &WorkflowOptions,
    resume: Option<&Checkpoint>,
    sink: &mut dyn FnMut(&TraceEntry, &Context),
) -> WorkflowReport {
    let mut ctx = Context { config: wf.config.clone(), ..Default::default() };
    if let Some(cp) = resume {
        ctx.derived = cp.derived.iter().map(|(k, v)| (k.clone(), (v.clone(), 0))).collect();
        ctx.attempts = cp.attempts.clone();
        ctx.history = cp.history.clone();
    }
    let mut e = Engine { wf, host, opts, refs: BTreeMap::new(), last_frame: None, ctx, trace: Vec::new(), sink, started: Instant::now() };
    e.emit(WorkflowEvent::Started {
        workflow: wf.name.clone(),
        dry_run: opts.run.dry_run,
        resumed_from: resume.map(|c| c.state.clone()),
    });
    let result = (|| {
        if let Some(cp) = resume {
            if cp.workflow != wf.name || wf.state(&cp.state).is_none() {
                return Err(WorkflowError::ResumeMismatch { checkpoint: cp.state.clone() });
            }
        }
        let compiled = compile_workflow(wf, secrets, layout)?;
        e.run_states(&compiled, resume)
    })();
    let mut result = result;
    if !opts.run.dry_run {
        if let Err(err) = e.host.release_all() {
            if result.is_ok() {
                result = Err(WorkflowError::Action { state: e.ctx.current.clone().unwrap_or_default(), error: RunError::Host(err) });
            }
        }
    }
    if let Err(err) = &result {
        let state = e.ctx.current.clone();
        e.emit(WorkflowEvent::Stopped { state, reason: err.to_string() });
    }
    WorkflowReport { result, trace: e.trace, context: e.ctx }
}

// ---------- export ----------

/// One JSON object per trace entry. Contains no typed text and no secret values (events never hold them).
pub fn trace_jsonl(report: &WorkflowReport) -> String {
    report
        .trace
        .iter()
        .map(|t| serde_json::to_string(t).expect("trace serialises"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A short Markdown record for a ticket: outcome, transitions and the final context.
pub fn trace_markdown(wf: &Workflow, report: &WorkflowReport) -> String {
    let mut md = format!("### Workflow: {}\n\n", if wf.name.is_empty() { "(unnamed)" } else { &wf.name });
    md.push_str(&match &report.result {
        Ok(()) => format!("**Completed** in state `{}`.\n\n", report.context.current.as_deref().unwrap_or("?")),
        Err(e) => format!("**Stopped:** {e}\n\n"),
    });
    md.push_str("| # | From | To | Attempts | Confidence | At |\n|---|---|---|---|---|---|\n");
    for (i, t) in report.context.history.iter().enumerate() {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {:.0}% | {:.1} s |\n",
            i + 1,
            t.from,
            t.to,
            t.attempts,
            t.confidence * 100.0,
            t.at_ms as f64 / 1000.0
        ));
    }
    md.push_str("\n| Namespace | Key | Value | Source |\n|---|---|---|---|\n");
    for v in report.context.inspect() {
        md.push_str(&format!("| {} | {} | {} | {:?} |\n", v.namespace, v.key, v.value.replace('|', "\\|"), v.provenance));
    }
    md.push_str("\nSecrets are never recorded. Screens were matched against reference images; this is not a hardware verification record.\n");
    md
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
