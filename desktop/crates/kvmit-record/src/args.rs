//! Pure ffmpeg argument building. No process is spawned here.
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Gif,
    WebM,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Format::Gif => "gif",
            Format::WebM => "webm",
        }
    }
}

/// How ffmpeg should open the audio device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackend {
    Pulse,
    Alsa,
    Dshow,
    /// macOS: ffmpeg's AVFoundation input, by device name.
    AvFoundation,
}

/// A named audio input to mux as Opus (WebM only). Audio is OFF unless the caller sets one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInput {
    pub backend: AudioBackend,
    pub device: String,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub format: Format,
    /// Input and output frame rate.
    pub fps: u32,
    /// Output is scaled down (never up) to at most this many pixels wide.
    pub max_width: u32,
    /// Hard stop: frames beyond this many seconds are not sent. GIF holds the whole clip in memory for
    /// palettegen, so its default cap is short.
    pub max_duration: Option<Duration>,
    /// WebM only; ignored for GIF.
    pub audio: Option<AudioInput>,
    /// Explicit ffmpeg binary; `None` means `ffmpeg` on PATH.
    pub ffmpeg_path: Option<std::path::PathBuf>,
}

impl Settings {
    pub fn gif() -> Self {
        Settings {
            format: Format::Gif,
            fps: 12,
            max_width: 960,
            max_duration: Some(Duration::from_secs(30)),
            audio: None,
            ffmpeg_path: None,
        }
    }

    pub fn webm() -> Self {
        Settings {
            format: Format::WebM,
            fps: 30,
            max_width: 1920,
            max_duration: None,
            audio: None,
            ffmpeg_path: None,
        }
    }

    /// Frames allowed by the duration cap, if any.
    pub fn max_frames(&self) -> Option<u64> {
        self.max_duration
            .map(|d| (d.as_secs_f64() * self.fps.max(1) as f64).round() as u64)
    }

    pub fn uses_audio(&self) -> bool {
        self.format == Format::WebM && self.audio.is_some()
    }
}

/// The GIF filter graph: one pass, palette computed from the whole clip. (`\\,` is the filtergraph escape for a
/// comma inside `min()`; no shell is involved.)
pub fn gif_filter(max_width: u32) -> String {
    format!(
        "scale='min({max_width}\\,iw)':-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle"
    )
}

/// VP9 needs even dimensions for yuv420p.
pub fn webm_filter(max_width: u32) -> String {
    format!("scale='min({max_width}\\,iw)':-2:flags=bicubic,scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p")
}

