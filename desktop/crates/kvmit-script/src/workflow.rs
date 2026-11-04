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
    /// Compare only this part of the screen: `[x, y, width, height]` as fractions of the frame (both the capture and
    /// the reference are cropped the same way).
    pub region: Option<[f64; 4]>,
}

/// How a frame is compared with a reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matching {
    /// The pattern of light and dark ([`Frame::structure_similarity`]). The default: real setup screens share their
    /// background and differ in text, which brightness alone barely sees.
    Structure,
    /// Mean brightness difference ([`Frame::similarity`]), as `wait_for` uses.
    Luma,
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
    /// The operator does this step by hand (connect a network, approve a sign-in on a phone): nothing is typed, and the
    /// engine waits, watching, for the screen to move on to a permitted next state. Shown as written; no secrets.
    pub manual: Option<String>,
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
    pub matching: Matching,
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
    #[serde(default)]
    region: Option<[f64; 4]>,
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
    manual: Option<String>,
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
    #[serde(default, rename = "match")]
    matching: Option<String>,
    states: Vec<RawState>,
}

const MAX_STATES: usize = 64;
const MAX_RETRIES: u32 = 5;
const MIN_TIMEOUT: Duration = Duration::from_millis(500);

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A recorded fact is one short line of printable text ("connected", "United States").
fn valid_fact(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= 200 && !s.chars().any(char::is_control)
}

/// The most a state's actions may pause in total. A pause is a gap between the engine's last look and the keys that
/// follow it, so it must stay short; longer waits belong to states.
pub const MAX_ACTION_PAUSE: Duration = Duration::from_secs(2);

fn total_pause(steps: &[Step]) -> Duration {
    steps.iter().fold(Duration::ZERO, |acc, s| {
        acc.saturating_add(match s {
            Step::Delay(d) => *d,
            Step::Repeat { count, steps } => total_pause(steps).saturating_mul(*count),
            _ => Duration::ZERO,
        })
    })
}

/// The `{{name}}` references in a template (malformed templates are reported later, by the compiler).
fn template_names(t: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(i) = rest.find("{{") {
        let after = &rest[i + 2..];
        let Some(j) = after.find("}}") else { break };
        out.push(after[..j].trim().to_string());
        rest = &after[j + 2..];
    }
    out
}

/// Checks on a state's actions that the script format allows but a workflow does not.
fn check_actions(state: &str, steps: &[Step], secrets: &BTreeMap<String, RawSecret>) -> Result<(), String> {
    for step in steps {
        match step {
            // Waiting and asking belong to states: a step that blocks inside the actions would let the screen change
            // under keys that are still to be typed, without the engine looking again.
            Step::Wait(_) | Step::Confirm(_) => {
                return Err(format!(
                    "state {state:?}: `wait_for` and `confirm` are not allowed in workflow actions (use states and `gate`)"
                ))
            }
            // Secret text may only come from prompted secrets: a config value is stored in the file and shown in the
            // inspector and exports, so typing it "as a secret" would only pretend to protect it.
            Step::SecretText(t) => {
                if let Some(n) = template_names(t).into_iter().find(|n| !secrets.contains_key(n)) {
                    return Err(format!(
                        "state {state:?}: secret_text uses {n:?}, which is not under [secrets]; secrets must be prompted"
                    ));
                }
            }
            Step::Repeat { steps, .. } => check_actions(state, steps, secrets)?,
            _ => {}
        }
    }
    Ok(())
}

