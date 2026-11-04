//! Script library: built-in examples plus plain files in the user's script folder.
use kvmit_script::{Script, Workflow};
use std::path::{Path, PathBuf};

pub struct Builtin {
    pub file: &'static str,
    pub source: &'static str,
}

pub const BUILTINS: &[Builtin] = &[
    Builtin { file: "demo-notepad.toml", source: include_str!("../examples/demo-notepad.toml") },
    Builtin { file: "windows-oobe-shift-f10.toml", source: include_str!("../examples/windows-oobe-shift-f10.toml") },
    Builtin { file: "type-secret-and-enter.toml", source: include_str!("../examples/type-secret-and-enter.toml") },
    Builtin {
        file: "windows-11-oobe-provisioning.toml",
        source: include_str!("../examples/windows-11-oobe-provisioning.toml"),
    },
];

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: Option<PathBuf>, // None = built-in
    pub source: String,
    pub error: Option<String>,
    /// A stateful workflow (`workflow_format = 1`) rather than a linear script.
    pub workflow: bool,
}

/// Whether a file is a workflow: its parsed top level has a `workflow_format` key.
pub fn is_workflow(source: &str) -> bool {
    kvmit_script::workflow::is_workflow_source(source)
}

pub fn load_all(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = BUILTINS
        .iter()
        .map(|b| {
            let workflow = is_workflow(b.source);
            let kind = if workflow { "[built-in workflow]" } else { "[built-in]" };
            Entry { name: format!("{kind} {}", b.file), path: None, source: b.source.to_string(), error: None, workflow }
        })
        .collect();
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut files: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        files.sort();
        for p in files {
            let source = std::fs::read_to_string(&p).unwrap_or_default();
            let workflow = is_workflow(&source);
            let error = if workflow { Workflow::parse(&source).err() } else { Script::parse(&source).err() };
            let mut name = p.file_name().unwrap().to_string_lossy().into_owned();
            if workflow {
                name = format!("[workflow] {name}");
            }
            out.push(Entry { name, path: Some(p), source, error, workflow });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflows_are_told_apart_by_their_top_level_key() {
        assert!(is_workflow("workflow_format = 1\nstart = []\n"));
        assert!(is_workflow("\"workflow_format\" = 1\n"), "a quoted key is the same key");
        assert!(!is_workflow("format = 1\n[vars]\nworkflow_format = { default = \"x\" }\n"), "nested, not top level");
        assert!(!is_workflow("format = 1\ndescription = \"\"\"\nworkflow_format = 1\n\"\"\"\n"), "inside a string");
        assert!(!is_workflow("not toml at all ["));
    }

    #[test]
    fn builtin_workflows_are_listed_as_workflows() {
        let all = load_all(Path::new("no-such-folder"));
        let oobe = all.iter().find(|e| e.name.ends_with("windows-11-oobe-provisioning.toml")).unwrap();
        assert!(oobe.workflow && oobe.name.starts_with("[built-in workflow]"));
        assert!(all.iter().filter(|e| e.name.ends_with("demo-notepad.toml")).all(|e| !e.workflow));
    }

    /// The provisioning template's safety rules: both paths are guarded by the approved profile, the password is typed
    /// only after the tenant check, secrets are only typed as secret text, and only screens a person must handle wait
    /// for one.
    #[test]
    fn the_provisioning_template_keeps_its_safety_rules() {
        let b = BUILTINS.iter().find(|b| b.file == "windows-11-oobe-provisioning.toml").unwrap();
        let w = Workflow::parse(b.source).unwrap();
        let st = |n: &str| w.state(n).unwrap_or_else(|| panic!("{n}"));
        assert_eq!(st("AUTOPILOT_SIGNIN").require.get("deployment_mode").map(String::as_str), Some("autopilot"));
        assert_eq!(st("SETUP_CHOICE").require.get("deployment_mode").map(String::as_str), Some("work_account"));
        assert_eq!(st("WORK_SIGNIN").require.get("deployment_mode").map(String::as_str), Some("work_account"));
        assert!(st("PASSWORD").verify_tenant, "the tenant is checked before the password is typed");
        assert_eq!(st("EULA").require.get("accept_license").map(String::as_str), Some("yes"));
        for s in &w.states {
            let typed_secret = s.actions.iter().any(|a| matches!(a, kvmit_script::Step::SecretText(_)));
            let plain_text = s.actions.iter().any(|a| matches!(a, kvmit_script::Step::Text(_)));
            assert!(!plain_text, "{}: nothing is typed as plain text", s.name);
            if typed_secret {
                assert!(s.require.contains_key("deployment_mode") || s.verify_tenant, "{}: secrets only on guarded screens", s.name);
            }
        }
        let manual: Vec<&str> = w.states.iter().filter(|s| s.manual.is_some()).map(|s| s.name.as_str()).collect();
        assert_eq!(manual, ["NETWORK", "DEVICE_NAME", "MFA", "PRIVACY"]);
        assert!(st("HANDOFF").terminal);
    }

    /// A simulated target for the provisioning template: each screen is a distinct blocky pattern (so structure
    /// matching has something to tell apart), and it moves on when Enter is pressed or, for screens a person or the
    /// target itself finishes, after some looks.
    mod sim {
        use kvmit_script::{Frame, Host};
        use std::time::Duration;

        pub enum Next {
            Enter,
            Looks(usize),
        }

        pub fn pattern(name: &str) -> Frame {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in name.bytes() {
                h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
            }
            let (w, ht) = (160, 90);
            let mut px = vec![0u8; w * ht];
            for by in 0..ht / 10 {
                for bx in 0..w / 10 {
                    h = h.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    let v = (h >> 56) as u8;
                    for y in by * 10..by * 10 + 10 {
                        for x in bx * 10..bx * 10 + 10 {
                            px[y * w + x] = v;
                        }
                    }
                }
            }
            Frame::new(w, ht, px).unwrap()
        }

        pub struct Target {
            pub screens: Vec<(&'static str, Next)>,
            pub pos: usize,
            looks_here: usize,
            seq: u64,
            /// The screen each typed character (not Enter, not a modifier) landed on.
            pub typed: Vec<&'static str>,
        }

        impl Target {
            pub fn new(screens: Vec<(&'static str, Next)>) -> Target {
                Target { screens, pos: 0, looks_here: 0, seq: 0, typed: Vec::new() }
            }
            fn here(&self) -> &'static str {
                self.screens[self.pos].0
            }
            pub fn typed_on(&self, screen: &str) -> usize {
                self.typed.iter().filter(|s| **s == screen).count()
            }
        }

        impl Host for Target {
            fn key_down(&mut self, _: kvmit_hid::Key) -> Result<(), String> {
                Ok(())
            }
            fn key_up(&mut self, k: kvmit_hid::Key) -> Result<(), String> {
                if k.0 == 0x28 {
                    if matches!(self.screens[self.pos].1, Next::Enter) && self.pos + 1 < self.screens.len() {
                        self.pos += 1;
                        self.looks_here = 0;
                    }
                } else if !(0xe0..=0xe7).contains(&k.0) && k.0 != 0x51 {
                    let here = self.here();
                    self.typed.push(here);
                }
                Ok(())
            }
            fn release_all(&mut self) -> Result<(), String> {
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
                self.looks_here += 1;
                if let Next::Looks(n) = self.screens[self.pos].1 {
                    if self.looks_here > n && self.pos + 1 < self.screens.len() {
                        self.pos += 1;
                        self.looks_here = 0;
                    }
                }
                self.seq += 1;
                Some(pattern(self.here()).with_seq(Some(self.seq)).with_captured_at(Some(std::time::Instant::now())))
            }
            fn reference(&mut self, name: &str) -> Result<Frame, String> {
                // The password page is the organisation's branded page: its reference is the tenant's picture.
                let name = name.strip_prefix("oobe-win11/").unwrap_or(name).trim_end_matches(".png");
                Ok(pattern(if name == "password" { "tenant-contoso" } else { name }))
            }
            fn confirm(&mut self, _: &str) -> bool {
                false
            }
            fn cancelled(&self) -> bool {
                false
            }
            fn event(&mut self, _: kvmit_script::RunEvent) {}
        }
    }

    fn provisioning(mode: &str) -> Workflow {
        let b = BUILTINS.iter().find(|b| b.file == "windows-11-oobe-provisioning.toml").unwrap();
        // Only the profile's line: the guards say `deployment_mode = "..."` too.
        let src = b.source.replacen("\ndeployment_mode = \"autopilot\"", &format!("\ndeployment_mode = \"{mode}\""), 1);
        Workflow::parse(&src).unwrap()
    }

    fn provision(wf: &Workflow, target: &mut sim::Target) -> kvmit_script::WorkflowReport {
        let secrets: kvmit_script::Vars =
            [("username".to_string(), "ada@contoso.example".to_string()), ("password".to_string(), "pw-123".to_string())].into();
        let mut opts = kvmit_script::WorkflowOptions::default();
        opts.run.key_hold = std::time::Duration::ZERO;
        opts.run.key_gap = std::time::Duration::ZERO;
        kvmit_script::run_workflow(wf, &secrets, &kvmit_layout::UsAnsi, target, &opts, None, &mut |_, _| {})
    }

    fn path(r: &kvmit_script::WorkflowReport) -> Vec<&str> {
        r.context.history.iter().map(|h| h.to.as_str()).collect()
    }

    #[test]
    fn the_provisioning_template_runs_both_paths_on_a_simulated_target() {
        use sim::Next::{Enter, Looks};
        // Autopilot: the organisation's sign-in follows the updates; the user approves MFA; enrolment runs; handoff.
        let mut t = sim::Target::new(vec![
            ("region", Enter),
            ("keyboard", Enter),
            ("second-keyboard", Enter),
            ("network", Looks(6)),
            ("updates", Looks(8)),
            ("autopilot-signin", Enter),
            ("tenant-contoso", Enter),
            ("mfa", Looks(6)),
            ("esp", Looks(10)),
            ("hello", Looks(1000)),
        ]);
        let r = provision(&provisioning("autopilot"), &mut t);
        assert_eq!(r.result, Ok(()), "{:#?}", r.trace);
        assert_eq!(path(&r), ["KEYBOARD", "SECOND_KEYBOARD", "NETWORK", "UPDATES", "AUTOPILOT_SIGNIN", "PASSWORD", "MFA", "ESP", "HANDOFF"]);
        assert_eq!(t.typed_on("autopilot-signin"), "ada@contoso.example".len());
        assert_eq!(t.typed_on("tenant-contoso"), "pw-123".len(), "the password only on the verified password page");
        assert_eq!(t.typed.len(), "ada@contoso.example".len() + "pw-123".len(), "nothing typed anywhere else");
        assert_eq!(r.context.derived["enrolment"].0, "Windows Autopilot");

        // Work or school account: licence, device name by hand, "work or school", sign-in, privacy by hand.
        let mut t = sim::Target::new(vec![
            ("region", Enter),
            ("keyboard", Enter),
            ("second-keyboard", Enter),
            ("updates", Looks(8)),
            ("license", Enter),
            ("device-name", Looks(6)),
            ("setup-choice", Enter),
            ("work-signin", Enter),
            ("tenant-contoso", Enter),
            ("privacy", Looks(6)),
            ("desktop", Looks(1000)),
        ]);
        let r = provision(&provisioning("work_account"), &mut t);
        assert_eq!(r.result, Ok(()), "{:#?}", r.trace);
        assert_eq!(
            path(&r),
            ["KEYBOARD", "SECOND_KEYBOARD", "UPDATES", "EULA", "DEVICE_NAME", "SETUP_CHOICE", "WORK_SIGNIN", "PASSWORD", "PRIVACY", "HANDOFF"]
        );
        assert_eq!(r.context.derived["enrolment"].0, "Microsoft Entra join");
        assert_eq!(r.context.derived["license"].0, "accepted");
    }

    #[test]
    fn the_provisioning_template_stops_on_the_other_path_or_another_tenant() {
        use sim::Next::{Enter, Looks};
        // The profile says work account, but the device is registered for Autopilot: stop before typing there.
        let mut t = sim::Target::new(vec![
            ("region", Enter),
            ("keyboard", Enter),
            ("second-keyboard", Enter),
            ("autopilot-signin", Enter),
            ("tenant-contoso", Enter),
        ]);
        let r = provision(&provisioning("work_account"), &mut t);
        assert!(
            matches!(&r.result, Err(kvmit_script::WorkflowError::Guard { state, .. }) if state == "AUTOPILOT_SIGNIN"),
            "{:?}",
            r.result
        );
        assert!(t.typed.is_empty());

        // Another organisation's sign-in page: the password is never typed.
        let mut t = sim::Target::new(vec![("autopilot-signin", Enter), ("tenant-fabrikam", Enter), ("esp", Looks(1000))]);
        let r = provision(&provisioning("autopilot"), &mut t);
        assert!(r.result.is_err(), "{:?}", r.result);
        assert_eq!(t.typed_on("tenant-fabrikam"), 0, "no password typed into another organisation's page");
    }

    #[test]
    fn every_builtin_parses_and_compiles() {
        for b in BUILTINS {
            if is_workflow(b.source) {
                let w = Workflow::parse(b.source).unwrap_or_else(|e| panic!("{}: {e}", b.file));
                let secrets: kvmit_script::Vars = w.secrets.keys().map(|k| (k.clone(), "x".to_string())).collect();
                kvmit_script::workflow::compile_workflow(&w, &secrets, &kvmit_layout::UsAnsi)
                    .unwrap_or_else(|e| panic!("{}: {e}", b.file));
                continue;
            }
            let s = Script::parse(b.source).unwrap_or_else(|e| panic!("{}: {e}", b.file));
            let mut vars = kvmit_script::Vars::new();
            for (k, d) in &s.vars {
                vars.insert(k.clone(), d.default.clone().unwrap_or_else(|| "x".into()));
            }
            kvmit_script::compile(&s, &vars, &kvmit_layout::UsAnsi).unwrap_or_else(|e| panic!("{}: {e}", b.file));
        }
    }
}
