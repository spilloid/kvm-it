//! DuckyScript → native script, common subset. Anything not understood is *reported*, never guessed.
use crate::model::{Script, Step};
use kvmit_hid::parse_key;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ImportReport {
    /// (1-based line number, original line) for every line that was not translated.
    pub unsupported: Vec<(usize, String)>,
    pub translated: usize,
}

fn is_key_token(t: &str) -> bool {
    parse_key(t).is_some()
}

pub fn import(source: &str, name: &str) -> (Script, ImportReport) {
    let mut steps: Vec<Step> = Vec::new();
    let mut report = ImportReport::default();
    let mut default_delay: Option<Duration> = None;
    let mut last_step: Option<Step> = None;

    for (n, raw) in source.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let (cmd, arg) = match trimmed.split_once(char::is_whitespace) {
            Some((c, a)) => (c, a),
            None => (trimmed, ""),
        };
        let upper = cmd.to_ascii_uppercase();
        let mut new_steps: Vec<Step> = Vec::new();
        let ok = match upper.as_str() {
            "REM" => {
                new_steps.push(Step::Comment(arg.to_string()));
                true
            }
            "STRING" => {
                new_steps.push(Step::Text(arg.to_string()));
                true
            }
            "STRINGLN" => {
                new_steps.push(Step::Text(arg.to_string()));
                new_steps.push(Step::Key("ENTER".into()));
                true
            }
            "DELAY" => match arg.trim().parse::<u64>() {
                Ok(ms) => {
                    new_steps.push(Step::Delay(Duration::from_millis(ms)));
                    true
                }
                Err(_) => false,
            },
            "DEFAULT_DELAY" | "DEFAULTDELAY" => match arg.trim().parse::<u64>() {
                Ok(ms) => {
                    default_delay = Some(Duration::from_millis(ms));
                    true
                }
                Err(_) => false,
            },
            "REPEAT" => match (arg.trim().parse::<u32>(), &last_step) {
                (Ok(c), Some(prev)) if (1..=10_000).contains(&c) => {
                    new_steps.push(Step::Repeat { count: c, steps: vec![prev.clone()] });
                    true
                }
                _ => false,
            },
            _ => {
                // key or combo: "ENTER", "GUI r", "CTRL-ALT-DELETE", "ALT F4"
                let tokens: Vec<&str> = trimmed.split(|c: char| c.is_whitespace() || c == '-').filter(|t| !t.is_empty()).collect();
                if !tokens.is_empty() && tokens.iter().all(|t| is_key_token(t)) {
                    if tokens.len() == 1 {
                        new_steps.push(Step::Key(tokens[0].to_string()));
                    } else {
                        new_steps.push(Step::Chord(tokens.iter().map(|t| t.to_string()).collect()));
                    }
                    true
                } else {
                    false
                }
            }
        };
        if ok {
            report.translated += 1;
            for s in new_steps {
                if !matches!(s, Step::Comment(_) | Step::Repeat { .. }) {
                    last_step = Some(s.clone());
                }
                steps.push(s);
            }
        } else {
            report.unsupported.push((n + 1, trimmed.to_string()));
        }
    }

    // DEFAULT_DELAY applies after every action step that follows it; apply it uniformly after typed steps.
    if let Some(d) = default_delay {
        let mut with_delay = Vec::new();
        for s in steps {
            let action = !matches!(s, Step::Comment(_) | Step::Delay(_));
            with_delay.push(s);
            if action {
                with_delay.push(Step::Delay(d));
            }
        }
        steps = with_delay;
    }
    (Script { name: name.to_string(), description: "Imported from DuckyScript".into(), vars: Default::default(), steps }, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_common_subset() {
        let src = "REM open run\nGUI r\nDELAY 500\nSTRING notepad\nENTER\nCTRL-ALT-DELETE\nSTRINGLN hello\nREPEAT 2\n";
        let (s, r) = import(src, "t");
        assert!(r.unsupported.is_empty(), "{:?}", r.unsupported);
        assert_eq!(s.steps[0], Step::Comment("open run".into()));
        assert_eq!(s.steps[1], Step::Chord(vec!["GUI".into(), "r".into()]));
        assert_eq!(s.steps[2], Step::Delay(Duration::from_millis(500)));
        assert_eq!(s.steps[3], Step::Text("notepad".into()));
        assert_eq!(s.steps[4], Step::Key("ENTER".into()));
        assert_eq!(s.steps[5], Step::Chord(vec!["CTRL".into(), "ALT".into(), "DELETE".into()]));
        assert_eq!(s.steps[6], Step::Text("hello".into()));
        assert_eq!(s.steps[7], Step::Key("ENTER".into()));
        assert_eq!(s.steps[8], Step::Repeat { count: 2, steps: vec![Step::Key("ENTER".into())] });
        // output is a valid native script
        assert_eq!(Script::parse(&s.to_toml()).unwrap(), s);
    }

    #[test]
    fn reports_what_it_cannot_translate() {
        let src = "REPEAT 3\nSTRING ok\nWAIT_FOR_BUTTON_PRESS\nIF (x) THEN\nDELAY soon\n";
        let (s, r) = import(src, "t");
        assert_eq!(s.steps.len(), 1);
        assert_eq!(r.unsupported.iter().map(|(n, _)| *n).collect::<Vec<_>>(), vec![1, 3, 4, 5]);
    }

    #[test]
    fn default_delay_follows_actions() {
        let (s, _) = import("DEFAULT_DELAY 100\nSTRING a\nENTER\n", "t");
        assert_eq!(s.steps, vec![
            Step::Text("a".into()), Step::Delay(Duration::from_millis(100)),
            Step::Key("ENTER".into()), Step::Delay(Duration::from_millis(100)),
        ]);
    }
}
