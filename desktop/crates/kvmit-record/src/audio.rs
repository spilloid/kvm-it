//! Audio INPUT device listing, kept separate from video devices (`kvmit_video::list_devices`). Audio is opt-in:
//! nothing here is called unless the user picks a device. The parsers are pure and tested on sample output.
use crate::args::{AudioBackend, AudioInput};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    /// What to pass to ffmpeg (`-i <id>`; dshow gets the `audio=` prefix added by the arg builder).
    pub id: String,
    /// Human-readable name for a picker.
    pub description: String,
    /// A "monitor of <output>" source: captures what the host plays, not a microphone.
    pub monitor: bool,
    pub backend: AudioBackend,
}

impl AudioDevice {
    pub fn as_input(&self) -> AudioInput {
        AudioInput {
            backend: self.backend,
            device: self.id.clone(),
        }
    }
}

/// `pactl list short sources`: `index<TAB>name<TAB>driver<TAB>sample-spec<TAB>state`.
pub fn parse_pactl_sources(text: &str) -> Vec<AudioDevice> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let _idx = f.next()?;
            let name = f.next()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(AudioDevice {
                id: name.to_string(),
                description: name.to_string(),
                monitor: name.ends_with(".monitor"),
                backend: AudioBackend::Pulse,
            })
        })
        .collect()
}

/// `ffmpeg -sources pulse`: `  [*] name [Description] (none)` after a header line.
pub fn parse_ffmpeg_sources_pulse(text: &str) -> Vec<AudioDevice> {
    text.lines()
        .filter(|l| l.starts_with(' ') || l.starts_with('*'))
        .filter_map(|l| {
            let l = l.trim_start_matches([' ', '*']).trim_end();
            let l = l.strip_suffix("(none)").unwrap_or(l).trim_end();
            let (name, rest) = l.split_once(' ')?;
            let desc = rest.trim().trim_start_matches('[').trim_end_matches(']');
            Some(AudioDevice {
                id: name.to_string(),
                description: if desc.is_empty() {
                    name.to_string()
                } else {
                    desc.to_string()
                },
                monitor: name.ends_with(".monitor"),
                backend: AudioBackend::Pulse,
            })
        })
        .collect()
}

/// `ffmpeg -list_devices true -f dshow -i dummy` (stderr). Handles both the newer format (each device line ends
/// in `(audio)` / `(video)`) and the older one (a `DirectShow audio devices` section header). `Alternative name`
/// lines are skipped.
pub fn parse_dshow_audio(text: &str) -> Vec<AudioDevice> {
    let mut out = Vec::new();
    let mut section_audio = false;
    for line in text.lines() {
        let body = line.find(']').map_or(line, |i| &line[i + 1..]).trim();
        if body.contains("DirectShow audio devices") {
            section_audio = true;
            continue;
        }
        if body.contains("DirectShow video devices") {
            section_audio = false;
            continue;
        }
        if body.starts_with("Alternative name") || !body.starts_with('"') {
            continue;
        }
        let Some(end) = body[1..].find('"') else {
            continue;
        };
        let name = &body[1..1 + end];
        let tail = body[end + 2..].trim();
        let is_audio = if tail.contains("(audio)") {
            true
        } else if tail.contains("(video)") {
            false
        } else {
            section_audio
        };
        if is_audio && !name.is_empty() {
            out.push(AudioDevice {
                id: name.to_string(),
                description: name.to_string(),
                monitor: false,
                backend: AudioBackend::Dshow,
            });
        }
    }
    out
}

/// How long a device lookup may take before its tool is killed.
const LOOKUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn run_text(prog: &str, args: &[&str], stderr: bool) -> Option<String> {
    let mut c = std::process::Command::new(prog);
    c.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = c.spawn().ok()?;
    // A hung audio server must not hang the lookup: wait a bounded time, then kill and reap the child.
    fn drain(mut r: impl std::io::Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = r.read_to_end(&mut v);
            v
        })
    }
    let (to, te) = (drain(child.stdout.take()?), drain(child.stderr.take()?));
    let deadline = std::time::Instant::now() + LOOKUP_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let (o, e) = (to.join().ok()?, te.join().ok()?);
    Some(String::from_utf8_lossy(if stderr { &e } else { &o }).into_owned())
}

