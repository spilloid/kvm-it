//! Dev harness, not part of the app: run a workflow against a QEMU/Proxmox VM instead of a capture card and adapter.
//!
//! Keys go in with `qm sendkey` and the screen comes back with the monitor's `screendump`, both over `ssh` to the
//! Proxmox host. Each screendump is a new captured frame and gets a new frame number. A run through this harness is
//! **VM-verified**, never hardware-verified: QEMU's virtual keyboard is not the adapter's USB HID, and a screendump is
//! not an HDMI capture.
//!
//! ```text
//! cargo run -p kvmit-video --example qemu_workflow -- <ssh-target> <vmid> <workflow.toml> [--dry-run] [--snap <dir>]
//! ```
//! `--snap <dir>` only saves the current screen as `<dir>/screen.png` (for making reference images) and exits.

use kvmit_hid::Key;
use kvmit_script::workflow::{run_workflow, trace_jsonl, trace_markdown, Workflow, WorkflowOptions};
use kvmit_script::{Frame, Host, RunEvent, RunOptions, Vars};
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

struct Qemu {
    ssh: String,
    vmid: String,
    base: PathBuf,
    held: BTreeSet<u8>,
    seq: u64,
}

/// QEMU key name for a HID usage code (US layout), or `None` for keys this harness does not map.
fn qemu_key(code: u8) -> Option<String> {
    let name = match code {
        0x04..=0x1d => ((b'a' + (code - 0x04)) as char).to_string(),
        0x1e..=0x26 => ((b'1' + (code - 0x1e)) as char).to_string(),
        0x27 => "0".into(),
        0x28 => "ret".into(),
        0x29 => "esc".into(),
        0x2a => "backspace".into(),
        0x2b => "tab".into(),
        0x2c => "spc".into(),
        0x2d => "minus".into(),
        0x2e => "equal".into(),
        0x2f => "bracket_left".into(),
        0x30 => "bracket_right".into(),
        0x31 => "backslash".into(),
        0x33 => "semicolon".into(),
        0x34 => "apostrophe".into(),
        0x35 => "grave_accent".into(),
        0x36 => "comma".into(),
        0x37 => "dot".into(),
        0x38 => "slash".into(),
        0x3a..=0x45 => format!("f{}", code - 0x39),
        0x4a => "home".into(),
        0x4b => "pgup".into(),
        0x4c => "delete".into(),
        0x4d => "end".into(),
        0x4e => "pgdn".into(),
        0x4f => "right".into(),
        0x50 => "left".into(),
        0x51 => "down".into(),
        0x52 => "up".into(),
        _ => return None,
    };
    Some(name)
}

fn modifier(code: u8) -> Option<&'static str> {
    match code {
        0xe0 | 0xe4 => Some("ctrl"),
        0xe1 | 0xe5 => Some("shift"),
        0xe2 | 0xe6 => Some("alt"),
        0xe3 | 0xe7 => Some("meta_l"),
        _ => None,
    }
}

impl Qemu {
    fn ssh(&self, cmd: &str) -> Result<Vec<u8>, String> {
        let out = Command::new("ssh")
            .args(["-o", "BatchMode=yes", &self.ssh, cmd])
            .output()
            .map_err(|e| format!("ssh: {e}"))?;
        if !out.status.success() {
            return Err(format!("ssh exited with {}", out.status));
        }
        Ok(out.stdout)
    }

    fn grab(&mut self) -> Option<Frame> {
        let cmd = format!(
            "echo 'screendump /tmp/kvmit-wf-{0}.ppm' | qm monitor {0} >/dev/null && cat /tmp/kvmit-wf-{0}.ppm",
            self.vmid
        );
        let ppm = self.ssh(&cmd).ok()?;
        let (w, h, rgb) = parse_ppm(&ppm)?;
        let gray = rgb.as_chunks::<3>().0.iter().map(|p| ((p[0] as u32 * 77 + p[1] as u32 * 150 + p[2] as u32 * 29) >> 8) as u8).collect();
        self.seq += 1;
        Frame::new(w, h, gray).map(|f| f.with_seq(Some(self.seq)))
    }
}

fn parse_ppm(data: &[u8]) -> Option<(usize, usize, &[u8])> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while fields.len() < 4 {
        while data.get(pos)?.is_ascii_whitespace() {
            pos += 1;
        }
        let start = pos;
        while !data.get(pos)?.is_ascii_whitespace() {
            pos += 1;
        }
        fields.push(std::str::from_utf8(&data[start..pos]).ok()?.to_string());
    }
    pos += 1;
    if fields[0] != "P6" || fields[3] != "255" {
        return None;
    }
    let (w, h): (usize, usize) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let rgb = data.get(pos..pos + w * h * 3)?;
    Some((w, h, rgb))
}

