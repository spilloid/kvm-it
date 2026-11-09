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

/// How a state is recognised on screen: by the phrases it shows, read off the screen (when the computer can read
/// text), or else by comparing it with reference pictures.
#[derive(Debug, Clone, PartialEq)]
pub struct Detect {
    /// Reference images (relative to the workflow file); the best match counts. Optional when `text` is given.
    pub screens: Vec<String>,
    /// Minimum similarity to count as this state.
    pub threshold: f64,
    /// Phrases the screen shows (in `region`, if given), as alternatives: the screen matches when every phrase of any
    /// one set is found (a screen Windows words differently from build to build gets one set per wording). A state
    /// with phrases is recognised by them whenever text can be read, and its pictures are not used.
    pub text: Vec<Vec<String>>,
    /// Minimum [`crate::text::phrase_score`] each phrase needs.
    pub text_threshold: f64,
    /// Compare only this part of the screen: `[x, y, width, height]` as fractions of the frame (both the capture and
    /// the reference are cropped the same way).
    pub region: Option<[f64; 4]>,
}

impl Detect {
    /// How well the screen's text (`lines`, read off the whole frame) shows this state's phrases, and the best set: a set
    /// scores its weakest phrase (every one must be there), the state its best set. `None` without phrases.
    pub fn text_score(&self, lines: &[crate::text::TextLine]) -> Option<(f64, &Vec<String>)> {
        let shown = crate::text::screen_text(lines, self.region);
        self.text
            .iter()
            .map(|set| (set.iter().map(|p| crate::text::phrase_score(&shown, p)).fold(f64::INFINITY, f64::min), set))
            .max_by(|a, b| a.0.total_cmp(&b.0))
    }
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
    /// After `actions`, read the screen back: the value of this secret (or config value) must show as typed, exactly,
    /// before `submit` is sent. Letters with their case swapped mean Caps Lock is on at the target; anything else, a
    /// different keyboard layout there or a field that did not take the text. Either stops the run before `submit`.
    pub verify_typed: Option<String>,
    /// Sent once `verify_typed` has seen the text (only with it): pressing Enter on what was checked.
    pub submit: Vec<Step>,
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
    /// Used by the tenant check only when it cannot read text, or the profile has no `tenant_domain`.
    pub tenants: BTreeMap<String, String>,
    /// The secret typed as the user's sign-in (`name@domain`). With `config.tenant_domain`, the tenant check reads the
    /// page: the sign-in must be an account in that domain before anything is typed, and the password page must show
    /// it before the password is.
    pub account: Option<String>,
    /// The profile values a person may set (in the app's setup form or `kvmit workflow new|set`), in file order. Nothing
    /// else in the file is changed that way: states, actions and guards stay as written.
    pub setup: Vec<SetupField>,
    /// A classification must beat the runner-up by at least this much similarity.
    pub ambiguity_margin: f64,
    pub matching: Matching,
    pub states: Vec<StateDef>,
}

/// One settable profile value: a `[config]` key with a label, optional help, and either a fixed list of choices or
/// free text. The `tenant` key, when the workflow lists tenants, is the organisation's name: setting it to a new name
/// renames the configured tenant and points it at `tenant-<name>.png` beside the old picture.
#[derive(Debug, Clone, PartialEq)]
pub struct SetupField {
    pub key: String,
    pub label: String,
    pub help: String,
    /// Empty: free text.
    pub choices: Vec<SetupChoice>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetupChoice {
    pub value: String,
    pub label: String,
}

impl SetupField {
    /// Whether `value` may be set: one of the choices, or (free text) 1-64 printable characters; an organisation name
    /// is letters, digits, `-` and `_` only, since it names a file.
    pub fn check(&self, value: &str) -> Result<(), String> {
        if !self.choices.is_empty() && value.is_empty() {
            return Err(format!("{}: choose one", self.label));
        }
        if !self.choices.is_empty() {
            return match self.choices.iter().any(|c| c.value == value) {
                true => Ok(()),
                false => Err(format!(
                    "{}: {value:?} is not one of {}",
                    self.key,
                    self.choices.iter().map(|c| c.value.as_str()).collect::<Vec<_>>().join(", ")
                )),
            };
        }
        if self.key == "tenant" {
            if value.is_empty() || value.len() > 64 || !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                return Err("the organisation's name is 1-64 letters, digits, - and _ (it names its picture's file)".into());
            }
            return Ok(());
        }
        if !setup_text(value) {
            return Err(format!("{}: 1-64 characters, no line breaks or control characters", self.key));
        }
        Ok(())
    }
}

