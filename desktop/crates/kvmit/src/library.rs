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
        .map(|b| Entry {
            name: format!("[built-in] {}", b.file),
            path: None,
            source: b.source.to_string(),
            error: None,
            workflow: false,
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
    fn every_builtin_parses_and_compiles() {
        for b in BUILTINS {
            let s = Script::parse(b.source).unwrap_or_else(|e| panic!("{}: {e}", b.file));
            let mut vars = kvmit_script::Vars::new();
            for (k, d) in &s.vars {
                vars.insert(k.clone(), d.default.clone().unwrap_or_else(|| "x".into()));
            }
            kvmit_script::compile(&s, &vars, &kvmit_layout::UsAnsi).unwrap_or_else(|e| panic!("{}: {e}", b.file));
        }
    }
}
