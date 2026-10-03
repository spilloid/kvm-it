//! The native script file (TOML). See docs/ux.md for the format and rationale.
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::Deref;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Script {
    pub name: String,
    pub description: String,
    pub vars: BTreeMap<String, VarDef>,
    pub steps: Vec<Step>,
}

/// Template text for `secret_text`. Its Debug output is redacted so a stray `{:?}` can never print a credential.
#[derive(Clone, PartialEq, Default)]
pub struct SecretTemplate(pub String);
impl fmt::Debug for SecretTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretTemplate(<{} bytes, redacted>)", self.0.len())
    }
}
impl Deref for SecretTemplate {
    type Target = String;
    fn deref(&self) -> &String {
        &self.0
    }
}
impl From<String> for SecretTemplate {
    fn from(s: String) -> Self {
        SecretTemplate(s)
    }
}
impl From<&str> for SecretTemplate {
    fn from(s: &str) -> Self {
        SecretTemplate(s.to_string())
    }
}

#[derive(Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VarDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub secret: bool,
}
fn is_false(b: &bool) -> bool {
    !*b
}
impl fmt::Debug for VarDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VarDef")
            .field("prompt", &self.prompt)
            .field("default", &self.default.as_ref().map(|d| if self.secret { "<redacted>".to_string() } else { d.clone() }))
            .field("secret", &self.secret)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum WaitSpec {
    /// Wait until the screen resembles a reference image (file relative to the script).
    Screen { image: String, timeout: Duration, threshold: f64, on_timeout_continue: bool },
    /// Wait until the screen has not changed for `stable_for`.
    Stable { stable_for: Duration, timeout: Duration, on_timeout_continue: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Text(String),
    SecretText(SecretTemplate),
    Key(String),
    Chord(Vec<String>),
    Delay(Duration),
    Repeat { count: u32, steps: Vec<Step> },
    Wait(WaitSpec),
    Confirm(String),
    Click(String),
    MoveMouse { dx: i32, dy: i32 },
    Comment(String),
}

pub const FORMAT: u32 = 1;
const MAX_REPEAT: u32 = 10_000;

/// "250ms", "1s", "1.5s", "2m". Bare numbers are rejected: units are explicit.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else {
        return Err(format!("duration {s:?} needs a unit (ms, s or m)"));
    };
    let v: f64 = num.trim().parse().map_err(|_| format!("bad duration {s:?}"))?;
    if !(0.0..=86_400.0).contains(&(v * mult)) {
        return Err(format!("duration {s:?} out of range"));
    }
    Ok(Duration::from_secs_f64(v * mult))
}

fn fmt_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms.is_multiple_of(1000) { format!("{}s", ms / 1000) } else { format!("{ms}ms") }
}