/// Audio inputs on this machine. Empty (not an error) when the tools are absent. Linux: `pactl`, falling back
/// to `ffmpeg -sources pulse`. Windows: ffmpeg's dshow listing. Other platforms: empty.
pub fn list_audio_devices(ffmpeg: Option<&std::path::Path>) -> Vec<AudioDevice> {
    let ff = ffmpeg
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ffmpeg".into());
    if cfg!(windows) {
        run_text(
            &ff,
            &[
                "-hide_banner",
                "-list_devices",
                "true",
                "-f",
                "dshow",
                "-i",
                "dummy",
            ],
            true,
        )
        .map(|t| parse_dshow_audio(&t))
        .unwrap_or_default()
    } else if cfg!(target_os = "linux") {
        if let Some(t) = run_text("pactl", &["list", "short", "sources"], false) {
            let d = parse_pactl_sources(&t);
            if !d.is_empty() {
                return d;
            }
        }
        run_text(&ff, &["-hide_banner", "-sources", "pulse"], false)
            .map(|t| parse_ffmpeg_sources_pulse(&t))
            .unwrap_or_default()
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pactl() {
        let t = "50\talsa_output.pci-0000_00_1f.3.analog-stereo.monitor\tPipeWire\ts32le 2ch 48000Hz\tRUNNING\n\
                 51\talsa_input.pci-0000_00_1f.3.analog-stereo\tPipeWire\ts32le 2ch 48000Hz\tSUSPENDED\n\n";
        let d = parse_pactl_sources(t);
        assert_eq!(d.len(), 2);
        assert!(d[0].monitor && !d[1].monitor);
        assert_eq!(d[1].id, "alsa_input.pci-0000_00_1f.3.analog-stereo");
        assert_eq!(d[1].as_input().backend, AudioBackend::Pulse);
    }

    #[test]
    fn ffmpeg_pulse() {
        let t = "Auto-detected sources for pulse:\n  alsa_output.pci-0000_00_1f.3.analog-stereo.monitor [Monitor of Built-in Audio Analog Stereo] (none)\n* alsa_input.pci-0000_00_1f.3.analog-stereo [Built-in Audio Analog Stereo] (none)\n";
        let d = parse_ffmpeg_sources_pulse(t);
        assert_eq!(d.len(), 2);
        assert!(d[0].monitor);
        assert_eq!(d[1].id, "alsa_input.pci-0000_00_1f.3.analog-stereo");
        assert_eq!(d[1].description, "Built-in Audio Analog Stereo");
    }

    #[test]
    fn dshow_new_format() {
        let t = "[dshow @ 000001] DirectShow video devices (some may be both video and audio devices)\n\
[dshow @ 000001]  \"USB Video\" (video)\n\
[dshow @ 000001]     Alternative name \"@device_pnp_\\\\?\\usb#vid_345f\"\n\
[dshow @ 000001] DirectShow audio devices\n\
[dshow @ 000001]  \"Microphone (Realtek(R) Audio)\" (audio)\n\
[dshow @ 000001]     Alternative name \"@device_cm_{33D9A762}\\wave_{1}\"\n\
[dshow @ 000001]  \"Digital Audio (USB Video)\" (audio)\n\
dummy: Immediate exit requested\n";
        let d = parse_dshow_audio(t);
        let names: Vec<_> = d.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(
            names,
            ["Microphone (Realtek(R) Audio)", "Digital Audio (USB Video)"]
        );
        assert!(d.iter().all(|x| x.backend == AudioBackend::Dshow));
    }

    #[test]
    fn dshow_old_format() {
        let t = "[dshow @ 0] DirectShow video devices\n[dshow @ 0]  \"Cam\"\n[dshow @ 0]     Alternative name \"@x\"\n[dshow @ 0] DirectShow audio devices\n[dshow @ 0]  \"Mic\"\n[dshow @ 0]     Alternative name \"@y\"\n";
        let d = parse_dshow_audio(t);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].id, "Mic");
    }
}