/// Whether a file is a workflow rather than a script: its parsed top level has a `workflow_format` key (quoted or not;
/// a key of that name nested in a table does not count). A file that is not valid TOML is neither, and says so when
/// parsed as a script.
pub fn is_workflow_source(toml_text: &str) -> bool {
    toml::from_str::<toml::Table>(toml_text).is_ok_and(|t| t.contains_key("workflow_format"))
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
        if !(0.01..=0.5).contains(&margin) {
            return Err("ambiguity_margin must be within 0.01..=0.5 (a tie must never pick a state)".into());
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
            if let Some([x, y, w, h]) = r.detect.region {
                let ok = [x, y, w, h].iter().all(|v| (0.0..=1.0).contains(v)) && w > 0.0 && h > 0.0 && x + w <= 1.0 && y + h <= 1.0;
                if !ok {
                    return Err(format!("state {:?}: region must be [x, y, width, height] inside 0..1", r.name));
                }
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
            if r.next.contains(&r.name) {
                return Err(format!(
                    "state {:?} lists itself in `next`: staying on a screen is never a transition (use retries)",
                    r.name
                ));
            }
            if r.terminal && !r.next.is_empty() {
                return Err(format!("state {:?} is terminal and cannot have `next` states", r.name));
            }
            if !r.terminal && r.next.is_empty() {
                return Err(format!("state {:?} needs `next` states or `terminal = true`", r.name));
            }
            if let Some(m) = &r.manual {
                if m.trim().is_empty() {
                    return Err(format!("state {:?}: `manual` must say what the operator should do", r.name));
                }
                if !r.actions.is_empty() || r.gate.is_some() || r.terminal || r.retries > 0 || r.retry_safe {
                    return Err(format!(
                        "state {:?}: a `manual` state types nothing, asks nothing first and is never retried (no actions, gate, retries or terminal)",
                        r.name
                    ));
                }
            }
            if r.verify_tenant && raw.tenants.is_empty() {
                return Err(format!("state {:?} verifies the tenant but the workflow lists no tenants", r.name));
            }
            for (k, v) in &r.derive {
                if !valid_name(k) || raw.config.contains_key(k) || raw.secrets.contains_key(k) {
                    return Err(format!("state {:?}: bad derived name {k:?}", r.name));
                }
                if !valid_fact(v) {
                    return Err(format!(
                        "state {:?}: derived value for {k:?} must be one line of at most 200 characters",
                        r.name
                    ));
                }
            }
            if r.require.keys().any(|k| raw.secrets.contains_key(k)) {
                return Err(format!("state {:?}: a guard cannot compare a secret", r.name));
            }
            let timeout = parse_duration(r.timeout.as_deref().unwrap_or("60s"))?;
            if timeout < MIN_TIMEOUT {
                return Err(format!("state {:?}: timeout must be at least 500ms", r.name));
            }
            let actions: Vec<Step> = r.actions.into_iter().map(step_from_raw).collect::<Result<_, _>>()?;
            check_actions(&r.name, &actions, &raw.secrets)?;
            if total_pause(&actions) > MAX_ACTION_PAUSE {
                return Err(format!(
                    "state {:?}: its actions pause for more than 2s in total; wait in a state instead, so the engine looks again",
                    r.name
                ));
            }
            states.push(StateDef {
                timeout,
                actions,
                name: r.name,
                description: r.description,
                detect: Detect { screens, threshold, region: r.detect.region },
                next: r.next,
                require: r.require,
                gate: r.gate,
                manual: r.manual,
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
            matching: match raw.matching.as_deref() {
                None => {
                    return Err("`match` is required: \"structure\" (the pattern of light and dark, for real screens) or \"luma\" (brightness); thresholds mean different things for each".into())
                }
                Some("structure") => Matching::Structure,
                Some("luma") => Matching::Luma,
                Some(m) => return Err(format!("match must be \"structure\" or \"luma\", got {m:?}")),
            },
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
    /// Attempts per state over the whole run (for the record).
    pub attempts: BTreeMap<String, u32>,
    /// Attempts in the current visit to the current state: what a resume must account for.
    pub visit_attempts: u32,
    /// The current visit passed its entry checks and is watching for the screen to move on without sending anything (a
    /// manual step, or a state that only waits). A resume that finds the next screen reconciles that transition.
    pub visit_waiting: bool,
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
/// `attempts` keeps the attempts already spent in `state`: resuming continues the retry budget instead of starting a
/// new one, and actions that were sent and not verified are only sent again with the operator's say-so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub workflow: String,
    pub state: String,
    pub derived: BTreeMap<String, String>,
    pub attempts: BTreeMap<String, u32>,
    /// How many times this visit to `state` sent its actions. Zero means nothing was sent (the run stopped at a guard,
    /// a gate or a look), so there is no pending transition to reconcile and nothing was spent.
    /// `None` in a checkpoint written before this field existed: unknown, and treated as "sent" (the spent
    /// attempts of the state) for replay protection, and as "nothing to reconcile" for facts.
    #[serde(default)]
    pub attempted: Option<u32>,
    /// The state sent nothing but was waiting for the screen to move on (a manual step, or a wait): when a resume finds
    /// a permitted next screen, that is the state's transition, and its facts are recorded. Never a reason to send.
    #[serde(default)]
    pub waiting: bool,
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
    /// A `manual` state: the operator was asked to do this by hand; the engine now waits for the screen to move on.
    ManualStepRequested { state: String, message: String, timeout_ms: u64 },
    ActionAttempted { state: String, attempt: u32, steps: usize, dry_run: bool },
    TransitionVerified { from: String, to: String, confidence: f64, duration_ms: u64 },
    ContextChanged { key: String, from: Option<String>, to: String },
    StateTimedOut { state: String, attempts: u32 },
    Stopped { state: Option<String>, reason: String },
    RunCompleted { state: String },
}

impl WorkflowEvent {
    /// Replace any run-time secret value that found its way into free text (a gate message an author wrote, a host
    /// diagnostic) before the event is published.
    fn scrub(&mut self, secrets: &Secrets) {
        match self {
            WorkflowEvent::HumanGateRequested { message, .. } | WorkflowEvent::ManualStepRequested { message, .. } => {
                secrets.scrub(message)
            }
            WorkflowEvent::Stopped { reason, .. } => secrets.scrub(reason),
            WorkflowEvent::GuardChecked { detail, .. } => secrets.scrub(detail),
            WorkflowEvent::StateEntered { reason, .. } => secrets.scrub(reason),
            WorkflowEvent::ContextChanged { from, to, .. } => {
                secrets.scrub(to);
                if let Some(f) = from {
                    secrets.scrub(f);
                }
            }
            _ => {}
        }
    }
}

/// The run's secret values, kept only to scrub them from published text; wiped when the run ends.
struct Secrets(Vec<String>);

impl Secrets {
    /// Replace every occurrence of every secret. Matches are found in the original text and overlapping ones merged,
    /// so redacting one secret can never break the match of another that overlaps it.
    fn scrub(&self, text: &mut String) {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for s in self.0.iter().filter(|s| !s.is_empty()) {
            let mut from = 0;
            while let Some(i) = text[from..].find(s.as_str()) {
                spans.push((from + i, from + i + s.len()));
                from += i + 1;
                while !text.is_char_boundary(from) {
                    from += 1;
                }
            }
        }
        if spans.is_empty() {
            return;
        }
        spans.sort();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (a, b) in spans {
            match merged.last_mut() {
                Some(last) if a <= last.1 => last.1 = last.1.max(b),
                _ => merged.push((a, b)),
            }
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (a, b) in merged {
            out.push_str(&text[at..a]);
            out.push_str("<redacted>");
            at = b;
        }
        out.push_str(&text[at..]);
        *text = out;
    }

    fn appears_in(&self, text: &str) -> bool {
        self.0.iter().any(|s| !s.is_empty() && text.contains(s.as_str()))
    }
}

/// The host as the nested script runner sees it: its events are scrubbed before they are published, like the
/// workflow's own.
struct Scrubbed<'a> {
    inner: &'a mut dyn Host,
    secrets: &'a Secrets,
}

impl Host for Scrubbed<'_> {
    fn key_down(&mut self, key: kvmit_hid::Key) -> Result<(), String> {
        self.inner.key_down(key)
    }
    fn key_up(&mut self, key: kvmit_hid::Key) -> Result<(), String> {
        self.inner.key_up(key)
    }
    fn release_all(&mut self) -> Result<(), String> {
        self.inner.release_all()
    }
    fn mouse_move(&mut self, dx: i32, dy: i32) -> Result<(), String> {
        self.inner.mouse_move(dx, dy)
    }
    fn mouse_button(&mut self, mask: u8, down: bool) -> Result<(), String> {
        self.inner.mouse_button(mask, down)
    }
    fn sleep(&mut self, d: Duration) {
        self.inner.sleep(d)
    }
    fn screen(&mut self) -> Option<Frame> {
        self.inner.screen()
    }
    fn reference(&mut self, name: &str) -> Result<Frame, String> {
        self.inner.reference(name)
    }
    fn confirm(&mut self, message: &str) -> bool {
        self.inner.confirm(message)
    }
    fn cancelled(&self) -> bool {
        self.inner.cancelled()
    }
    fn event(&mut self, mut ev: crate::exec::RunEvent) {
        match &mut ev {
            crate::exec::RunEvent::Aborted { reason } => self.secrets.scrub(reason),
            crate::exec::RunEvent::StepStarted { detail, .. } => self.secrets.scrub(detail),
            _ => {}
        }
        self.inner.event(ev)
    }
}

impl Drop for Secrets {
    fn drop(&mut self) {
        for s in self.0.iter_mut() {
            zeroize::Zeroize::zeroize(s);
        }
    }
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
    /// Resuming would send actions that were already sent and not verified; the operator did not agree.
    ResumeDeclined { state: String },
    /// The video source does not number its frames, so a stalled capture cannot be told from a still screen.
    NoFrameNumbers,
    /// The workflow file itself contains the value of a prompted secret (where is named, never the value).
    SecretInFile { place: String },
    Options(String),
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
            WorkflowError::ResumeDeclined { state } => write!(
                f,
                "state {state}: its actions were already sent and not verified; not sending them again without the operator"
            ),
            WorkflowError::NoFrameNumbers => write!(
                f,
                "this video source does not number its frames; a workflow cannot tell a stalled capture from a still screen"
            ),
            WorkflowError::SecretInFile { place } => write!(
                f,
                "the workflow file contains the value of a prompted secret ({place}); remove it from the file"
            ),
            WorkflowError::Options(m) => write!(f, "{m}"),
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
    /// After a boundary (an action, an approval), frames arriving within this time may still be pictures from before
    /// it, already in the capture's queue, and are not used as evidence.
    pub capture_latency: Duration,
}

impl Default for WorkflowOptions {
    fn default() -> Self {
        WorkflowOptions {
            run: RunOptions::default(),
            observe_timeout: Duration::from_secs(30),
            settle_frames: 2,
            offer_manual_recovery: true,
            capture_latency: Duration::from_millis(300),
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
            attempted: Some(self.context.visit_attempts),
            waiting: self.context.visit_waiting,
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
    /// The newest capture number seen. Frames at or below a boundary taken after acting were captured before the
    /// action took effect, and never verify it.
    seen_seq: Option<u64>,
    secrets: Secrets,
    ctx: Context,
    trace: Vec<TraceEntry>,
    sink: &'a mut dyn FnMut(&TraceEntry, &Context),
    started: Instant,
}

/// A moment after which evidence must have been captured: a decision was taken, or keys were sent.
#[derive(Debug, Clone, Copy)]
struct Boundary {
    /// The newest capture number seen at that moment.
    seq: u64,
    /// The moment itself. A frame the backend received before it can never be evidence after it, however late it is
    /// delivered.
    at: Instant,
}

/// How a look at the screen ended.
struct Look {
    /// The last classification, normalised: `state` is `None` unless it settled.
    obs: Option<Observation>,
}

impl Engine<'_> {
    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn emit(&mut self, mut event: WorkflowEvent) {
        event.scrub(&self.secrets);
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

    /// The winner among `scores` (name, similarity, threshold), if it clears its threshold and beats the runner-up
    /// by the margin. A tie never wins, whatever the margin.
    fn winner(&self, scores: &[(String, f64, f64)]) -> Option<String> {
        let top = scores.first()?;
        let clear = scores.get(1).is_none_or(|r| top.1 - r.1 > 0.0 && top.1 - r.1 >= self.wf.ambiguity_margin);
        (top.1 >= top.2 && clear).then(|| top.0.clone())
    }

    /// One frame against one reference, with the workflow's comparison, in a region if the state has one.
    fn compare(&self, frame: &Frame, reference: &Frame, region: Option<[f64; 4]>) -> f64 {
        let (f, r) = match region {
            Some([x, y, w, h]) => (frame.crop(x, y, w, h), reference.crop(x, y, w, h)),
            None => (frame.clone(), reference.clone()),
        };
        match self.wf.matching {
            Matching::Structure => f.structure_similarity(&r),
            Matching::Luma => f.similarity(&r),
        }
    }

    /// Best and runner-up state for one frame.
    fn classify(&mut self, frame: &Frame) -> Result<Observation, WorkflowError> {
        let mut scores: Vec<(String, f64, f64)> = Vec::new(); // state, similarity, threshold
        let mut refs: BTreeMap<String, String> = BTreeMap::new();
        for s in &self.wf.states {
            let mut best: Option<(f64, String)> = None;
            for screen in &s.detect.screens {
                let r = self.reference(screen)?;
                let sim = self.compare(frame, &r, s.detect.region);
                if best.as_ref().is_none_or(|(b, _)| sim > *b) {
                    best = Some((sim, screen.clone()));
                }
            }
            let (sim, screen) = best.expect("every state has a screen");
            refs.insert(s.name.clone(), screen);
            scores.push((s.name.clone(), sim, s.detect.threshold));
        }
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        let state = self.winner(&scores);
        Ok(Observation {
            state,
            confidence: scores[0].1,
            runner_up: scores.get(1).map(|r| (r.0.clone(), r.1)),
            reference: refs.remove(&scores[0].0),
            frame_hash: frame_hash(frame),
            at_ms: self.now(),
        })
    }

    fn check(&self) -> Result<(), WorkflowError> {
        if self.host.cancelled() { Err(WorkflowError::Cancelled) } else { Ok(()) }
    }

    fn record(&mut self, o: &Observation) {
        self.ctx.observed = Some(o.clone());
        self.emit(WorkflowEvent::StateObserved { observation: o.clone() });
    }

    /// Look until the screen settles on a classification the caller wants, or `timeout` passes.
    ///
    /// A classification settles when it is the same for `settle_frames` *distinct* captured frames in a row: reading
    /// the same captured frame twice is not a second look, a missing frame starts over, and frames captured at or
    /// before `after_seq` (taken right after acting) are ignored, so a picture from before the action can never
    /// verify it. Sources without capture numbers get the plain consecutive-reads rule.
    fn observe(
        &mut self,
        timeout: Duration,
        after: Option<Boundary>,
        stop_on: impl Fn(&Observation) -> bool,
    ) -> Result<Look, WorkflowError> {
        let poll = self.opts.run.poll.max(Duration::from_millis(10));
        let need = self.opts.settle_frames.max(1);
        // Frames that may still be queued pictures from before the boundary are skipped.
        let ignore_for = if after.is_some() { self.opts.capture_latency } else { Duration::ZERO };
        let real = Instant::now();
        let mut elapsed = Duration::ZERO;
        let mut last: Option<Observation> = None;
        let mut last_seq: Option<u64> = None;
        let mut floor: Option<u64> = None;
        let mut streak = 0u32;
        let mut first = true;
        loop {
            self.check()?;
            // No new capture is started once the deadline has passed (the first look always happens).
            let expired = elapsed >= timeout || real.elapsed() >= timeout;
            let frame = if first || !expired { Some(self.host.screen()) } else { None };
            first = false;
            // `None`: no capture was started (past the deadline); the timeout below reports what was seen.
            if let Some(frame) = frame { match frame {
                None => {
                    streak = 0;
                    last = None;
                }
                Some(frame) => {
                    // Without frame numbers a stalled capture looks like a still screen: refuse to guess.
                    let sq = frame.seq.ok_or(WorkflowError::NoFrameNumbers)?;
                    self.seen_seq = Some(self.seen_seq.map_or(sq, |s| s.max(sq)));
                    // A frame first seen inside the latency window may be a queued picture from before the
                    // boundary: it, and anything not newer than it, never counts in this look, even once the window
                    // has passed (a capture that freezes on it keeps showing it).
                    if elapsed < ignore_for {
                        floor = Some(floor.map_or(sq, |f: u64| f.max(sq)));
                    }
                    // (A frame inside the window has just raised the floor to itself, so it is excluded too.)
                    // Received by the backend before the boundary: a picture of the past, whatever its number.
                    let received_after = match (after, frame.captured_at) {
                        (Some(b), Some(t)) => t > b.at,
                        _ => true,
                    };
                    let fresh = after.is_none_or(|b| sq > b.seq) && floor.is_none_or(|f| sq > f) && received_after;
                    // Only a newer frame is a new look; the same (or an older, reordered) frame is not.
                    let new = last_seq.is_none_or(|prev| sq > prev);
                    if fresh && new {
                        last_seq = Some(sq);
                        let o = self.classify(&frame)?;
                        // Evidence counts only inside the deadline, measured in real time too (classifying and the
                        // host's own delays take time the poll count does not see). A late frame does not count at
                        // all, so it cannot complete a settled look that the timeout would then report.
                        let in_time = elapsed <= timeout && real.elapsed() <= timeout;
                        if in_time {
                            self.last_frame = Some(frame);
                            streak = if last.as_ref().is_some_and(|l| l.state == o.state) { streak + 1 } else { 1 };
                            last = Some(o.clone());
                            if streak >= need && o.state.is_some() && stop_on(&o) {
                                self.record(&o);
                                return Ok(Look { obs: Some(o) });
                            }
                        }
                    }
                }
            } }
            if elapsed >= timeout || real.elapsed() >= timeout {
                // Report what was decided, not a provisional match: an unsettled classification is unknown.
                let obs = last.map(|mut o| {
                    if streak < need {
                        o.state = None;
                    }
                    o
                });
                if let Some(o) = &obs {
                    self.record(o);
                }
                return Ok(Look { obs });
            }
            let left = timeout.saturating_sub(elapsed).min(timeout.saturating_sub(real.elapsed()));
            if !left.is_zero() {
                self.host.sleep(poll.min(left));
            }
            elapsed += poll;
        }
    }

    fn unknown(o: Option<Observation>) -> WorkflowError {
        WorkflowError::UnknownScreen { best: o.map(|o| (o.state.unwrap_or_else(|| "unknown".into()), o.confidence)) }
    }

    /// Recognise the screen; on an unknown screen, offer the operator a manual fix and look once more.
    fn recognise(&mut self, timeout: Duration, allowed: &[String]) -> Result<Observation, WorkflowError> {
        let accept = |o: &Observation| o.state.is_some();
        let mut o = self.observe(timeout, None, accept)?.obs;
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
            o = self.observe(timeout, None, accept)?.obs;
        }
        match o {
            Some(o) if o.state.is_some() => Ok(o),
            other => Err(Self::unknown(other)),
        }
    }

    /// Right before sending keys: the screen must be `state` *now*. The boundary is taken at this moment (after any gate
    /// or decision), only frames captured after it count, and the tenant check is repeated on that look. Every burst of
    /// keys, first attempt or retry, is preceded by this.
    fn still_on(&mut self, s: &StateDef) -> Result<(), WorkflowError> {
        let here = s.name.clone();
        // The decision is now: nothing the capture received before this moment counts.
        let decided = Instant::now();
        let now = self.host.screen().map(|f| f.seq.ok_or(WorkflowError::NoFrameNumbers)).transpose()?;
        let boundary = Boundary { seq: now.max(self.seen_seq).unwrap_or(0), at: decided };
        let look = self.observe(self.opts.observe_timeout, Some(boundary), |o| o.state.is_some())?;
        match look.obs.as_ref().and_then(|o| o.state.clone()) {
            Some(st) if st == here => {}
            Some(st) => return Err(WorkflowError::Unexpected { expected: vec![here], saw: st }),
            None => return Err(Self::unknown(look.obs)),
        }
        if s.verify_tenant {
            self.verify_tenant(s)?;
        }
        Ok(())
    }

    fn enter(&mut self, to: &str, reason: &str) {
        self.ctx.visit_attempts = 0;
        self.ctx.visit_waiting = false;
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
                let mut detail =
                    format!("{k} must be {want:?}, is {}", have.map(|h| format!("{h:?}")).unwrap_or("unset".into()));
                self.secrets.scrub(&mut detail);
                self.emit(WorkflowEvent::GuardChecked { state: s.name.clone(), ok: false, detail: detail.clone() });
                return Err(WorkflowError::Guard { state: s.name.clone(), detail });
            }
        }
        if !s.require.is_empty() {
            let detail = format!("{} condition(s)", s.require.len());
            self.emit(WorkflowEvent::GuardChecked { state: s.name.clone(), ok: true, detail });
        }
        Ok(())
    }

    /// The configured tenant's branding must be on screen (the frame that classified it), and match better than any
    /// other allowed tenant's.
    fn verify_tenant(&mut self, s: &StateDef) -> Result<(), WorkflowError> {
        let expected = self.wf.config.get("tenant").cloned().unwrap_or_default();
        let mismatch = || WorkflowError::TenantMismatch { state: s.name.clone(), expected: expected.clone() };
        let frame = self.last_frame.clone().ok_or_else(mismatch)?;
        let mut scores = Vec::new();
        for (tenant, image) in &self.wf.tenants.clone() {
            let r = self.reference(image)?;
            scores.push((tenant.clone(), self.compare(&frame, &r, None), s.detect.threshold));
        }
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        if self.winner(&scores).as_deref() != Some(expected.as_str()) {
            return Err(mismatch());
        }
        let confidence = scores[0].1;
        self.emit(WorkflowEvent::TenantVerified { state: s.name.clone(), tenant: expected, confidence });
        Ok(())
    }

    fn act(&mut self, s: &StateDef, ops: &[(usize, Op)], attempt: u32) -> Result<(), WorkflowError> {
        *self.ctx.attempts.entry(s.name.clone()).or_insert(0) += 1;
        // Only keys sent count against a resume: waiting in a state that types nothing sends nothing to replay.
        if !ops.is_empty() {
            self.ctx.visit_attempts = attempt;
        }
        self.emit(WorkflowEvent::ActionAttempted {
            state: s.name.clone(),
            attempt,
            steps: ops.len(),
            dry_run: self.opts.run.dry_run,
        });
        if ops.is_empty() {
            return Ok(());
        }
        let mut host = Scrubbed { inner: &mut *self.host, secrets: &self.secrets };
        run(ops, &mut host, &self.opts.run).map_err(|e| match e {
            RunError::Cancelled => WorkflowError::Cancelled,
            error => WorkflowError::Action { state: s.name.clone(), error },
        })
    }

    /// Record a verified transition out of `from`: history, its derived facts, and entering `to`.
    fn transition(&mut self, from: &StateDef, to: &str, confidence: f64, attempts: u32, entered_at: u64, reason: &str) {
        let at = self.now();
        self.emit(WorkflowEvent::TransitionVerified {
            from: from.name.clone(),
            to: to.to_string(),
            confidence,
            duration_ms: at.saturating_sub(entered_at),
        });
        self.ctx.history.push(Transition { from: from.name.clone(), to: to.to_string(), at_ms: at, attempts, confidence });
        for (k, v) in &from.derive {
            let old = self.ctx.derived.insert(k.clone(), (v.clone(), at)).map(|(old, _)| old);
            if old.as_deref() != Some(v.as_str()) {
                self.emit(WorkflowEvent::ContextChanged { key: format!("derived.{k}"), from: old, to: v.clone() });
            }
        }
        self.enter(to, reason);
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
        let confidence = first.confidence;
        let first = first.state.expect("recognised");
        if !allowed.contains(&first) {
            return Err(match resume {
                Some(cp) => WorkflowError::ResumeMismatch { checkpoint: cp.state.clone() },
                None => WorkflowError::Unexpected { expected: allowed, saw: first },
            });
        }
        // Attempts already spent in the state the run (re)starts in.
        let mut spent = 0;
        match resume {
            None => self.enter(&first, "observed at start"),
            Some(cp) if cp.state == first => {
                self.enter(&first, "re-observed after resume");
                // Unknown (an older checkpoint) counts as everything spent in that state: ask rather than replay.
                spent = cp.attempted.unwrap_or_else(|| cp.attempts.get(&first).copied().unwrap_or(0));
                self.ctx.visit_attempts = spent;
            }
            Some(cp) if cp.attempted.unwrap_or(0) == 0 && !cp.waiting => {
                // Nothing was sent in that visit (it stopped at a guard, a gate or a look), so the screen moving on is
                // not that state's transition: no facts are recorded for it. The new state's own checks decide.
                self.enter(&first, "observed after resume; the checkpoint's state sent nothing");
            }
            Some(cp) => {
                // The screen moved on while the run was stopped: that is the checkpoint's pending transition, now
                // verified by a fresh look. Record it (and its facts) before the new state's guard.
                let from = self.wf.state(&cp.state).expect("checked").clone();
                let attempts = cp.attempted.unwrap_or(0);
                self.ctx.current = Some(cp.state.clone());
                let now = self.now();
                self.transition(&from, &first, confidence, attempts, now, "verified after resume");
            }
        }

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
            if spent > 0 && !s.retry_safe && !self.opts.run.dry_run {
                let msg = format!(
                    "The actions for {name} were already sent and not verified. Send them again? (Check the screen first.)"
                );
                let approved = self.host.confirm(&msg);
                self.emit(WorkflowEvent::HumanGateRequested { state: name.clone(), message: msg, approved });
                if !approved {
                    return Err(WorkflowError::ResumeDeclined { state: name });
                }
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
            let budget = if s.retry_safe { s.retries + 1 } else { 1 };
            let mut attempt = spent;
            if s.retry_safe && attempt >= budget {
                self.emit(WorkflowEvent::StateTimedOut { state: name.clone(), attempts: attempt });
                return Err(WorkflowError::TimedOut { state: name, attempts: attempt });
            }
            spent = 0;
            let next = loop {
                attempt += 1;
                // Every burst of keys, first or retry, follows a fresh look taken now. A state that sends nothing (done by
                // hand, or the target finishing by itself) needs no such look: the screen may already have moved on,
                // and the verification below decides whether it moved somewhere permitted.
                if s.manual.is_none() && !ops.is_empty() {
                    self.still_on(&s)?;
                }
                // Sending nothing, this visit now waits for the screen to move on (see `Checkpoint::waiting`).
                self.ctx.visit_waiting = s.manual.is_some() || ops.is_empty();
                match &s.manual {
                    // Nothing is sent, so nothing is spent: a resume never asks to "send again" a step done by hand.
                    Some(message) => self.emit(WorkflowEvent::ManualStepRequested {
                        state: name.clone(),
                        message: message.clone(),
                        timeout_ms: s.timeout.as_millis() as u64,
                    }),
                    None => self.act(&s, ops, attempt)?,
                }
                // Everything captured up to now predates the action's effect.
                let sent = Instant::now();
                let boundary = self
                    .host
                    .screen()
                    .and_then(|f| f.seq)
                    .or(self.seen_seq)
                    .map(|seq| Boundary { seq, at: sent });
                // Verify: the screen must settle on a permitted next state. Staying put is "not yet"; anything else
                // stops the run.
                let here = name.clone();
                let look = self.observe(s.timeout, boundary, |o| o.state.as_ref().is_some_and(|st| *st != here))?;
                match look.obs.as_ref().and_then(|o| o.state.clone().map(|st| (st, o.confidence))) {
                    Some((st, conf)) if st != name && s.next.contains(&st) => break (st, conf),
                    Some((st, _)) if st != name => {
                        return Err(WorkflowError::Unexpected { expected: s.next.clone(), saw: st });
                    }
                    // Settled, freshly, on the same screen: the action did not take. Retry only where marked safe
                    // and within the budget, after the tenant check again.
                    Some(_) => {
                        if attempt < budget {
                            continue;
                        }
                        self.emit(WorkflowEvent::StateTimedOut { state: name.clone(), attempts: attempt });
                        return Err(WorkflowError::TimedOut { state: name, attempts: attempt });
                    }
                    // Unknown, unsettled or no fresh frames: never type into what cannot be seen.
                    None => {
                        self.emit(WorkflowEvent::StateTimedOut { state: name.clone(), attempts: attempt });
                        return Err(Self::unknown(look.obs));
                    }
                }
            };
            let (to, confidence) = next;
            self.transition(&s, &to, confidence, attempt, entered_at, "transition verified");
        }
    }
}

/// The most a single key may be held, or a gap between keys may last. Typing is the action itself; these timings
/// only shape it, and a long one would be a pause the engine does not look through.
pub const MAX_KEY_TIMING: Duration = Duration::from_millis(100);

/// Checks before a run that need no screen: timing options, the pause budget of every state, and that no prompted
/// secret's value appears anywhere in the workflow or the checkpoint (both are published: inspector, trace, exports).
fn precheck(wf: &Workflow, secrets: &Secrets, opts: &WorkflowOptions, resume: Option<&Checkpoint>) -> Result<(), WorkflowError> {
    // Privacy first: no other check may report anything from the file before this one has passed.
    if let Some(place) = secret_place(wf, secrets, resume) {
        return Err(WorkflowError::SecretInFile { place });
    }
    if opts.run.key_hold > MAX_KEY_TIMING || opts.run.key_gap > MAX_KEY_TIMING {
        return Err(WorkflowError::Options("key hold and key gap must be at most 100 ms in a workflow".into()));
    }
    Ok(())
}

/// Every string a workflow (and a checkpoint) holds, field by field (the real values, not a formatted rendering that
/// would escape quotes), with a location that is built only from positions, never from the file's own text.
fn secret_place(wf: &Workflow, secrets: &Secrets, resume: Option<&Checkpoint>) -> Option<String> {
    fn steps<'a>(out: &mut Vec<&'a str>, list: &'a [Step]) {
        for s in list {
            match s {
                Step::Text(t) | Step::Key(t) | Step::Confirm(t) | Step::Click(t) | Step::Comment(t) => out.push(t),
                Step::SecretText(t) => out.push(t.as_str()),
                Step::Chord(keys) => out.extend(keys.iter().map(String::as_str)),
                Step::Repeat { steps: inner, .. } => steps(out, inner),
                Step::Wait(crate::model::WaitSpec::Screen { image, .. }) => out.push(image),
                Step::Wait(_) | Step::Delay(_) | Step::MoveMouse { .. } => {}
            }
        }
    }
    let hit = |vals: &[&str]| vals.iter().any(|v| secrets.appears_in(v));
    if hit(&[&wf.name, &wf.description]) {
        return Some("the workflow's name or description".into());
    }
    if wf.start.iter().any(|v| secrets.appears_in(v)) {
        return Some("the start states".into());
    }
    for (i, (k, v)) in wf.config.iter().enumerate() {
        if hit(&[k, v]) {
            return Some(format!("config entry {}", i + 1));
        }
    }
    for (i, (k, v)) in wf.secrets.iter().enumerate() {
        if hit(&[k, v]) {
            return Some(format!("secret declaration {}", i + 1));
        }
    }
    for (i, (k, v)) in wf.tenants.iter().enumerate() {
        if hit(&[k, v]) {
            return Some(format!("tenant {}", i + 1));
        }
    }
    for (i, st) in wf.states.iter().enumerate() {
        let mut vals: Vec<&str> = vec![&st.name, &st.description];
        vals.extend(st.detect.screens.iter().map(String::as_str));
        vals.extend(st.next.iter().map(String::as_str));
        vals.extend(st.require.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]));
        vals.extend(st.derive.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]));
        vals.extend(st.gate.iter().map(String::as_str));
        vals.extend(st.manual.iter().map(String::as_str));
        steps(&mut vals, &st.actions);
        if hit(&vals) {
            return Some(format!("state {} (in file order)", i + 1));
        }
    }
    if let Some(cp) = resume {
        let mut vals: Vec<&str> = vec![&cp.workflow, &cp.state];
        vals.extend(cp.derived.iter().flat_map(|(k, v)| [k.as_str(), v.as_str()]));
        vals.extend(cp.attempts.keys().map(String::as_str));
        vals.extend(cp.history.iter().flat_map(|t| [t.from.as_str(), t.to.as_str()]));
        if hit(&vals) {
            return Some("the checkpoint".into());
        }
    }
    None
}