fn audio_input_args(a: &AudioInput) -> Vec<String> {
    let (fmt, dev) = match a.backend {
        AudioBackend::Pulse => ("pulse", a.device.clone()),
        AudioBackend::Alsa => ("alsa", a.device.clone()),
        AudioBackend::Dshow => ("dshow", format!("audio={}", a.device)),
        AudioBackend::AvFoundation => ("avfoundation", format!(":{}", a.device)), // ":name" = audio only, no video
    };
    ["-thread_queue_size", "512", "-f", fmt, "-i", &dev]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Full ffmpeg argument list (without the program name) for frames of `width` x `height` piped on stdin,
/// written to `out`. `-n` makes ffmpeg refuse to overwrite as a second line of defence.
pub fn build_args(s: &Settings, width: u32, height: u32, out: &Path) -> Vec<OsString> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |xs: &[&str]| a.extend(xs.iter().map(|x| x.to_string()));
    push(&["-hide_banner", "-loglevel", "error", "-nostdin", "-n"]);
    let size = format!("{width}x{height}");
    let fps = s.fps.max(1).to_string();
    push(&[
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgba",
        "-s",
        &size,
        "-framerate",
        &fps,
        "-i",
        "pipe:0",
    ]);
    match s.format {
        Format::Gif => {
            push(&[
                "-an",
                "-filter_complex",
                &gif_filter(s.max_width),
                "-loop",
                "0",
                "-f",
                "gif",
            ]);
        }
        Format::WebM => {
            let mut audio = Vec::new();
            if let Some(au) = &s.audio {
                audio = audio_input_args(au);
            }
            let have_audio = !audio.is_empty();
            a.extend(audio);
            let mut push = |xs: &[&str]| a.extend(xs.iter().map(|x| x.to_string()));
            push(&["-vf", &webm_filter(s.max_width)]);
            push(&[
                "-c:v",
                "libvpx-vp9",
                "-b:v",
                "0",
                "-crf",
                "34",
                "-deadline",
                "realtime",
                "-cpu-used",
                "8",
                "-row-mt",
                "1",
            ]);
            if have_audio {
                push(&[
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                    "-c:a",
                    "libopus",
                    "-b:a",
                    "96k",
                    "-shortest",
                ]);
            } else {
                push(&["-an"]);
            }
            push(&["-f", "webm"]);
        }
    }
    let mut v: Vec<OsString> = a.into_iter().map(OsString::from).collect();
    v.push(out.as_os_str().to_owned());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strs(v: &[OsString]) -> Vec<String> {
        v.iter().map(|s| s.to_string_lossy().into_owned()).collect()
    }

    fn has_pair(v: &[String], a: &str, b: &str) -> bool {
        v.windows(2).any(|w| w[0] == a && w[1] == b)
    }

    #[test]
    fn gif_args() {
        let v = strs(&build_args(
            &Settings::gif(),
            1920,
            1080,
            Path::new("/o/x.gif"),
        ));
        assert!(has_pair(&v, "-s", "1920x1080"));
        assert!(has_pair(&v, "-framerate", "12"));
        assert!(has_pair(&v, "-i", "pipe:0"));
        assert!(v.contains(&"-n".to_string()));
        let fg = v.iter().position(|x| x == "-filter_complex").unwrap();
        assert!(v[fg + 1].contains("palettegen") && v[fg + 1].contains("paletteuse"));
        assert!(v[fg + 1].contains("min(960\\,iw)"), "{}", v[fg + 1]);
        assert_eq!(v.last().unwrap(), "/o/x.gif");
        assert!(!v.iter().any(|x| x == "libopus"));
    }

    #[test]
    fn webm_without_audio_is_silent_by_default() {
        let v = strs(&build_args(
            &Settings::webm(),
            1280,
            720,
            Path::new("o.webm"),
        ));
        assert!(has_pair(&v, "-c:v", "libvpx-vp9"));
        assert!(v.contains(&"-an".to_string()));
        assert!(!v.iter().any(|x| x == "libopus" || x == "pulse"));
        assert_eq!(Settings::webm().audio, None);
    }

    #[test]
    fn webm_audio_pulse_and_dshow() {
        let mut s = Settings::webm();
        s.audio = Some(AudioInput {
            backend: AudioBackend::Pulse,
            device: "alsa_input.x".into(),
        });
        let v = strs(&build_args(&s, 640, 480, Path::new("o.webm")));
        assert!(has_pair(&v, "-f", "pulse") && has_pair(&v, "-i", "alsa_input.x"));
        assert!(has_pair(&v, "-c:a", "libopus"));
        assert!(has_pair(&v, "-map", "1:a:0"));
        s.audio = Some(AudioInput {
            backend: AudioBackend::Dshow,
            device: "Microphone (USB)".into(),
        });
        let v = strs(&build_args(&s, 640, 480, Path::new("o.webm")));
        assert!(has_pair(&v, "-i", "audio=Microphone (USB)"));
    }

    #[test]
    fn audio_ignored_for_gif() {
        let mut s = Settings::gif();
        s.audio = Some(AudioInput {
            backend: AudioBackend::Pulse,
            device: "d".into(),
        });
        assert!(!s.uses_audio());
        let v = strs(&build_args(&s, 10, 10, Path::new("o.gif")));
        assert!(!v.contains(&"pulse".to_string()));
    }

    #[test]
    fn duration_cap_frames() {
        assert_eq!(Settings::gif().max_frames(), Some(360));
        assert_eq!(Settings::webm().max_frames(), None);
    }
}