fn setup_text(s: &str) -> bool {
    !s.trim().is_empty() && s.chars().count() <= 64 && !s.chars().any(char::is_control)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetup {
    key: String,
    label: String,
    #[serde(default)]
    help: String,
    #[serde(default)]
    choices: Vec<RawChoice>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawChoice {
    Plain(String),
    Labelled { value: String, label: String },
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
    #[serde(default)]
    text: RawText,
    #[serde(default)]
    text_threshold: Option<f64>,
}

/// `text = ["a", "b"]` (one set: all of them) or `text = [["a"], ["b", "c"]]` (any one set).
#[derive(Deserialize)]
#[serde(untagged)]
enum RawText {
    One(Vec<String>),
    Any(Vec<Vec<String>>),
}

impl Default for RawText {
    fn default() -> Self {
        RawText::One(Vec::new())
    }
}

impl RawText {
    fn sets(self) -> Vec<Vec<String>> {
        match self {
            RawText::One(v) if v.is_empty() => Vec::new(),
            RawText::One(v) => vec![v],
            RawText::Any(v) => v,
        }
    }
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
    verify_typed: Option<String>,
    #[serde(default)]
    submit: Vec<RawStep>,
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
    account: Option<String>,
    #[serde(default)]
    setup: Vec<RawSetup>,
    #[serde(default)]
    ambiguity_margin: Option<f64>,
    #[serde(default, rename = "match")]
    matching: Option<String>,
    states: Vec<RawState>,
}

impl Workflow {
    /// Of these states, the ones whose entry guard the profile already fails (on `[config]` alone): states that exist
    /// so the run stops there, such as the other deployment method's screen or a licence the profile does not accept.
    /// Without a picture of such a screen, that stop is not made: the screen may be taken for a similar state.
    pub fn stop_states<'a>(&self, states: &'a [String]) -> Vec<&'a str> {
        states
            .iter()
            .filter(|n| {
                self.state(n).is_some_and(|s| s.require.iter().any(|(k, v)| self.config.get(k).is_some_and(|c| c != v)))
            })
            .map(String::as_str)
            .collect()
    }

    /// The prompted secrets (name, prompt) in the order the run types them (first use, states in file order), then any
    /// not typed at all: what the app shows and the command line asks, so the sign-in comes before the password.
    pub fn secrets_in_order(&self) -> Vec<(&str, &str)> {
        // `{{ name }}` placeholders of a template, in order (spaces allowed, as the compiler allows them).
        fn names(t: &str) -> Vec<&str> {
            let mut out = Vec::new();
            let mut rest = t;
            while let Some(i) = rest.find("{{") {
                let after = &rest[i + 2..];
                let Some(j) = after.find("}}") else { break };
                out.push(after[..j].trim());
                rest = &after[j + 2..];
            }
            out
        }
        fn walk<'a>(steps: &'a [crate::model::Step], used: &mut Vec<&'a str>) {
            for step in steps {
                match step {
                    crate::model::Step::SecretText(t) => used.extend(names(t)),
                    crate::model::Step::Repeat { steps, .. } => walk(steps, used),
                    _ => {}
                }
            }
        }
        let mut used = Vec::new();
        for st in &self.states {
            walk(&st.actions, &mut used);
        }
        let mut out: Vec<(&str, &str)> = Vec::new();
        for name in used {
            if let Some((k, p)) = self.secrets.get_key_value(name) {
                if !out.iter().any(|(o, _)| o == k) {
                    out.push((k, p));
                }
            }
        }
        for (k, p) in &self.secrets {
            if !out.iter().any(|(o, _)| o == k) {
                out.push((k, p));
            }
        }
        out
    }
}

const MAX_STATES: usize = 64;
const MAX_SETUP: usize = 16;

/// `[[setup]]`: each names a `[config]` key once, with a label; choices are distinct and include the configured value.
fn setup_fields(
    raw: Vec<RawSetup>,
    config: &BTreeMap<String, String>,
    tenants: &BTreeMap<String, String>,
) -> Result<Vec<SetupField>, String> {
    if raw.len() > MAX_SETUP {
        return Err(format!("at most {MAX_SETUP} setup values"));
    }
    let mut out: Vec<SetupField> = Vec::new();
    for r in raw {
        let Some(current) = config.get(&r.key) else {
            return Err(format!("setup value {:?} is not in [config]", r.key));
        };
        if out.iter().any(|f| f.key == r.key) {
            return Err(format!("setup value {:?} is listed twice", r.key));
        }
        if r.label.trim().is_empty() || r.label.chars().count() > 80 {
            return Err(format!("setup value {:?}: its label is 1-80 characters", r.key));
        }
        if r.help.chars().count() > 400 {
            return Err(format!("setup value {:?}: its help is at most 400 characters", r.key));
        }
        if r.key == "tenant" && !tenants.is_empty() && !r.choices.is_empty() {
            return Err("the tenant setup value is the organisation's name: it takes no choices".into());
        }
        let mut choices: Vec<SetupChoice> = Vec::new();
        for c in r.choices {
            let c = match c {
                RawChoice::Plain(v) => SetupChoice { label: v.clone(), value: v },
                RawChoice::Labelled { value, label } => SetupChoice { value, label },
            };
            if !setup_text(&c.value) || c.label.trim().is_empty() || c.label.chars().count() > 80 {
                return Err(format!("setup value {:?}: a choice is 1-64 characters, its label 1-80", r.key));
            }
            if choices.iter().any(|d| d.value == c.value) {
                return Err(format!("setup value {:?}: choice {:?} is listed twice", r.key, c.value));
            }
            choices.push(c);
        }
        let field = SetupField { key: r.key, label: r.label, help: r.help, choices };
        // An empty value of a choice is "not chosen yet": the file is valid, but no run starts until it is chosen.
        if !(current.is_empty() && !field.choices.is_empty()) {
            field.check(current).map_err(|e| format!("[config] does not fit its setup: {e}"))?;
        }
        out.push(field);
    }
    Ok(out)
}
const MAX_RETRIES: u32 = 5;
const MAX_PHRASES: usize = 8;
/// The `[config]` key naming the organisation's sign-in domain (see [`Workflow::account`]).
pub const TENANT_DOMAIN: &str = "tenant_domain";

/// `name@domain` exactly as it will be typed: printable ASCII with no spaces, one `@`, a name before it, and the domain
/// after it equal to `domain` (case aside). A newline or tab would be typed as Enter or Tab before any check.
pub fn account_in(account: &str, domain: &str) -> bool {
    let Some((local, dom)) = account.split_once('@') else { return false };
    !local.is_empty() && crate::text::is_address(account) && !local.ends_with('.') && dom.eq_ignore_ascii_case(domain)
}

/// `contoso.com`, `mail.contoso.co.uk`: letters, digits and hyphens in dot-separated labels, at least two labels.
pub fn valid_domain(d: &str) -> bool {
    let labels: Vec<&str> = d.split('.').collect();
    d.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}
