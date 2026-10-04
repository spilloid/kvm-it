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
    /// Adapter address (default: the last one used, else the strongest in range)
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
        /// A firmware build directory (contains flasher_args.json)
        #[arg(long, default_value = "firmware/build")]
        firmware: PathBuf,
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
    /// Run a script file
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
            let mut c = cfg;
            c.last_device = Some(id.clone());
            c.save();
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
            let mut c = cfg;
            c.last_device = None;
            c.save();
            println!("removed pairing for {id}. To erase the adapter's side too, hold BOOT for 10 s.");
            Ok(())
        }),
        Cmd::Status => with_device(cli.device, cfg, |rt, dev| {
            let i = dev.info();
            println!("adapter: {} (protocol v{}.{}, firmware {}.{}.{})", i.name, i.major, i.minor, i.fw[0], i.fw[1], i.fw[2]);
            let s = rt.block_on(dev.status())?;
            println!("target USB: {}", if s.hid_mounted { "enumerated" } else { "NOT enumerated (cable, power or suspend?)" });
            println!("held: {} keys, buttons {:#04x}; dropped motion {}; bad frames {}", s.keys, s.buttons, s.dropped_motion, s.bad_crc);
            println!("round trip: {} ms", rt.block_on(dev.ping())?.as_millis());
            Ok(())
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
            execute(cli.device, cfg, script, PathBuf::from("."), vars, false, true)
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
            execute(cli.device, cfg, script, PathBuf::from("."), Vars::new(), false, true)
        }
        Cmd::Run { file, vars, dry_run, yes } => {
            let src = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
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
            execute(cli.device, cfg, script, base, v, dry_run, yes || dry_run)
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
        Cmd::Video { cmd } => match cmd {
            VideoCmd::List => {
                let d = kvmit_video::list_devices();
                if d.is_empty() {
                    println!("no capture devices");
                }
                for x in d {
                    println!("{}  {}", x.path, x.name);
                }
                Ok(())
            }
            VideoCmd::Snap { output, path, timeout } => {
                let path = match path.or(cfg.last_video) {
                    Some(p) => p,
                    None => kvmit_video::list_devices().first().map(|d| d.path.clone()).ok_or("no capture device")?,
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
    let mut c = cfg;
    c.last_device = Some(id);
    c.save();
    let out = f(&rt, &dev);
    rt.block_on(async {
        dev.shutdown().await;
        conn.disconnect().await;
    });
    out
}

fn execute(id: Option<String>, cfg: Config, script: Script, base: PathBuf, vars: Vars, dry: bool, yes: bool) -> R<()> {
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
    let needs_video = ops.iter().any(|(_, o)| matches!(o, kvmit_script::Op::Wait(_)));
    let video = if needs_video { kvmit_video::list_devices().first().and_then(|d| kvmit_video::Capture::open(&d.path).ok()) } else { None };
    let capture = Arc::new(std::sync::Mutex::new(video));
    if needs_video && capture.lock().unwrap().is_none() {
        eprintln!("note: this script waits on the screen but no capture device could be opened");
    }
    with_device(id, cfg, |rt, dev| {
        let mut host = ScriptHost {
            rt: rt.handle().clone(),
            dev: dev.clone(),
            capture: Some(capture),
            base_dir: base,
            cancel,
            confirm: Box::new(|msg| {
                print!("{msg} [y/N] ");
                let _ = std::io::stdout().flush();
                let mut a = String::new();
                std::io::stdin().read_line(&mut a).ok();
                a.trim().eq_ignore_ascii_case("y")
            }),
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
