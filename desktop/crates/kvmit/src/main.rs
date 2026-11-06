use clap::{Parser, Subcommand};
use kvmit::config::Config;
use kvmit::host::ScriptHost;
use kvmit::session;
use kvmit_ble::backend;
use kvmit_hid::parse_key;
use kvmit_layout::UsAnsi;
use kvmit_script::{compile, duckyscript, preview, run, RunEvent, RunOptions, Script, Step, Vars};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "kvmit", version, about = "kvm-it controller: drive a target computer through the ESP32-S3 USB-HID adapter")]
struct Cli {
    /// Adapter id: its Bluetooth address (on macOS, the id macOS gave it) (default: the last one used, else the strongest in range)
    #[arg(long, global = true)]
    device: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the graphical app (default)
    Gui,
    /// List adapters in range
    Scan {
        #[arg(long, default_value_t = backend::DEFAULT_SCAN_SECS)]
        seconds: u64,
    },
    /// Pair with an adapter (within 15 s of plugging it in, or after a brief BOOT press)
    Pair,
    /// Remove the OS pairing for an adapter (also hold BOOT 10 s on the adapter to erase its side)
    Unpair,
    /// Connect and print adapter info, target-USB state and link round-trip time
    Status,
    /// Show, turn on or turn off the adapter's read-only boot drive (iPXE, for network-booting a UEFI target). It is off unless you turn it on; the adapter
    /// restarts when the setting changes, so the target sees it re-plug.
    BootDrive {
        /// on | off (omit to just show the state)
        state: Option<String>,
    },
    /// Type text on the target (reads stdin if no text is given; --secret prompts without echo)
    Type {
        text: Option<String>,
        #[arg(long)]
        secret: bool,
    },
    /// Press a key or chord, e.g. `kvmit key ctrl alt delete`. With --hold, keep it pressed for that long
    /// (e.g. `kvmit key --hold 15s f12` while the target boots); Ctrl+C releases early.
    Key {
        keys: Vec<String>,
        #[arg(long, value_parser = kvmit_script::parse_duration)]
        hold: Option<Duration>,
    },
    /// Flash the adapter's firmware through its UART (COM) port. Never use the board's native USB port for this.
    Flash {
        /// A firmware folder (contains flasher_args.json). Default: $KVMIT_FIRMWARE, else the `firmware` folder next to
        /// the program, else firmware/release in a source checkout
        #[arg(long)]
        firmware: Option<PathBuf>,
        /// Serial port (default: the one adapter UART port found)
        #[arg(long)]
        port: Option<String>,
        /// Flash through a USB serial port that is not a known adapter UART (the native USB port is still refused)
        #[arg(long)]
        any_port: bool,
        /// Erase everything first, including the pairing (bond) and settings
        #[arg(long)]
        erase_all: bool,
        /// List USB serial ports and exit
        #[arg(long)]
        list: bool,
        /// Skip the confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Run a script or a workflow file (`workflow_format = 1`: watches the screen, prompts for its secrets, prints
    /// each state and transition as it happens)
    Run {
        file: PathBuf,
        /// name=value (not for secrets: use the hidden prompt)
        #[arg(long = "var", value_parser = parse_kv)]
        vars: Vec<(String, String)>,
        /// Show what would happen without sending anything
        #[arg(long)]
        dry_run: bool,
        /// Skip the "this will type into the target" confirmation
        #[arg(long)]
        yes: bool,
        /// Capture card for screen waits, by its key (stable across replugs) or path from `kvmit video list`.
        /// Default: the card last opened in the app if it is connected, else (none remembered) the only card connected;
        /// otherwise the run stops and asks. With `--device`, one adapter and card pair per process.
        #[arg(long)]
        video: Option<String>,
        /// Workflows: continue the run saved when this file last stopped. The screen is looked at first; the run goes on
        /// only if it shows that state or one that may follow it, and actions already sent are not sent again unasked.
        #[arg(long)]
        resume: bool,
        /// Workflows: when the run ends, save its trace (JSONL) and notes (Markdown) in the scripts folder. No secrets.
        #[arg(long)]
        save_trace: bool,
        /// Workflows: check the file and list its reference images (present or missing), without connecting to anything.
        /// Make a missing one with `kvmit video snap <path>` while the target shows that screen.
        #[arg(long)]
        check: bool,
    },
    /// Convert a DuckyScript payload to the native format and list anything untranslatable
    Import {
        file: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Capture devices
    Video {
        #[command(subcommand)]
        cmd: VideoCmd,
    },
    /// Workflows: make your own copy of a built-in one and set its profile (deployment mode, organisation, ...)
    Workflow {
        #[command(subcommand)]
        cmd: WorkflowCmd,
    },
}

#[derive(Subcommand)]
enum WorkflowCmd {
    /// List the built-in workflows and the workflow files in the scripts folder
    List,
    /// Show a workflow's setup values: what each means, its value now, and its choices
    Show {
        /// A built-in workflow's name, or a workflow file
        workflow: String,
    },
    /// Make your own copy of a built-in workflow in the scripts folder, with its setup values. In a terminal, asks
    /// for each one not given with --set (Enter keeps the value shown).
    New {
        /// The built-in workflow's name (`kvmit workflow list`)
        workflow: String,
        /// name=value, for a setup value (`kvmit workflow show <workflow>` lists them)
        #[arg(long = "set", value_parser = parse_kv)]
        set: Vec<(String, String)>,
        /// The new file's name in the scripts folder (default: the workflow's name and the organisation's)
        #[arg(long)]
        name: Option<String>,
    },
    /// Change setup values of a workflow file in place. A checkpoint saved from it before no longer resumes.
    Set {
        file: PathBuf,
        /// name=value, for a setup value
        #[arg(long = "set", value_parser = parse_kv, required = true)]
        set: Vec<(String, String)>,
    },
}

#[derive(Subcommand)]
enum VideoCmd {
    List,
    /// Save the current frame as a PNG (use it as a `wait_for` reference image). Waits for a non-blank frame:
    /// capture cards send a flat fill while they lock onto the source.
    Snap {
        output: PathBuf,
        #[arg(long)]
        path: Option<String>,
        /// How long to wait for a non-blank frame before saving whatever arrived
        #[arg(long, default_value = "8s", value_parser = kvmit_script::parse_duration)]
        timeout: Duration,
    },
}

fn parse_kv(s: &str) -> Result<(String, String), String> {
    s.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).ok_or_else(|| "expected name=value".to_string())
}

type R<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn main() {
    if kvmit::syskeys::run_helper_if_requested() {
        return; // started as the keyboard-grab helper (Windows)
    }
    let cli = Cli::parse();
    if let Err(e) = real_main(cli) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime")
}

/// Record a setting for later runs. A settings file that cannot be read is left alone (`Config::update`); the command
/// itself still goes ahead, so this is a warning, not an error.
fn save_settings(cfg: &Config, f: impl Fn(&mut Config)) {
    if let Err(e) = cfg.clone().update(f) {
        eprintln!("warning: {e}");
    }
}

fn real_main(cli: Cli) -> R<()> {
    let cfg = Config::load();
    match cli.cmd.unwrap_or(Cmd::Gui) {
        Cmd::Gui => kvmit::gui::run_gui().map_err(|e| e.to_string().into()),
        Cmd::Scan { seconds } => rt().block_on(async {
            let found = backend::scan(Duration::from_secs(seconds)).await?;
            if found.is_empty() {
                println!("no kvm-it adapters found");
            }
            for f in found {
                println!("{}  {}  {}", f.id, f.name, f.rssi.map(|r| format!("{r} dBm")).unwrap_or_default());
            }
            Ok(())
        }),
        Cmd::Pair => rt().block_on(async {
            let id = session::resolve(cli.device, None).await?;
            println!("pairing with {id} (the adapter must be in its pairing window: first 15 s after plug-in, or BOOT short press)…");
            backend::pair(&id).await?;
            save_settings(&cfg, |c| c.last_device = Some(id.clone()));
            let (dev, conn) = session::connect(&id).await?;
            println!("paired and connected: {} (firmware {}.{}.{})", dev.info().name, dev.info().fw[0], dev.info().fw[1], dev.info().fw[2]);
            dev.shutdown().await;
            conn.disconnect().await;
            Ok(())
        }),
        Cmd::Flash { firmware, port, any_port, erase_all, list, yes } => {
            kvmit::flashcmd::run(kvmit::flashcmd::Args { firmware, port, any_port, erase_all, yes, list })
        }
        Cmd::Unpair => rt().block_on(async {
            let id = session::resolve(cli.device, cfg.last_device.clone()).await?;
            backend::unpair(&id).await?;
            save_settings(&cfg, |c| c.last_device = None);
            println!("removed pairing for {id}. To erase the adapter's side too, hold BOOT for 10 s.");
            Ok(())
        }),
        Cmd::Status => with_device(cli.device, cfg, |rt, dev| {
            let i = dev.info();
            println!("adapter: {} (protocol v{}.{}, firmware {}.{}.{})", i.name, i.major, i.minor, i.fw[0], i.fw[1], i.fw[2]);
            let s = rt.block_on(dev.status())?;
            println!("target USB: {}", if s.hid_mounted { "enumerated" } else { "NOT enumerated (cable, power or suspend?)" });
            println!("held: {} keys, buttons {:#04x}; dropped motion {}; bad frames {}", s.keys, s.buttons, s.dropped_motion, s.bad_crc);
            match s.boot_drive {
                Some(on) => println!("boot drive: {}", if on { "ON (the target sees a read-only iPXE disk)" } else { "off" }),
                None => println!("boot drive: not supported by this firmware (needs 0.2.0 or newer)"),
            }
            println!("round trip: {} ms", rt.block_on(dev.ping())?.as_millis());
            Ok(())
        }),
        Cmd::BootDrive { state } => with_device(cli.device, cfg, |rt, dev| {
            let want = match state.as_deref() {
                None => None,
                Some("on") => Some(true),
                Some("off") => Some(false),
                Some(other) => return Err(format!("expected `on` or `off`, not {other:?}").into()),
            };
            let now = rt.block_on(dev.status())?.boot_drive;
            match (want, now) {
                (_, None) => Err("this adapter's firmware has no boot drive (needs 0.2.0 or newer): flash it with `kvmit flash`".into()),
                (None, Some(on)) => {
                    println!("boot drive: {}", if on { "on" } else { "off" });
                    Ok(())
                }
                (Some(w), Some(n)) if w == n => {
                    println!("boot drive is already {}", if n { "on" } else { "off" });
                    Ok(())
                }
                (Some(w), Some(_)) => {
                    rt.block_on(dev.set_boot_drive(w))?;
                    println!("boot drive {}: the adapter restarts now and the target sees it re-plug (reconnect in a few seconds).", if w { "turned on" } else { "turned off" });
                    Ok(())
                }
            }
        }),
        Cmd::Type { text, secret } => {
            let text = match text {
                Some(t) => t,
                None if secret => rpassword::prompt_password("Text to type (hidden): ")?,
                None => {
                    let mut s = String::new();
                    std::io::stdin().read_line(&mut s)?;
                    s.trim_end_matches(['\r', '\n']).to_string()
                }
            };
            let mut script = Script::default();
            script.vars.insert("t".into(), kvmit_script::VarDef { secret, ..Default::default() });
            script.steps.push(if secret { Step::SecretText("{{t}}".into()) } else { Step::Text("{{t}}".into()) });
            let vars: Vars = [("t".to_string(), text)].into();
            execute(cli.device, None, cfg, script, PathBuf::from("."), vars, false, true)
        }
        Cmd::Key { keys, hold } => {
            if keys.is_empty() {
                return Err("give at least one key name, e.g. `kvmit key enter`".into());
            }
            for k in &keys {
                parse_key(k).ok_or_else(|| format!("unknown key {k:?}"))?;
            }
            if let Some(hold) = hold {
                let ks: Vec<kvmit_hid::Key> = keys.iter().filter_map(|k| parse_key(k)).collect();
                let cancel = Arc::new(AtomicBool::new(false));
                let c2 = cancel.clone();
                ctrlc::set_handler(move || c2.store(true, Ordering::SeqCst)).ok();
                return with_device(cli.device, cfg, |rt, dev| {
                    rt.block_on(async {
                        let mut r = Ok(());
                        for k in &ks {
                            if let Err(e) = dev.key_down(*k).await {
                                r = Err(e);
                                break;
                            }
                        }
                        if r.is_ok() {
                            println!("holding {} for {hold:?} (Ctrl+C releases)", keys.join("+"));
                            let end = tokio::time::Instant::now() + hold;
                            while tokio::time::Instant::now() < end && !cancel.load(Ordering::SeqCst) {
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        }
                        // Release even after an error; with_device's shutdown also sends RELEASE_ALL.
                        for k in ks.iter().rev() {
                            let _ = dev.key_up(*k).await;
                        }
                        r.map_err(|e| e.to_string().into())
                    })
                });
            }
            let mut script = Script::default();
            script.steps.push(if keys.len() == 1 { Step::Key(keys[0].clone()) } else { Step::Chord(keys) });
            execute(cli.device, None, cfg, script, PathBuf::from("."), Vars::new(), false, true)
        }
        Cmd::Run { file, vars, dry_run, yes, video, resume, save_trace, check } => {
            let src = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            if kvmit_script::workflow::is_workflow_source(&src) {
                if !vars.is_empty() {
                    return Err("--var is for scripts: a workflow's values are its [config] (the approved profile) and its prompted [secrets]".into());
                }
                if check {
                    return check_workflow(&file, &src);
                }
                let run = WfRun { file, dry: dry_run, yes, resume, save_trace };
                return run_workflow_cmd(cli.device, video, cfg, &src, run);
            }
            if resume || save_trace || check {
                return Err("--resume, --save-trace and --check are for workflows (files with `workflow_format = 1`)".into());
            }
            let script = Script::parse(&src)?;
            let mut v: Vars = vars.into_iter().collect();
            for (name, def) in &script.vars {
                if v.contains_key(name) || (def.default.is_some() && !def.secret) {
                    continue;
                }
                let prompt = format!("{}{}: ", def.prompt.as_deref().unwrap_or(name), if def.secret { " (hidden)" } else { "" });
                let val = if def.secret { rpassword::prompt_password(prompt)? } else {
                    print!("{prompt}");
                    std::io::stdout().flush()?;
                    let mut s = String::new();
                    std::io::stdin().read_line(&mut s)?;
                    s.trim_end().to_string()
                };
                v.insert(name.clone(), val);
            }
            let p = preview(&script, &v, &UsAnsi, &RunOptions::default())?;
            println!("{}: {} steps, {} chars typed, {} secret chars, ~{} s typing{}", script.name, p.steps, p.typed_chars, p.secret_chars, p.estimated_typing.as_secs(), if p.needs_video { ", needs video" } else { "" });
            for s in &p.suspicious {
                println!("  warning: types something that looks like a command: {s}");
            }
            let base = file.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
            execute(cli.device, video, cfg, script, base, v, dry_run, yes || dry_run)
        }
        Cmd::Import { file, output } => {
            let src = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let (script, report) = duckyscript::import(&src, &name);
            let toml = script.to_toml();
            match output {
                Some(o) => {
                    std::fs::write(&o, &toml)?;
                    println!("wrote {} ({} lines translated)", o.display(), report.translated);
                }
                None => print!("{toml}"),
            }
            for (n, l) in &report.unsupported {
                eprintln!("not translated, line {n}: {l}");
            }
            if !report.unsupported.is_empty() {
                std::process::exit(2);
            }
            Ok(())
        }
        Cmd::Workflow { cmd } => workflow_cmd(cmd, &cfg),
        Cmd::Video { cmd } => match cmd {
            VideoCmd::List => {
                let d = kvmit_video::list_devices();
                if d.is_empty() {
                    println!("no capture devices");
                }
                for x in d {
                    // the key is what `run --video` should be given: the path can change across replugs
                    let key = if x.key != x.path { format!("  (key: {})", x.key) } else { String::new() };
                    println!("{}  {}{key}", x.path, x.name);
                }
                Ok(())
            }
            VideoCmd::Snap { output, path, timeout } => {
                let path = match path {
                    Some(p) => p,
                    None => {
                        let devices = kvmit_video::list_devices();
                        // the remembered device by its stable key, else the first one (an explicit command, not a startup guess)
                        let i = kvmit::video_state::initial_video(&devices, cfg.last_video_key.as_deref(), cfg.last_video.as_deref()).unwrap_or(0);
                        devices.get(i).map(|d| d.path.clone()).ok_or("no capture device")?
                    }
                };
                let cap = kvmit_video::Capture::open(&path)?;
                let deadline = std::time::Instant::now() + timeout;
                let mut last = None;
                let frame = loop {
                    if let Some(f) = cap.latest() {
                        if !f.is_blank() {
                            break Some(f);
                        }
                        last = Some(f);
                    }
                    if std::time::Instant::now() > deadline {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                };
                let (frame, blank) = match (frame, last) {
                    (Some(f), _) => (f, false),
                    (None, Some(f)) => (f, true),
                    (None, None) => return Err(format!("no frame within {timeout:?} (is the capture card working?)").into()),
                };
                image_save(&output, frame.width, frame.height, &frame.rgba)?;
                println!("saved {}x{} frame to {}", frame.width, frame.height, output.display());
                if blank {
                    eprintln!(
                        "warning: the frame is blank (one flat colour): the card has no signal yet. Check the source is \
                         outputting (Win+P: Duplicate); if it is, replug the capture card's USB and try again."
                    );
                }
                Ok(())
            }
        },
    }
}

fn image_save(path: &std::path::Path, w: usize, h: usize, rgba: &[u8]) -> R<()> {
    // kvmit-video already depends on `image`; re-use it via the PNG writer in std-free form
    kvmit_video::convert::save_png(path, w, h, rgba).map_err(|e| e.into())
}

fn with_device<T>(id: Option<String>, cfg: Config, f: impl FnOnce(&tokio::runtime::Runtime, &kvmit_ble::Device) -> R<T>) -> R<T> {
    let rt = rt();
    let id = rt.block_on(session::resolve(id, cfg.last_device.clone()))?;
    let (dev, conn) = rt.block_on(session::connect(&id))?;
    // skip a no-op save: several `kvmit` processes (one per adapter) may be running
    if cfg.last_device.as_deref() != Some(id.as_str()) {
        save_settings(&cfg, |c| c.last_device = Some(id.clone()));
    }
    let out = f(&rt, &dev);
    rt.block_on(async {
        dev.shutdown().await;
        conn.disconnect().await;
    });
    out
}

/// The capture card a run watches: `--video`, else the one last opened in the app, else the only one connected.
fn open_capture(video: Option<&str>, cfg: &Config) -> R<kvmit_video::Capture> {
    let devices = kvmit_video::list_devices();
    let d = &devices[kvmit::video_state::run_video(&devices, video, cfg.last_video_key.as_deref())?];
    let cap = kvmit_video::Capture::open(&d.path).map_err(|e| format!("capture device {}: {e}", d.name))?;
    // the path is only a handle: if the card was swapped between listing and opening, it names another one
    if cap.info.key != d.key {
        return Err(format!("capture device changed while opening {} (now {}): run again", d.key, cap.info.key).into());
    }
    Ok(cap)
}

fn ask_stdin(msg: &str) -> bool {
    print!("{msg} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut a = String::new();
    std::io::stdin().read_line(&mut a).ok();
    a.trim().eq_ignore_ascii_case("y")
}

/// `kvmit run --check`: the file parses, and which reference images are present (the app's workflow panel lists the
/// same and saves missing ones from the video).
fn check_workflow(file: &std::path::Path, src: &str) -> R<()> {
    let wf = kvmit_script::Workflow::parse(src).map_err(|e| format!("{}: {e}", file.display()))?;
    let base = file.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let refs = kvmit::wf_view::references(&wf, &base);
    println!("{}: {} states, {} reference images", file.display(), wf.states.len(), refs.len());
    let mut missing = 0;
    for r in &refs {
        let (mark, path) = match &r.path {
            Ok(p) if r.present => ("ok     ", p.display().to_string()),
            Ok(p) => ("MISSING", p.display().to_string()),
            Err(e) => ("INVALID", e.clone()),
        };
        missing += usize::from(!r.present);
        println!("  {mark} {path}  ({})", r.used_by.join(", "));
    }
    let invalid = refs.iter().filter(|r| r.path.is_err()).count();
    if invalid > 0 {
        return Err(format!("{invalid} reference image name(s) are not valid paths: fix them in the file").into());
    }
    if missing > 0 {
        println!(
            "{missing} reference image(s) missing: a screen whose pictures are all missing is not recognised, and the run stops if it appears. \
             Bring the target to each screen you can and run `kvmit video snap <path>` (or use Save this screen as in the app)."
        );
        return Ok(());
    }
    println!("ready: every reference image is present");
    Ok(())
}

/// A workflow named on the command line: an existing file, else a built-in's name. Returns its text and how to call it.
fn named_workflow(name: &str) -> R<(String, String, Option<PathBuf>)> {
    let path = PathBuf::from(name);
    if path.is_file() {
        let src = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        return Ok((src, path.display().to_string(), Some(path)));
    }
    match kvmit::library::builtin(name) {
        Some(b) if kvmit::library::is_workflow(b.source) => Ok((b.source.to_string(), format!("{} (built in)", b.file), None)),
        _ => Err(format!("{name}: no such file, and not a built-in workflow (`kvmit workflow list`)").into()),
    }
}

fn print_setup(wf: &kvmit_script::Workflow) {
    if wf.setup.is_empty() {
        println!("  no setup values: change it by editing its file");
    }
    for f in &wf.setup {
        let now = wf.config.get(&f.key).map(String::as_str).unwrap_or("");
        println!("  {} = {now:?}   {}", f.key, f.label);
        if !f.help.is_empty() {
            println!("      {}", f.help);
        }
        for c in &f.choices {
            println!("      {} {}: {}", if c.value == now { "*" } else { "-" }, c.value, c.label);
        }
    }
}

/// Ask for one setup value in the terminal: Enter keeps `now`; a choice is given by its number or its value.
fn ask_setup(f: &kvmit_script::workflow::SetupField, now: &str) -> R<String> {
    println!("{}", f.label);
    if !f.help.is_empty() {
        println!("  {}", f.help);
    }
    for (i, c) in f.choices.iter().enumerate() {
        println!("  {}. {}{}", i + 1, c.label, if c.value == now { " (now)" } else { "" });
    }
    loop {
        print!("{} [{now}]: ", f.key);
        std::io::stdout().flush()?;
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer)? == 0 {
            return Err("no answer (end of input)".into());
        }
        let v = match answer.trim() {
            "" => now.to_string(),
            a => match a.parse::<usize>() {
                Ok(n) if (1..=f.choices.len()).contains(&n) => f.choices[n - 1].value.clone(),
                _ => a.to_string(),
            },
        };
        match f.check(&v) {
            Ok(()) => return Ok(v),
            Err(e) => println!("  {e}"),
        }
    }
}

fn workflow_cmd(cmd: WorkflowCmd, cfg: &Config) -> R<()> {
    use std::io::IsTerminal;
    let dir = cfg.script_dir();
    match cmd {
        WorkflowCmd::List => {
            for e in kvmit::library::load_all(&dir).iter().filter(|e| e.workflow) {
                match (&e.path, e.builtin_file()) {
                    (_, Some(f)) => println!("{f}  (built in: `kvmit workflow new {}`)", f.trim_end_matches(".toml")),
                    (Some(p), None) => println!("{}{}", p.display(), e.error.as_ref().map(|x| format!("  (does not parse: {x})")).unwrap_or_default()),
                    (None, None) => {}
                }
            }
            println!("scripts folder: {}", dir.display());
            Ok(())
        }
        WorkflowCmd::Show { workflow } => {
            let (src, label, _) = named_workflow(&workflow)?;
            let wf = kvmit_script::Workflow::parse(&src).map_err(|e| format!("{label}: {e}"))?;
            println!("{label}: {}", if wf.name.is_empty() { "workflow" } else { &wf.name });
            print_setup(&wf);
            Ok(())
        }
        WorkflowCmd::New { workflow, set, name } => {
            let Some(b) = kvmit::library::builtin(&workflow).filter(|b| kvmit::library::is_workflow(b.source)) else {
                return Err(format!("{workflow}: not a built-in workflow (`kvmit workflow list`); to change a file of yours, use `kvmit workflow set`").into());
            };
            let wf = kvmit_script::Workflow::parse(b.source)?;
            let mut values: std::collections::BTreeMap<String, String> = set.into_iter().collect();
            if std::io::stdin().is_terminal() {
                for f in &wf.setup {
                    if !values.contains_key(&f.key) {
                        let v = ask_setup(f, wf.config.get(&f.key).map(String::as_str).unwrap_or(""))?;
                        values.insert(f.key.clone(), v);
                    }
                }
            }
            let text = kvmit_script::setup::apply(b.source, &values)?;
            let name = name.unwrap_or_else(|| kvmit::wf_view::copy_file_name(b.file, &values));
            let path = kvmit::wf_view::write_new_workflow(&dir, &name, &text)?;
            println!("wrote {}", path.display());
            print_setup(&kvmit_script::Workflow::parse(&text)?);
            println!("next: save its reference screens (`kvmit run {} --check` lists them), then a dry run", path.display());
            Ok(())
        }
        WorkflowCmd::Set { file, set } => {
            let was = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            if !kvmit_script::workflow::is_workflow_source(&was) {
                return Err(format!("{}: not a workflow file", file.display()).into());
            }
            let text = kvmit_script::setup::apply(&was, &set.into_iter().collect())?;
            kvmit::wf_view::rewrite_workflow(&file, &was, &text)?;
            println!("updated {}", file.display());
            print_setup(&kvmit_script::Workflow::parse(&text)?);
            Ok(())
        }
    }
}

struct WfRun {
    file: PathBuf,
    dry: bool,
    yes: bool,
    resume: bool,
    save_trace: bool,
}

/// `kvmit run` for a workflow: what the app's workflow panel does, in a terminal. Secrets are prompted without echo,
/// each state, transition and question is printed as it happens, and the variables are listed at the end. A stopped
/// live run saves a checkpoint that the app and `--resume` share; a completed one clears it.
fn run_workflow_cmd(id: Option<String>, video: Option<String>, cfg: Config, src: &str, a: WfRun) -> R<()> {
    use kvmit::wf_view;
    let wf = kvmit_script::Workflow::parse(src).map_err(|e| format!("{}: {e}", a.file.display()))?;
    let source = wf_view::file_source(&a.file);
    let dir = cfg.script_dir();
    // Secrets first: nothing from the file (or its checkpoint) is printed until it is known not to hold a secret's
    // value, since this output may be going to a log.
    let mut secrets = Vars::new();
    for (name, prompt) in wf.secrets_in_order() {
        if a.dry {
            break; // a dry run sends nothing: placeholders below
        }
        let v = rpassword::prompt_password(format!("{prompt} (hidden): "))?;
        if v.is_empty() {
            return Err(format!("{prompt}: a value is required").into());
        }
        secrets.insert(name.to_string(), v);
    }
    if a.dry {
        secrets = wf_view::dry_run_secrets(&wf, &secrets);
    }
    // Checkpoints belong to a workflow on one adapter: the one this run will use.
    // (A dry run sends nothing and saves no checkpoint: it needs an adapter only to look one up for --resume.)
    let adapter = if a.dry && !a.resume { String::new() } else { rt().block_on(session::resolve(id.clone(), cfg.last_device.clone()))? };
    let slot = wf_view::Slot::new(source.clone(), &adapter);
    // Read now to show it and check it; the run itself starts from the copy taken under the slot's lock, and only if it
    // is still this one.
    let saved = if a.resume { wf_view::load_checkpoint_raw(&dir, &slot, src)? } else { None };
    // Field by field, decoded (an escape in the file cannot hide a value), as the engine checks before it publishes.
    kvmit_script::workflow::check_secrets(&wf, &secrets, saved.as_ref().map(|(_, cp)| cp))?;
    let name = if wf.name.is_empty() { "(unnamed)" } else { wf.name.as_str() };
    println!("workflow: {name}");
    if !wf.description.is_empty() {
        println!("  {}", wf.description);
    }
    println!("states: {}", wf.states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(" · "));
    for (k, v) in &wf.config {
        println!("  approved profile: {k} = {v}");
    }
    let shown = if a.resume {
        let (raw, cp) = saved.ok_or("no saved checkpoint for this workflow on this adapter (none was saved, the run completed, or the file changed since)")?;
        println!("resuming from {} (the screen is looked at first)", cp.state);
        Some(raw)
    } else {
        None
    };
    if !a.yes && !a.dry && !ask_stdin("This workflow will type into the target computer. Did you write or review it?") {
        return Err("cancelled".into());
    }
    // A workflow always watches the screen, a dry run too.
    let capture = Arc::new(std::sync::Mutex::new(Some(open_capture(video.as_deref(), &cfg)?)));
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    ctrlc::set_handler(move || c2.store(true, Ordering::SeqCst)).ok();
    let base = a.file.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let go = |rt: &tokio::runtime::Handle, dev: Option<kvmit_ble::Device>, resume: Option<&kvmit_script::Checkpoint>| {
        let mut host = ScriptHost {
            rt: rt.clone(),
            dev,
            capture: Some(capture.clone()),
            base_dir: base.clone(),
            cancel: cancel.clone(),
            confirm: Box::new(ask_stdin),
            // The workflow's own trace says what happened; the nested script runner's step events would only repeat it.
            on_event: Box::new(|_| {}),
        };
        let mut opts = kvmit_script::WorkflowOptions::default();
        opts.run.dry_run = a.dry;
        kvmit_script::run_workflow(&wf, &secrets, &UsAnsi, &mut host, &opts, resume, &mut |t, _| {
            for line in wf_view::cli_lines(t) {
                println!("{line}");
            }
        })
    };
    // Kept as soon as the run ends, inside the connection: tearing the link down afterwards can be slow.
    let keep = |report: &kvmit_script::WorkflowReport, claim: Option<&wf_view::Claim>| {
        wf_view::keep_checkpoint(&dir, &slot, src, a.dry, &wf, report, claim)
    };
    let (report, kept) = if a.dry {
        // A dry run sends nothing and takes no lock: it may follow the checkpoint as read.
        let dry_from = shown.as_ref().and_then(|raw| {
            wf_view::load_checkpoint_raw(&dir, &slot, src).ok().flatten().filter(|(now, _)| now == raw).map(|(_, cp)| cp)
        });
        if shown.is_some() && dry_from.is_none() {
            return Err("the checkpoint changed since it was read (another run saved one): run again".into());
        }
        let r = go(rt().handle(), None, dry_from.as_ref());
        let k = keep(&r, None);
        (r, k)
    } else {
        with_device(Some(adapter), cfg.clone(), |rt, dev| {
            // Connected; before any key: the slot is locked and its saved checkpoint set aside, so none older than this
            // run can be resumed, and no second run of this workflow on this adapter starts meanwhile.
            let mut claim = wf_view::claim_checkpoint(&dir, &slot)?;
            // A resume starts from the checkpoint as read under the lock, and only if it is the one shown above.
            let resume = match &shown {
                Some(raw) => {
                    let (c, cp) = wf_view::resume_from_claim(&dir, &slot, src, claim.take(), raw)?;
                    claim = c;
                    Some(cp)
                }
                None => None,
            };
            let r = go(rt.handle(), Some(dev.clone()), resume.as_ref());
            let k = keep(&r, claim.as_ref());
            Ok((r, k))
        })?
    };
    for v in secrets.values_mut() {
        zeroize::Zeroize::zeroize(v);
    }
    println!("variables:");
    for v in report.context.inspect() {
        println!("  {:<22} {:<20} {}", format!("{} ({:?})", v.namespace, v.provenance).to_lowercase(), v.key, v.value);
    }
    if let Some(msg) = kept {
        let hint = if msg.starts_with("Saved") { " (`kvmit run <file> --resume`, or Resume in the app)" } else { "" };
        println!("{msg}{hint}");
    }
    if a.save_trace {
        let jsonl = kvmit_script::workflow::trace_jsonl(&report);
        let md = kvmit_script::workflow::trace_markdown(&wf, &report);
        match wf_view::save_exports(&dir, &kvmit_record::naming::stamp_now(), &jsonl, &md) {
            Ok(stem) => println!("saved {stem}.trace.jsonl and {stem}.notes.md in {}", dir.display()),
            Err(e) => eprintln!("warning: could not save the trace: {e}"),
        }
    }
    report.result.map_err(|e| e.to_string().into())
}

#[allow(clippy::too_many_arguments)]
fn execute(id: Option<String>, video: Option<String>, cfg: Config, script: Script, base: PathBuf, vars: Vars, dry: bool, yes: bool) -> R<()> {
    let ops = compile(&script, &vars, &UsAnsi)?;
    if !yes {
        print!("This will type into the target computer. Continue? [y/N] ");
        std::io::stdout().flush()?;
        let mut a = String::new();
        std::io::stdin().read_line(&mut a)?;
        if !a.trim().eq_ignore_ascii_case("y") {
            return Err("cancelled".into());
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    ctrlc::set_handler(move || c2.store(true, Ordering::SeqCst)).ok();
    // Only open a capture device when the script waits on the screen: holding it otherwise blocks other users of
    // the card (e.g. `kvmit video snap`) for no reason.
    // A dry run skips waits, so it neither needs nor holds a card.
    let video = if kvmit_script::exec::needs_video(&ops) && !dry { Some(open_capture(video.as_deref(), &cfg)?) } else { None };
    let capture = Arc::new(std::sync::Mutex::new(video));
    with_device(id, cfg, |rt, dev| {
        let mut host = ScriptHost {
            rt: rt.handle().clone(),
            dev: Some(dev.clone()),
            capture: Some(capture),
            base_dir: base,
            cancel,
            confirm: Box::new(ask_stdin),
            on_event: Box::new(|ev| match ev {
                RunEvent::StepStarted { index, kind, detail } => println!("step {}: {kind} {detail}", index + 1),
                RunEvent::Finished => println!("done"),
                RunEvent::Aborted { reason } => eprintln!("stopped: {reason}"),
                _ => {}
            }),
        };
        run(&ops, &mut host, &RunOptions { dry_run: dry, ..Default::default() })?;
        Ok(())
    })
}
