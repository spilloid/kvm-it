//! Script library: built-in examples plus plain files in the user's script folder.
use kvmit_script::Script;
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
}

pub fn load_all(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = BUILTINS
        .iter()
        .map(|b| Entry { name: format!("[built-in] {}", b.file), path: None, source: b.source.to_string(), error: None })
        .collect();
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut files: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "toml")).collect();
        files.sort();
        for p in files {
            let source = std::fs::read_to_string(&p).unwrap_or_default();
            let error = Script::parse(&source).err();
            out.push(Entry { name: p.file_name().unwrap().to_string_lossy().into_owned(), path: Some(p), source, error });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