impl Host for Qemu {
    fn key_down(&mut self, key: Key) -> Result<(), String> {
        if modifier(key.0).is_some() {
            self.held.insert(key.0);
        }
        Ok(())
    }
    fn key_up(&mut self, key: Key) -> Result<(), String> {
        if modifier(key.0).is_some() {
            self.held.remove(&key.0);
            return Ok(());
        }
        let name = qemu_key(key.0).ok_or_else(|| format!("key 0x{:02x} is not mapped for QEMU", key.0))?;
        let combo: Vec<&str> = self.held.iter().filter_map(|m| modifier(*m)).chain(std::iter::once(name.as_str())).collect();
        self.ssh(&format!("qm sendkey {} {}", self.vmid, combo.join("-"))).map(|_| ())
    }
    fn release_all(&mut self) -> Result<(), String> {
        self.held.clear();
        Ok(())
    }
    fn mouse_move(&mut self, _: i32, _: i32) -> Result<(), String> {
        Err("the QEMU harness does not move the mouse".into())
    }
    fn mouse_button(&mut self, _: u8, _: bool) -> Result<(), String> {
        Err("the QEMU harness does not click".into())
    }
    fn sleep(&mut self, d: Duration) {
        std::thread::sleep(d);
    }
    fn screen(&mut self) -> Option<Frame> {
        self.grab()
    }
    fn reference(&mut self, name: &str) -> Result<Frame, String> {
        kvmit_video::convert::load_reference(&self.base.join(name))
    }
    fn confirm(&mut self, message: &str) -> bool {
        print!("\n? {message} [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        line.trim().eq_ignore_ascii_case("y")
    }
    fn cancelled(&self) -> bool {
        false
    }
    fn event(&mut self, _: RunEvent) {}
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: qemu_workflow <ssh-target> <vmid> <workflow.toml> [--dry-run] [--snap <dir>]");
        std::process::exit(2);
    }
    let path = Path::new(&args[2]);
    let mut host = Qemu {
        ssh: args[0].clone(),
        vmid: args[1].clone(),
        base: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
        held: BTreeSet::new(),
        seq: 0,
    };
    if let Some(i) = args.iter().position(|a| a == "--snap") {
        let dir = PathBuf::from(args.get(i + 1).expect("--snap <dir>"));
        let cmd = format!("echo 'screendump /tmp/kvmit-wf-{0}.ppm' | qm monitor {0} >/dev/null && cat /tmp/kvmit-wf-{0}.ppm", host.vmid);
        let ppm = host.ssh(&cmd).expect("screendump");
        let (w, h, rgb) = parse_ppm(&ppm).expect("a P6 screendump");
        let rgba: Vec<u8> = rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        kvmit_video::convert::save_png(&dir.join("screen.png"), w, h, &rgba).expect("save");
        println!("{}", dir.join("screen.png").display());
        return;
    }
    let wf = Workflow::parse(&std::fs::read_to_string(path).expect("read workflow")).expect("valid workflow");
    let mut opts = WorkflowOptions {
        run: RunOptions { key_hold: Duration::ZERO, key_gap: Duration::from_millis(50), poll: Duration::from_millis(500), ..Default::default() },
        observe_timeout: Duration::from_secs(60),
        ..Default::default()
    };
    opts.run.dry_run = args.iter().any(|a| a == "--dry-run");
    // This harness's workflows use no secrets; a real one would prompt for each, never take them from arguments.
    let report = run_workflow(&wf, &Vars::new(), &kvmit_layout::UsAnsi, &mut host, &opts, None, &mut |t, _| {
        println!("{}", serde_json::to_string(t).unwrap_or_default());
    });
    let stem = path.with_extension("");
    std::fs::write(stem.with_extension("trace.jsonl"), trace_jsonl(&report)).expect("write trace");
    std::fs::write(stem.with_extension("trace.md"), trace_markdown(&wf, &report)).expect("write notes");
    match report.result {
        Ok(()) => println!("\nCOMPLETED in {}", report.context.current.unwrap_or_default()),
        Err(e) => {
            println!("\nSTOPPED: {e}");
            std::process::exit(1);
        }
    }
}