/// The longest a state's actions may take to send, all told: every key's hold and gap, every delay, the default
/// delay after each step, repeats expanded. The engine does not look at the screen while they run, so this bounds
/// how stale its last look can be by the final key.
pub const MAX_ACTION_TIME: Duration = Duration::from_secs(5);

/// How long sending `ops` takes with these timings (an upper bound, ignoring the host's own latency).
pub fn action_time(ops: &[(usize, Op)], run: &RunOptions, top: bool) -> Duration {
    let tap = run.key_hold.saturating_add(run.key_gap);
    ops.iter().fold(Duration::ZERO, |acc, (_, op)| {
        let t = match op {
            // A shifted character is a shift press around the tap: count it as two taps.
            Op::Type { strokes, .. } => tap.saturating_mul(strokes.iter().map(|s| if s.shift { 2 } else { 1 }).sum::<u32>()),
            Op::Key(_) | Op::Chord(_) | Op::Click(_) => tap,
            Op::Delay(d) => *d,
            Op::Repeat { count, ops } => action_time(ops, run, false).saturating_mul(*count),
            Op::Wait(_) | Op::Confirm(_) | Op::Move(..) | Op::Comment => Duration::ZERO,
        };
        let after = if top { run.default_delay } else { Duration::ZERO };
        acc.saturating_add(t).saturating_add(after)
    })
}