/// How closely each detect phrase must be found: one misread character in ten still matches.
pub const DEFAULT_TEXT_THRESHOLD: f64 = 0.9;
/// The largest side of a picture handed to a text reader once enlarged.
const MAX_READ_SIDE: usize = 2600;
/// The most `WorkflowOptions::evidence_age` may be: beyond it a check on a picture says little about the screen now.
const MAX_EVIDENCE_AGE: Duration = Duration::from_secs(30);
/// How long a typed value has to appear on screen before `submit` (it is drawn as typed, so this is generous).
const TYPED_WAIT: Duration = Duration::from_secs(5);

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
        if let Some(a) = &raw.account {
            if !raw.secrets.contains_key(a) {
                return Err(format!("account {a:?} must name a secret (the sign-in typed)"));
            }
        }
        if let Some(d) = raw.config.get(TENANT_DOMAIN) {
            if !valid_domain(d) {
                return Err(format!("config {TENANT_DOMAIN} {d:?} must be a domain like contoso.com (the part after @)"));
            }
            if raw.account.is_none() {
                return Err(format!("config {TENANT_DOMAIN} needs `account`: the secret typed as the sign-in"));
            }
        }
        let setup = setup_fields(raw.setup, &raw.config, &raw.tenants)?;
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
            let text = r.detect.text.sets();
            if screens.is_empty() && text.is_empty() {
                return Err(format!("state {:?} needs detect text or at least one detect screen", r.name));
            }
            if text.iter().any(Vec::is_empty) {
                return Err(format!("state {:?}: a detect text set needs at least one phrase", r.name));
            }
            if text.iter().map(Vec::len).sum::<usize>() > MAX_PHRASES {
                return Err(format!("state {:?}: at most {MAX_PHRASES} detect phrases", r.name));
            }
            if let Some(t) = text.iter().flatten().find(|t| {
                crate::text::normalize(t).chars().count() < 3 || t.chars().count() > 200 || t.contains('\n')
            }) {
                return Err(format!(
                    "state {:?}: detect phrase {t:?} must be one line of 3 to 200 characters (letters or digits)",
                    r.name
                ));
            }
            let text_threshold = r.detect.text_threshold.unwrap_or(DEFAULT_TEXT_THRESHOLD);
            if !(0.6..=1.0).contains(&text_threshold) {
                return Err(format!("state {:?}: text_threshold must be within 0.6..=1", r.name));
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
            if r.verify_tenant && raw.tenants.is_empty() && !raw.config.contains_key(TENANT_DOMAIN) {
                return Err(format!(
                    "state {:?} verifies the tenant but the workflow lists no tenants and config has no {TENANT_DOMAIN}",
                    r.name
                ));
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
            let submit: Vec<Step> = r.submit.into_iter().map(step_from_raw).collect::<Result<_, _>>()?;
            check_actions(&r.name, &submit, &raw.secrets)?;
            match &r.verify_typed {
                Some(v) if !raw.secrets.contains_key(v) && !raw.config.contains_key(v) => {
                    return Err(format!("state {:?}: verify_typed {v:?} must name a secret or a config value", r.name))
                }
                Some(_) if actions.is_empty() || submit.is_empty() => {
                    return Err(format!(
                        "state {:?}: verify_typed needs `actions` (that type it) and `submit` (sent once it shows)",
                        r.name
                    ))
                }
                None if !submit.is_empty() => {
                    return Err(format!("state {:?}: `submit` is sent after verify_typed, which the state lacks", r.name))
                }
                _ => {}
            }
            if total_pause(&actions) + total_pause(&submit) > MAX_ACTION_PAUSE {
                return Err(format!(
                    "state {:?}: its actions pause for more than 2s in total; wait in a state instead, so the engine looks again",
                    r.name
                ));
            }
            states.push(StateDef {
                timeout,
                actions,
                verify_typed: r.verify_typed,
                submit,
                name: r.name,
                description: r.description,
                detect: Detect { screens, threshold, region: r.detect.region, text, text_threshold },
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
        let text_only = states.iter().all(|s| s.detect.screens.is_empty()) && raw.tenants.is_empty();
        Ok(Workflow {
            name: raw.name,
            description: raw.description,
            start: raw.start,
            config: raw.config,
            secrets: raw.secrets.into_iter().map(|(k, v)| (k, v.prompt)).collect(),
            tenants: raw.tenants,
            account: raw.account,
            setup,
            ambiguity_margin: margin,
            matching: match raw.matching.as_deref() {
                // Only pictures are compared this way: a workflow recognising every screen by text needs no `match`.
                None if text_only => Matching::Structure,
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

    /// The setup choices not made yet (an empty value): a built-in workflow leaves the consequential ones for its
    /// user to choose, and no run starts until they are.
    pub fn unchosen(&self) -> Vec<&SetupField> {
        self.setup.iter().filter(|f| !f.choices.is_empty() && self.config.get(&f.key).is_some_and(|v| v.is_empty())).collect()
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
    /// The reference image that matched best, or `text: <phrase>` when the state was recognised by its phrases.
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

impl Context {
    /// Where a run is, as a checkpoint: what a resume needs. Taken during a run (a journal, in case the run never
    /// ends cleanly) as well as at its end.
    pub fn checkpoint(&self, wf: &Workflow) -> Option<Checkpoint> {
        let state = self.current.clone()?;
        Some(Checkpoint {
            workflow: wf.name.clone(),
            state,
            derived: self.derived.iter().map(|(k, (v, _))| (k.clone(), v.clone())).collect(),
            attempts: self.attempts.clone(),
            attempted: Some(self.visit_attempts),
            waiting: self.visit_waiting,
            history: self.history.clone(),
        })
    }
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
    /// States that cannot be recognised in this run because none of their pictures is saved: if one of those screens
    /// appears, it is an unknown screen and the run stops there.
    PicturesMissing { states: Vec<String>, stops: Vec<String> },
    StateObserved { observation: Observation },
    StateEntered { state: String, from: Option<String>, reason: String },
    GuardChecked { state: String, ok: bool, detail: String },
    TenantVerified { state: String, tenant: String, confidence: f64 },
    /// What was typed for `key` shows on screen as typed (the value itself is never recorded).
    TypedVerified { state: String, key: String },
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
            // Defence in depth: the file was scanned for secrets, but what names a match is published here.
            WorkflowEvent::StateObserved { observation } => {
                if let Some(r) = observation.reference.as_mut() {
                    secrets.scrub(r);
                }
            }
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
    fn reference_missing(&mut self, name: &str) -> bool {
        self.inner.reference_missing(name)
    }
    fn read_text(&mut self, frame: &Frame) -> Result<Vec<crate::text::TextLine>, String> {
        self.inner.read_text(frame)
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
    /// Reading the screen's text failed, was too slow, or is not possible here for a check that needs it.
    Text(String),
    /// The screen could not be classified with confidence.
    UnknownScreen { best: Option<(String, f64)> },
    /// The screen showed a state the workflow does not allow here.
    Unexpected { expected: Vec<String>, saw: String },
    Guard { state: String, detail: String },
    TenantMismatch { state: String, expected: String },
    /// The page does not show the sign-in typed, as an account in the profile's domain.
    AccountNotShown { state: String, domain: String },
    /// What was typed does not show as typed: `caps_lock` when it showed with its letters' case swapped.
    TypedNotShown { state: String, caps_lock: bool },
    GateDeclined { state: String },
    TimedOut { state: String, attempts: u32 },
    ResumeMismatch { checkpoint: String },
    /// Resuming would send actions that were already sent and not verified; the operator did not agree.
    ResumeDeclined { state: String },
    /// The video source does not number its frames, so a stalled capture cannot be told from a still screen.
    NoFrameNumbers,
    /// The workflow file itself contains the value of a prompted secret (where is named, never the value).
    SecretInFile { place: String },
    /// The laptop's name contains the value of a prompted secret (it is saved with checkpoints and run records).
    SecretInName,
    /// A live run's progress could not be recorded (its journal), so nothing more was sent.
    Journal(String),
    Options(String),
    Action { state: String, error: RunError },
    Cancelled,
}

impl std::fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkflowError::Compile { state, error } => write!(f, "state {state}: {error}"),
            WorkflowError::Reference(s) => write!(f, "cannot load reference image: {s}"),
            WorkflowError::Text(s) => write!(f, "{s}"),
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
            WorkflowError::AccountNotShown { state, domain } => write!(
                f,
                "state {state}: the page does not show the sign-in entered (an account in {domain}); stopped before the password"
            ),
            WorkflowError::TypedNotShown { state, caps_lock: true } => write!(
                f,
                "state {state}: what was typed shows with upper and lower case swapped: Caps Lock looks on at the target. Turn it off, clear the field, and run again; nothing more was sent"
            ),
            WorkflowError::TypedNotShown { state, caps_lock: false } => write!(
                f,
                "state {state}: what was typed does not show on screen as typed (the target may use another keyboard layout than US, or the field did not take it); nothing more was sent"
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
            WorkflowError::SecretInName => write!(
                f,
                "the laptop's name contains the value of a prompted secret; name the laptop differently"
            ),
            WorkflowError::Journal(m) => {
                write!(f, "could not record the run's progress ({m}); stopped before sending anything more")
            }
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
    /// Run although some states have no saved picture (they cannot be recognised). Off by default: such a screen is
    /// usually unknown, but may be taken for a similar state. A dry run never needs it.
    pub allow_missing_pictures: bool,
    /// With `allow_missing_pictures`: exactly which states were accepted as unrecognisable (the app names them when
    /// you tick the box). A run that finds any other state without a picture is refused. `None`: any (the terminal's
    /// `--allow-missing-pictures`, which lists them first).
    pub allow_missing_only: Option<BTreeSet<String>>,
    /// The oldest a frame may be (from capture to the end of reading it) when a check on it decides that keys are
    /// sent: the account shown before a password, a value typed before Enter. 3 s by default; a slow capture
    /// (a VM's screendump over SSH) needs more.
    pub evidence_age: Duration,
    /// The laptop this run is on, as the operator named it (empty: not named). It is saved with the journal, the
    /// checkpoint and the run record, so it must not contain a prompted secret's value.
    pub laptop: String,
    /// How long a burst of keys may go on, by the clock, once it starts after the last look (5 s by default): a slow
    /// link stops it part-way rather than typing on into whatever the screen became.
    pub action_deadline: Duration,
}

impl Default for WorkflowOptions {
    fn default() -> Self {
        WorkflowOptions {
            run: RunOptions::default(),
            observe_timeout: Duration::from_secs(30),
            settle_frames: 2,
            offer_manual_recovery: true,
            capture_latency: Duration::from_millis(300),
            evidence_age: Duration::from_secs(3),
            allow_missing_pictures: false,
            allow_missing_only: None,
            laptop: String::new(),
            action_deadline: Duration::from_secs(5),
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
        self.context.checkpoint(wf)
    }
}

/// Where a state's compiled `submit` steps are kept beside its actions (state names cannot contain `/`).
pub fn submit_key(state: &str) -> String {
    format!("{state}/submit")
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
        for (key, steps) in [(s.name.clone(), &s.actions), (submit_key(&s.name), &s.submit)] {
            let script = Script { name: s.name.clone(), description: String::new(), vars: defs.clone(), steps: steps.clone() };
            let ops = compile(&script, &vars, layout).map_err(|error| WorkflowError::Compile { state: s.name.clone(), error })?;
            out.insert(key, ops);
        }
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
    /// Reference pictures not saved yet (see [`Host::reference_missing`]): never compared.
    missing: BTreeSet<String>,
    /// Why text cannot be read on this computer, if it cannot (probed once, before the first look).
    text_unreadable: Option<String>,
    /// The sign-in typed (the `account` secret), for the tenant check; empty when there is none. Wiped on drop.
    account: Secrets,
    /// The values states read back after typing (`verify_typed`), by name. Wiped on drop.
    typed: BTreeMap<String, Secrets>,
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

    /// Before the first look: which pictures are not saved yet, each probed once (one snapshot, so states sharing a
    /// picture always agree). The tenant check needs every allowed tenant's picture: a missing competitor could let the
    /// wrong organisation's page pass. A state none of whose pictures is saved cannot be recognised, and its screen, if
    /// it appears, is usually unknown (the run stops) but may be taken for a similar state: a live run accepts that
    /// only with `allow_missing_pictures`; a dry run, which sends nothing, always does.
    fn note_missing_pictures(&mut self) -> Result<(), WorkflowError> {
        let wf = self.wf;
        let reads_account = wf.config.contains_key(TENANT_DOMAIN) && wf.states.iter().any(|s| s.verify_tenant);
        if reads_account || wf.states.iter().any(|s| !s.detect.text.is_empty() || s.verify_typed.is_some()) {
            // A blank frame tells whether this computer reads text at all, before any screen depends on it.
            let blank = Frame::new(64, 36, vec![255; 64 * 36]).expect("valid size");
            self.text_unreadable = self.host.read_text(&blank).err();
        }
        let names: BTreeSet<&String> = wf.states.iter().flat_map(|s| &s.detect.screens).chain(wf.tenants.values()).collect();
        for n in names {
            if self.host.reference_missing(n) {
                self.missing.insert(n.clone());
            }
        }
        // Checks on the sign-in read the screen: they are never waived because it cannot be read here.
        if let (Some(why), false) = (&self.text_unreadable, self.opts.run.dry_run) {
            let reads_sign_in = wf.states.iter().any(|s| s.verify_typed.is_some())
                || (wf.config.contains_key(TENANT_DOMAIN) && wf.states.iter().any(|s| s.verify_tenant));
            if reads_sign_in {
                let mut why = why.clone();
                self.secrets.scrub(&mut why);
                return Err(WorkflowError::Text(format!(
                    "this workflow checks the sign-in by reading the screen, and text cannot be read here ({why}); nothing was sent"
                )));
            }
        }
        if wf.states.iter().any(|s| s.verify_tenant) && self.tenant_by_picture() && wf.tenants.is_empty() {
            return Err(WorkflowError::Reference(format!(
                "the tenant check reads the password page, but text cannot be read here ({}) and the workflow lists no tenant picture",
                self.text_unreadable.as_deref().unwrap_or("no recogniser")
            )));
        }
        if wf.states.iter().any(|s| s.verify_tenant) && self.tenant_by_picture() {
            if let Some((tenant, image)) = wf.tenants.iter().find(|(_, i)| self.missing.contains(*i)) {
                return Err(WorkflowError::Reference(format!(
                    "{image}, the picture of tenant {tenant}'s sign-in, is not saved: the tenant check compares every allowed tenant's picture before the password is typed"
                )));
            }
        }
        let blind: Vec<String> = wf.states.iter().filter(|s| self.is_blind(s)).map(|s| s.name.clone()).collect();
        if blind.len() == wf.states.len() {
            return Err(WorkflowError::Reference(match &self.text_unreadable {
                Some(why) => format!("no screen can be recognised: text cannot be read here ({why}) and no reference picture is saved"),
                None => "no reference picture is saved yet: save the screens first".into(),
            }));
        }
        if !blind.is_empty() {
            let stops: Vec<String> = wf.stop_states(&blind).into_iter().map(str::to_string).collect();
            let beyond: Vec<&String> = match &self.opts.allow_missing_only {
                Some(ok) => blind.iter().filter(|b| !ok.contains(*b)).collect(),
                None => Vec::new(),
            };
            if !self.opts.run.dry_run && self.opts.allow_missing_pictures && !beyond.is_empty() {
                return Err(WorkflowError::Reference(format!(
                    "{} also cannot be recognised now (its picture is gone), and running without it was not accepted",
                    beyond.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                )));
            }
            if !self.opts.run.dry_run && !self.opts.allow_missing_pictures {
                let lost = match stops.is_empty() {
                    true => String::new(),
                    false => format!(
                        "; this profile stops the run at {}, and without their pictures it may not",
                        stops.join(", ")
                    ),
                };
                return Err(WorkflowError::Reference(format!(
                    "no picture is saved for {}: save them, or run without them (a screen without a picture is usually stopped at as unknown, but may be taken for a similar one){lost}",
                    blind.join(", ")
                )));
            }
            self.emit(WorkflowEvent::PicturesMissing { states: blind, stops });
        }
        Ok(())
    }

    /// Whether the tenant check compares branding pictures: when the profile names no sign-in domain (or, in a dry run
    /// only, no text can be read here). Otherwise it reads the page.
    fn tenant_by_picture(&self) -> bool {
        // With a sign-in domain the page is read (a run that cannot read it does not start, see above); a dry run
        // that cannot read falls back to pictures, as it types nothing.
        !self.wf.config.contains_key(TENANT_DOMAIN) || self.text_unreadable.is_some()
    }

    /// A state that cannot be recognised this run: no phrases it can be read by, and none of its pictures saved.
    fn is_blind(&self, s: &StateDef) -> bool {
        let by_text = !s.detect.text.is_empty() && self.text_unreadable.is_none();
        !by_text && s.detect.screens.iter().all(|x| self.missing.contains(x))
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
        let reads_text = self.text_unreadable.is_none() && self.wf.states.iter().any(|s| !s.detect.text.is_empty());
        let lines = match reads_text {
            true => self
                .host
                .read_text(frame)
                .map_err(|e| WorkflowError::Text(format!("reading the screen's text failed: {e}")))?,
            false => Vec::new(),
        };
        for s in &self.wf.states {
            if reads_text && !s.detect.text.is_empty() {
                let (score, set) = s.detect.text_score(&lines).expect("a state with text has a set");
                refs.insert(s.name.clone(), format!("text: {}", set.join(" + ")));
                scores.push((s.name.clone(), score, s.detect.text_threshold));
                continue;
            }
            let mut best: Option<(f64, String)> = None;
            for screen in &s.detect.screens {
                if self.missing.contains(screen) {
                    continue;
                }
                let r = self.reference(screen)?;
                let sim = self.compare(frame, &r, s.detect.region);
                if best.as_ref().is_none_or(|(b, _)| sim > *b) {
                    best = Some((sim, screen.clone()));
                }
            }
            // A state none of whose pictures is saved cannot be recognised (the run checked at least one state can).
            let Some((sim, screen)) = best else { continue };
            refs.insert(s.name.clone(), screen);
            scores.push((s.name.clone(), sim, s.detect.threshold));
        }
        if scores.is_empty() {
            return Err(WorkflowError::Reference("no state's picture could be compared".into()));
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
        // Keys follow at once: the frame that settled the look must still be recent (classifying it, reading its text
        // included), or the screen may have moved on under them.
        self.fresh()?;
        if s.verify_tenant {
            self.verify_tenant(s)?;
        }
        Ok(())
    }

    /// Enter `to`, with what this visit already spent (`attempted`, `waiting`: a resume of the same visit carries them
    /// over). The whole state is recorded before it is announced, so no record of it ever says less was sent.
    fn enter(&mut self, to: &str, reason: &str, attempted: u32, waiting: bool) -> Result<(), WorkflowError> {
        self.ctx.visit_attempts = attempted;
        self.ctx.visit_waiting = waiting;
        let from = self.ctx.current.replace(to.to_string());
        if from.is_some() {
            self.ctx.previous = from.clone();
        }
        self.persist()?;
        self.emit(WorkflowEvent::StateEntered { state: to.to_string(), from, reason: reason.to_string() });
        Ok(())
    }

    /// A live run records where it is (the host's journal) before anything a crash must not lose. A failed record
    /// stops the run before what depends on it.
    fn persist(&mut self) -> Result<(), WorkflowError> {
        if self.opts.run.dry_run {
            return Ok(());
        }
        let Some(cp) = self.ctx.checkpoint(self.wf) else { return Ok(()) };
        self.host.persist(&cp).map_err(WorkflowError::Journal)
    }

    /// Before an attempt's keys: count it and record that it is about to be sent (a crash from here on counts the
    /// keys as sent, so a resume asks before sending them again). Nothing slow may come between the last look and the
    /// keys, so this goes first; the attempt is announced ([`Self::attempted`]) once the keys went.
    fn intend(&mut self, s: &StateDef, ops: &[(usize, Op)], attempt: u32) -> Result<(), WorkflowError> {
        *self.ctx.attempts.entry(s.name.clone()).or_insert(0) += 1;
        // Only keys sent count against a resume: waiting in a state that types nothing sends nothing to replay.
        if !ops.is_empty() {
            self.ctx.visit_attempts = attempt;
        }
        self.persist()
    }

    /// The trace's record of an attempt: after its keys were sent (or a burst was cut short, which counts as sent), so
    /// it never says keys went that a last look held back.
    fn attempted(&mut self, s: &StateDef, ops: &[(usize, Op)], attempt: u32) {
        self.emit(WorkflowEvent::ActionAttempted {
            state: s.name.clone(),
            attempt,
            steps: ops.len(),
            dry_run: self.opts.run.dry_run,
        });
    }

    /// Right before keys: the frame the last check rested on must still be within `evidence_age`. Whatever ran after
    /// that check (a slow trace sink, the journal, the reader) cannot let the screen move on unseen.
    fn fresh(&self) -> Result<(), WorkflowError> {
        match self.last_frame.as_ref().and_then(|f| f.captured_at).map(|t| t.elapsed()) {
            Some(age) if age > self.opts.evidence_age => Err(self.too_old(age)),
            _ => Ok(()),
        }
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
        if !self.tenant_by_picture() {
            return self.verify_account_shown(s);
        }
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

    /// Read the page that classified as `s`: it must show an account in the profile's domain and, in a live run, the
    /// very sign-in typed (a dry run typed none). Microsoft's password page shows the account above "Enter password".
    fn verify_account_shown(&mut self, s: &StateDef) -> Result<(), WorkflowError> {
        let domain = self.wf.config[TENANT_DOMAIN].to_lowercase();
        let not_shown = || WorkflowError::AccountNotShown { state: s.name.clone(), domain: domain.clone() };
        // The frame that just settled this state (`still_on` looks right before each burst of keys), its region read
        // enlarged.
        let frame = self.last_frame.clone().ok_or_else(not_shown)?;
        let lines = self.read_fresh(&frame, s.detect.region)?;
        // Whole addresses only, compared exactly (case aside): a near miss is another account, and so another
        // organisation's page.
        let accounts = crate::text::accounts_shown(&lines);
        let ok = match (self.opts.run.dry_run, self.account.0.first()) {
            (false, Some(account)) => accounts.iter().any(|a| a.eq_ignore_ascii_case(account)),
            // A dry run typed no sign-in: an account in the domain is all there is to see.
            _ => accounts.iter().any(|a| a.rsplit_once('@').is_some_and(|(_, d)| d.eq_ignore_ascii_case(&domain))),
        };
        if !ok {
            return Err(not_shown());
        }
        let confidence = 1.0;
        let tenant = self.wf.config.get("tenant").cloned().unwrap_or(domain);
        self.emit(WorkflowEvent::TenantVerified { state: s.name.clone(), tenant, confidence });
        Ok(())
    }

    fn send(&mut self, s: &StateDef, ops: &[(usize, Op)]) -> Result<(), WorkflowError> {
        if ops.is_empty() {
            return Ok(());
        }
        let mut host = Scrubbed { inner: &mut *self.host, secrets: &self.secrets };
        let opts = RunOptions { deadline: Some(Instant::now() + self.opts.action_deadline), ..self.opts.run.clone() };
        run(ops, &mut host, &opts).map_err(|e| match e {
            RunError::Cancelled => WorkflowError::Cancelled,
            error => WorkflowError::Action { state: s.name.clone(), error },
        })
    }

    /// Right after typing `key`'s value, and right before `submit`: a fresh, settled look must still be this state
    /// ([`Self::still_on`]: only frames captured after this moment, past the capture latency) and that very frame must
    /// show the value whole, as typed, in the state's region. A field drawn with its text cursor over a letter reads
    /// wrong, so looks repeat for up to [`TYPED_WAIT`] (each look's frame is fresh: [`Self::read_fresh`] bounds its
    /// age). A dry run typed nothing and checks nothing.
    fn verify_typed(&mut self, s: &StateDef, key: &str) -> Result<(), WorkflowError> {
        if self.opts.run.dry_run {
            return Ok(());
        }
        let value = self.typed.get(key).and_then(|v| v.0.first()).cloned().unwrap_or_default();
        let started = Instant::now();
        let mut swapped = false;
        loop {
            self.check()?;
            self.still_on(s)?;
            let frame = self.last_frame.clone().ok_or(WorkflowError::UnknownScreen { best: None })?;
            let lines = self.read_fresh(&frame, s.detect.region)?;
            // The frame is fresh (`read_fresh` refuses old evidence); the deadline bounds looking again.
            match crate::text::typed_shown(&lines, &value) {
                crate::text::Typed::Shown => {
                    self.emit(WorkflowEvent::TypedVerified { state: s.name.clone(), key: key.to_string() });
                    return Ok(());
                }
                crate::text::Typed::CaseSwapped => swapped = true,
                crate::text::Typed::Absent => {}
            }
            if started.elapsed() > TYPED_WAIT {
                return Err(WorkflowError::TypedNotShown { state: s.name.clone(), caps_lock: swapped });
            }
        }
    }

    /// Read `frame` (just settled, so fresh) enlarged, for small text: a sign-in in a field, an account above a
    /// password box. The answer must come back while the frame is still recent evidence
    /// ([`WorkflowOptions::evidence_age`]); a slow read is refused rather than trusted.
    fn read_fresh(&mut self, frame: &Frame, region: Option<[f64; 4]>) -> Result<Vec<crate::text::TextLine>, WorkflowError> {
        // A live check needs the frame's capture time: without it, its age cannot be told.
        let Some(captured) = frame.captured_at.or(if self.opts.run.dry_run { Some(Instant::now()) } else { None }) else {
            return Err(WorkflowError::Text(
                "this video source does not say when its pictures were taken, which checking the sign-in needs".into(),
            ));
        };
        // Only the state's region (no text elsewhere on screen can stand in for the field), enlarged while it stays
        // within what every recogniser reads (Windows OCR's limit was 2600 pixels on older builds).
        let part = match region {
            Some([x, y, w, h]) => frame.crop(x, y, w, h),
            None => frame.clone(),
        };
        let big = if part.width.max(part.height) * 2 <= MAX_READ_SIDE { part.doubled() } else { part };
        let lines = self.host.read_text(&big).map_err(|e| WorkflowError::Text(format!("reading the screen's text failed: {e}")))?;
        let age = captured.elapsed();
        if age > self.opts.evidence_age {
            return Err(self.too_old(age));
        }
        Ok(lines)
    }

    fn too_old(&self, age: Duration) -> WorkflowError {
        WorkflowError::Text(format!(
            "the screen's picture was {} ms old by the time it was read: the screen may have changed since; nothing more was sent",
            age.as_millis()
        ))
    }

    /// Record a verified transition out of `from`: history, its derived facts, and entering `to`.
    fn transition(
        &mut self,
        from: &StateDef,
        to: &str,
        confidence: f64,
        attempts: u32,
        entered_at: u64,
        reason: &str,
    ) -> Result<(), WorkflowError> {
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
        self.enter(to, reason, 0, false)
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
            None => self.enter(&first, "observed at start", 0, false)?,
            Some(cp) if cp.state == first => {
                // Unknown (an older checkpoint) counts as everything spent in that state: ask rather than replay.
                spent = cp.attempted.unwrap_or_else(|| cp.attempts.get(&first).copied().unwrap_or(0));
                // Still the same visit: what it spent, and a pending wait, carry over (until a verified transition),
                // and are recorded as such before anything else, so a crash from here on still asks.
                self.enter(&first, "re-observed after resume", spent, cp.waiting)?;
            }
            Some(cp) if cp.attempted.unwrap_or(0) == 0 && !cp.waiting => {
                // Nothing was sent in that visit (it stopped at a guard, a gate or a look), so the screen moving on is
                // not that state's transition: no facts are recorded for it. The new state's own checks decide.
                self.enter(&first, "observed after resume; the checkpoint's state sent nothing", 0, false)?;
            }
            Some(cp) => {
                // The screen moved on while the run was stopped: that is the checkpoint's pending transition, now
                // verified by a fresh look. Record it (and its facts) before the new state's guard.
                let from = self.wf.state(&cp.state).expect("checked").clone();
                let attempts = cp.attempted.unwrap_or(0);
                self.ctx.current = Some(cp.state.clone());
                let now = self.now();
                self.transition(&from, &first, confidence, attempts, now, "verified after resume")?;
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
                let sends = s.manual.is_none() && !ops.is_empty();
                if sends {
                    // Recorded as about to be sent first; then the last look, and the keys straight after it. If the
                    // look stops the run, nothing was sent: the count is taken back (a crash in between leaves the
                    // journal saying sent, the safe side).
                    let before = (self.ctx.attempts.get(&name).copied(), self.ctx.visit_attempts);
                    self.intend(&s, ops, attempt)?;
                    if let Err(e) = self.still_on(&s) {
                        match before.0 {
                            Some(n) => self.ctx.attempts.insert(name.clone(), n),
                            None => self.ctx.attempts.remove(&name),
                        };
                        self.ctx.visit_attempts = before.1;
                        return Err(e);
                    }
                }
                // Sending nothing, this visit now waits for the screen to move on (see `Checkpoint::waiting`).
                self.ctx.visit_waiting = !sends;
                match &s.manual {
                    // Nothing is sent, so nothing is spent: a resume never asks to "send again" a step done by hand.
                    // The wait is recorded before the operator is asked, so a crash while it is done resumes past it.
                    Some(message) => {
                        self.persist()?;
                        self.emit(WorkflowEvent::ManualStepRequested {
                            state: name.clone(),
                            message: message.clone(),
                            timeout_ms: s.timeout.as_millis() as u64,
                        })
                    }
                    None if !sends => {
                        self.intend(&s, ops, attempt)?;
                        self.attempted(&s, ops, attempt);
                    }
                    None => {
                        self.fresh()?;
                        let sent = self.send(&s, ops);
                        self.attempted(&s, ops, attempt);
                        sent?;
                        if let Some(key) = &s.verify_typed {
                            // Settled on this state and showing the value, on one fresh frame; then straight on.
                            self.verify_typed(&s, key)?;
                            self.fresh()?;
                            self.send(&s, &compiled[&submit_key(&name)])?;
                        }
                    }
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
            self.transition(&s, &to, confidence, attempt, entered_at, "transition verified")?;
        }
    }
}

/// The most a single key may be held, or a gap between keys may last. Typing is the action itself; these timings
/// only shape it, and a long one would be a pause the engine does not look through.
pub const MAX_KEY_TIMING: Duration = Duration::from_millis(100);

/// Whether any prompted secret's value appears in the workflow or the checkpoint, field by field (decoded values, not
/// the file's text, so an escape such as `s` cannot hide one). A caller that prints anything from either checks
/// this first; the error names only where, never the value.
pub fn check_secrets(wf: &Workflow, secrets: &Vars, resume: Option<&Checkpoint>, laptop: &str) -> Result<(), WorkflowError> {
    let values = Secrets(wf.secrets.keys().filter_map(|k| secrets.get(k).cloned()).collect());
    if let Some(place) = secret_place(wf, &values, resume) {
        return Err(WorkflowError::SecretInFile { place });
    }
    if values.appears_in(laptop) {
        return Err(WorkflowError::SecretInName);
    }
    Ok(())
}

/// Checks before a run that need no screen: timing options, the pause budget of every state, and that no prompted
/// secret's value appears anywhere in the workflow or the checkpoint (both are published: inspector, trace, exports).
fn precheck(wf: &Workflow, secrets: &Secrets, opts: &WorkflowOptions, resume: Option<&Checkpoint>) -> Result<(), WorkflowError> {
    // Privacy first: no other check may report anything from the file before this one has passed.
    if let Some(place) = secret_place(wf, secrets, resume) {
        return Err(WorkflowError::SecretInFile { place });
    }
    // The laptop's name is saved with the journal, the checkpoint and the run record, and names the record's files.
    if secrets.appears_in(&opts.laptop) {
        return Err(WorkflowError::SecretInName);
    }
    let unchosen = wf.unchosen();
    if !unchosen.is_empty() {
        return Err(WorkflowError::Options(format!(
            "set up this workflow first (Set up your copy, or `kvmit workflow new`): choose {}",
            unchosen.iter().map(|f| format!("\"{}\"", f.label)).collect::<Vec<_>>().join(" and ")
        )));
    }
    if opts.run.key_hold > MAX_KEY_TIMING || opts.run.key_gap > MAX_KEY_TIMING {
        return Err(WorkflowError::Options("key hold and key gap must be at most 100 ms in a workflow".into()));
    }
    if opts.evidence_age.is_zero() || opts.evidence_age > MAX_EVIDENCE_AGE {
        return Err(WorkflowError::Options("evidence age must be more than 0 and at most 30 s".into()));
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
    if hit(&[&wf.name, &wf.description]) || wf.account.as_deref().is_some_and(|a| secrets.appears_in(a)) {
        return Some("the workflow's name, description or account".into());
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
        steps(&mut vals, &st.submit);
        vals.extend(st.verify_typed.iter().map(String::as_str));
        vals.extend(st.detect.text.iter().flatten().map(String::as_str));
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
        missing: BTreeSet::new(),
        text_unreadable: None,
        account: Secrets(wf.account.as_ref().and_then(|a| secrets.get(a).cloned()).into_iter().collect()),
        typed: wf
            .states
            .iter()
            .filter_map(|s| s.verify_typed.clone())
            .map(|k| {
                let v = secrets.get(&k).or_else(|| wf.config.get(&k)).cloned();
                (k, Secrets(v.into_iter().collect()))
            })
            .collect(),
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
        // A value read back after typing is compared as one line: a line break or tab in it would be typed as Enter
        // or Tab before any check.
        if !opts.run.dry_run {
            if let Some(key) = e.typed.iter().find(|(_, v)| v.0.first().is_some_and(|v| v.chars().any(char::is_control))).map(|(k, _)| k.clone()) {
                return Err(WorkflowError::Options(format!(
                    "the value for {key} contains a line break, tab or other control character: enter it again"
                )));
            }
        }
        if let (Some(domain), false) = (wf.config.get(TENANT_DOMAIN), opts.run.dry_run) {
            let inside = e.account.0.first().is_some_and(|a| account_in(a, domain));
            if !inside {
                return Err(WorkflowError::Options(format!(
                    "the sign-in entered is not an account in {domain}, the profile's organisation: check it, or the profile"
                )));
            }
        }
        e.note_missing_pictures()?;
        for (i, st) in wf.states.iter().enumerate() {
            if action_time(&compiled[&st.name], &opts.run, true) + action_time(&compiled[&submit_key(&st.name)], &opts.run, true) > MAX_ACTION_TIME {
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
            WorkflowError::Reference(m)
            | WorkflowError::Text(m)
            | WorkflowError::Journal(m)
            | WorkflowError::Action { error: RunError::Host(m), .. } => {
                e.secrets.scrub(m)
            }
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
    md.push_str(
        "\nPrompted secrets are never recorded. Screens were recognised by their text or matched against reference \
         pictures (each observation names which); this is not a hardware verification record.\n",
    );
    md
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