// ---- serde mirror of the file layout ----
#[derive(Serialize, Deserialize, Default)]
struct RawWait {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    screen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stable_for: Option<String>,
    timeout: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    threshold: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on_timeout: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct RawRepeat {
    count: u32,
    steps: Vec<RawStep>,
}

#[derive(Serialize, Deserialize, Default)]
struct RawMove {
    dx: i32,
    dy: i32,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawStep {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chord: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repeat: Option<RawRepeat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wait_for: Option<RawWait>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    confirm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    click: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "move")]
    move_mouse: Option<RawMove>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    comment: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScript {
    #[serde(default)]
    name: String,
    format: u32,
    #[serde(default)]
    description: String,
    #[serde(default)]
    vars: BTreeMap<String, VarDef>,
    #[serde(default)]
    steps: Vec<RawStep>,
}

fn step_from_raw(r: RawStep) -> Result<Step, String> {
    let n = [
        r.text.is_some(), r.secret_text.is_some(), r.key.is_some(), r.chord.is_some(), r.delay.is_some(),
        r.repeat.is_some(), r.wait_for.is_some(), r.confirm.is_some(), r.click.is_some(),
        r.move_mouse.is_some(), r.comment.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    if n != 1 {
        return Err(format!("each step must have exactly one action, found {n}"));
    }
    Ok(if let Some(t) = r.text {
        Step::Text(t)
    } else if let Some(t) = r.secret_text {
        // A literal secret in a file would sit on disk in plaintext; secrets must come from a prompted variable.
        if !t.contains("{{") {
            return Err("secret_text must reference a secret variable ({{name}}); a literal secret in a script file would be stored in plaintext".into());
        }
        Step::SecretText(t.into())
    } else if let Some(k) = r.key {
        Step::Key(k)
    } else if let Some(c) = r.chord {
        if c.is_empty() {
            return Err("chord needs at least one key".into());
        }
        Step::Chord(c)
    } else if let Some(d) = r.delay {
        Step::Delay(parse_duration(&d)?)
    } else if let Some(rep) = r.repeat {
        if rep.count == 0 || rep.count > MAX_REPEAT {
            return Err(format!("repeat count must be 1..={MAX_REPEAT}"));
        }
        Step::Repeat { count: rep.count, steps: rep.steps.into_iter().map(step_from_raw).collect::<Result<_, _>>()? }
    } else if let Some(w) = r.wait_for {
        let timeout = parse_duration(&w.timeout)?;
        let cont = match w.on_timeout.as_deref() {
            None | Some("fail") => false,
            Some("continue") => true,
            Some(o) => return Err(format!("on_timeout must be \"fail\" or \"continue\", got {o:?}")),
        };
        match (w.screen, w.stable_for) {
            (Some(image), None) => {
                let threshold = w.threshold.unwrap_or(0.9);
                if !(0.0..=1.0).contains(&threshold) {
                    return Err("threshold must be within 0..=1".into());
                }
                Step::Wait(WaitSpec::Screen { image, timeout, threshold, on_timeout_continue: cont })
            }
            (None, Some(s)) => {
                Step::Wait(WaitSpec::Stable { stable_for: parse_duration(&s)?, timeout, on_timeout_continue: cont })
            }
            _ => return Err("wait_for needs exactly one of `screen` or `stable_for`".into()),
        }
    } else if let Some(c) = r.confirm {
        Step::Confirm(c)
    } else if let Some(b) = r.click {
        Step::Click(b)
    } else if let Some(m) = r.move_mouse {
        Step::MoveMouse { dx: m.dx, dy: m.dy }
    } else {
        Step::Comment(r.comment.unwrap_or_default())
    })
}

fn step_to_raw(s: &Step) -> RawStep {
    let mut r = RawStep::default();
    match s {
        Step::Text(t) => r.text = Some(t.clone()),
        Step::SecretText(t) => r.secret_text = Some(t.0.clone()),
        Step::Key(k) => r.key = Some(k.clone()),
        Step::Chord(c) => r.chord = Some(c.clone()),
        Step::Delay(d) => r.delay = Some(fmt_duration(*d)),
        Step::Repeat { count, steps } => {
            r.repeat = Some(RawRepeat { count: *count, steps: steps.iter().map(step_to_raw).collect() })
        }
        Step::Wait(WaitSpec::Screen { image, timeout, threshold, on_timeout_continue }) => {
            r.wait_for = Some(RawWait {
                screen: Some(image.clone()),
                timeout: fmt_duration(*timeout),
                threshold: Some(*threshold),
                on_timeout: on_timeout_continue.then(|| "continue".to_string()),
                ..Default::default()
            })
        }
        Step::Wait(WaitSpec::Stable { stable_for, timeout, on_timeout_continue }) => {
            r.wait_for = Some(RawWait {
                stable_for: Some(fmt_duration(*stable_for)),
                timeout: fmt_duration(*timeout),
                on_timeout: on_timeout_continue.then(|| "continue".to_string()),
                ..Default::default()
            })
        }
        Step::Confirm(c) => r.confirm = Some(c.clone()),
        Step::Click(b) => r.click = Some(b.clone()),
        Step::MoveMouse { dx, dy } => r.move_mouse = Some(RawMove { dx: *dx, dy: *dy }),
        Step::Comment(c) => r.comment = Some(c.clone()),
    }
    r
}

impl Script {
    pub fn parse(toml_text: &str) -> Result<Script, String> {
        let raw: RawScript = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        if raw.format != FORMAT {
            return Err(format!("unsupported script format {} (this build reads format {FORMAT})", raw.format));
        }
        for (name, def) in &raw.vars {
            if def.secret && def.default.is_some() {
                return Err(format!("secret variable {name:?} must not have a default (it would be stored in plaintext)"));
            }
        }
        Ok(Script {
            name: raw.name,
            description: raw.description,
            vars: raw.vars,
            steps: raw.steps.into_iter().map(step_from_raw).collect::<Result<_, _>>()?,
        })
    }

    pub fn to_toml(&self) -> String {
        let raw = RawScript {
            name: self.name.clone(),
            format: FORMAT,
            description: self.description.clone(),
            vars: self.vars.clone(),
            steps: self.steps.iter().map(step_to_raw).collect(),
        };
        toml::to_string(&raw).expect("script serialises")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("250ms").unwrap(), Duration::from_millis(250));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("-1s").is_err());
        assert!(parse_duration("abc s").is_err());
    }