/// Run a workflow. `secrets` holds the prompted secret values; they are compiled into key strokes and otherwise kept
/// only to scrub them from published text, then wiped. `sink` sees every trace entry as it happens (for a live view).
/// Always releases every key at the end, unless dry-running.
pub fn run_workflow(
    wf: &Workflow,
    secrets: &Vars,
    layout: &dyn Layout,
    host: &mut dyn Host,
    opts: &WorkflowOptions,
    resume: Option<&Checkpoint>,
    sink: &mut dyn FnMut(&TraceEntry, &Context),
) -> WorkflowReport {
    let secret_values = Secrets(wf.secrets.keys().filter_map(|k| secrets.get(k).cloned()).collect());
    // Checks that need no screen and must come before anything is recorded or shown: the context, the trace and the
    // sink all publish what the file says.
    if let Err(err) = precheck(wf, &secret_values, opts, resume) {
        if !opts.run.dry_run {
            let _ = host.release_all();
        }
        let entry = TraceEntry { at_ms: 0, event: WorkflowEvent::Stopped { state: None, reason: err.to_string() } };
        sink(&entry, &Context::default());
        return WorkflowReport { result: Err(err), trace: vec![entry], context: Context::default() };
    }
    let mut ctx = Context { config: wf.config.clone(), ..Default::default() };
    if let Some(cp) = resume {
        ctx.derived = cp.derived.iter().map(|(k, v)| (k.clone(), (v.clone(), 0))).collect();
        ctx.attempts = cp.attempts.clone();
        ctx.history = cp.history.clone();
    }
    let mut e = Engine {
        wf,
        host,
        opts,
        refs: BTreeMap::new(),
        last_frame: None,
        seen_seq: None,
        secrets: secret_values,
        ctx,
        trace: Vec::new(),
        sink,
        started: Instant::now(),
    };
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
        for (i, st) in wf.states.iter().enumerate() {
            if action_time(&compiled[&st.name], &opts.run, true) > MAX_ACTION_TIME {
                return Err(WorkflowError::Options(format!(
                    "state {} (in file order): its actions would take more than 5s to send, during which nothing is                      watched; split them across states",
                    i + 1
                )));
            }
        }
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
    if let Err(err) = &mut result {
        // Host diagnostics are free text: scrub them in the returned error as well as in the trace.
        match err {
            WorkflowError::Reference(m) | WorkflowError::Action { error: RunError::Host(m), .. } => e.secrets.scrub(m),
            _ => {}
        }
        let state = e.ctx.current.clone();
        e.emit(WorkflowEvent::Stopped { state, reason: err.to_string() });
    }
    WorkflowReport { result, trace: e.trace, context: e.ctx }
}