    const SAMPLE: &str = r#"
name = "Windows 11 OOBE"
format = 1

[vars]
wifi_pass = { prompt = "Wi-Fi password", secret = true }
user = { default = "tech" }

[[steps]]
wait_for = { screen = "oobe-region.png", timeout = "90s", threshold = 0.9 }
[[steps]]
key = "ENTER"
[[steps]]
text = "{{user}}"
[[steps]]
secret_text = "{{wifi_pass}}"
[[steps]]
chord = ["CTRL", "SHIFT", "F10"]
[[steps]]
delay = "1s"
[[steps]]
repeat = { count = 3, steps = [ { key = "TAB" } ] }
[[steps]]
confirm = "Is the desktop showing?"
"#;

    #[test]
    fn parses_the_documented_example_and_round_trips() {
        let s = Script::parse(SAMPLE).unwrap();
        assert_eq!(s.steps.len(), 8);
        assert!(s.vars["wifi_pass"].secret);
        assert_eq!(s.vars["user"].default.as_deref(), Some("tech"));
        let again = Script::parse(&s.to_toml()).unwrap();
        assert_eq!(s, again);
    }

    #[test]
    fn secrets_are_never_stored_or_printed() {
        assert!(Script::parse("format = 1\n[vars]\npw = { secret = true, default = \"hunter2\" }").is_err());
        assert!(Script::parse("format = 1\n[[steps]]\nsecret_text = \"hunter2\"").is_err());
        let ok = Script::parse("format = 1\n[vars]\npw = { secret = true }\n[[steps]]\nsecret_text = \"{{pw}}\"").unwrap();
        assert!(!format!("{ok:?}").contains("{{pw}}"));
        let leaky = VarDef { secret: true, default: Some("hunter2".into()), ..Default::default() };
        assert!(!format!("{leaky:?}").contains("hunter2"));
    }

    #[test]
    fn rejects_malformed_scripts() {
        for bad in [
            "format = 2",
            "format = 1\n[[steps]]\nkey = \"A\"\ntext = \"b\"",
            "format = 1\n[[steps]]",
            "format = 1\n[[steps]]\nbogus = 1",
            "format = 1\n[[steps]]\ndelay = \"5\"",
            "format = 1\n[[steps]]\nrepeat = { count = 0, steps = [] }",
            "format = 1\n[[steps]]\nwait_for = { timeout = \"1s\" }",
            "format = 1\n[[steps]]\nchord = []",
        ] {
            assert!(Script::parse(bad).is_err(), "should reject: {bad}");
        }
    }
}