// ---------- export ----------

/// One JSON object per trace entry. Events never hold typed text or secret values, and prompted secret values are
/// scrubbed from any free text (gate messages, host diagnostics) before it is recorded. A value an author writes into
/// the workflow file itself (a gate message, a config value) is not a secret to this engine: keep secrets under
/// `[secrets]`.
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
    // A run refused because the file itself holds a secret's value must not repeat any of the file's own text.
    let withheld = matches!(report.result, Err(WorkflowError::SecretInFile { .. }));
    let title = if withheld {
        "(name withheld: the file contains a secret's value)"
    } else if wf.name.is_empty() {
        "(unnamed)"
    } else {
        &wf.name
    };
    let mut md = format!("### Workflow: {title}\n\n");
    let stopped = report.trace.iter().rev().find_map(|t| match &t.event {
        WorkflowEvent::Stopped { reason, .. } => Some(reason.clone()),
        _ => None,
    });
    md.push_str(&match (&report.result, stopped) {
        (Ok(()), _) => format!("**Completed** in state `{}`.\n\n", report.context.current.as_deref().unwrap_or("?")),
        (Err(_), Some(reason)) => format!("**Stopped:** {reason}\n\n"),
        (Err(_), None) => "**Stopped.**\n\n".to_string(),
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
    md.push_str("\nPrompted secrets are never recorded. Screens were matched against reference images; this is not a hardware verification record.\n");
    md
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
