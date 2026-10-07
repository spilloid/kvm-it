//! Recording UI state (docs/ux.md, #30). The encoder lives in `kvmit-record`; this holds what the Record popup and the
//! top-bar chip need, with the decisions (labels, reasons, folder, settings) as small pure functions.
//! Never records or logs keys or typed text: only the picture (and, when picked, an audio device).
use kvmit_record::{poll_record, AudioDevice, Finished, Format, PollHandle, RecordError, Settings};
use kvmit_video::SharedFrame;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Why Record cannot start right now (shown as the disabled hover text), or `None` when it can.
pub fn record_block_reason(has_picture: bool, blank: bool) -> Option<&'static str> {
    if !has_picture {
        Some("Open the capture card first: there is no picture to record")
    } else if blank {
        Some("The picture is blank (no signal): nothing worth recording yet")
    } else {
        None
    }
}

/// `mm:ss`, or `h:mm:ss` from an hour on.
pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// Recordings go under the user's Videos folder (falling back to the home dir, then the working dir).
pub fn output_dir() -> PathBuf {
    dirs::video_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("kvm-it")
}

/// Settings for a choice in the popup. GIF never carries audio; WebM only when a device was picked.
pub fn settings_for(format: Format, audio: Option<&AudioDevice>) -> Settings {
    match format {
        Format::Gif => Settings::gif(),
        Format::WebM => Settings { audio: audio.map(AudioDevice::as_input), ..Settings::webm() },
    }
}

/// One line for the notice after a recording ended.
pub fn describe(result: &Result<Finished, RecordError>) -> String {
    match result {
        Ok(f) => {
            let lost = if f.dropped > 0 { format!(" ({} frames dropped: the encoder could not keep up)", f.dropped) } else { String::new() };
            format!("Saved {} ({}){lost}", f.path.display(), format_elapsed(f.duration))
        }
        Err(e) => format!("Recording failed: {e}"),
    }
}

struct Active {
    handle: PollHandle,
    since: Instant,
}

pub struct RecordUi {
    pub format: Format,
    /// Index into `audio` for WebM; `None` = no sound (the default).
    pub audio_sel: Option<usize>,
    audio: Vec<AudioDevice>,
    audio_listed: bool,
    active: Option<Active>,
    pub last_saved: Option<PathBuf>,
}

impl Default for RecordUi {
    fn default() -> Self {
        RecordUi { format: Format::Gif, audio_sel: None, audio: Vec::new(), audio_listed: false, active: None, last_saved: None }
    }
}

impl RecordUi {
    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub fn elapsed(&self) -> Option<Duration> {
        self.active.as_ref().map(|a| a.since.elapsed())
    }

    pub fn start(&mut self, source: impl FnMut() -> Option<SharedFrame> + Send + 'static) -> Result<(), String> {
        let dir = output_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let audio = self.audio_sel.and_then(|i| self.audio.get(i));
        let handle = poll_record(settings_for(self.format, audio), &dir, source).map_err(|e| e.to_string())?;
        self.active = Some(Active { handle, since: Instant::now() });
        Ok(())
    }

    /// Stop now (a click, or quitting) and return the outcome.
    pub fn stop(&mut self) -> Option<Result<Finished, RecordError>> {
        let a = self.active.take()?;
        Some(self.keep(a.handle.stop()))
    }

    /// If the recording ended by itself (duration cap, encoder died), collect its outcome.
    pub fn poll_ended(&mut self) -> Option<Result<Finished, RecordError>> {
        if self.active.as_ref().is_some_and(|a| a.handle.is_running()) {
            return None;
        }
        self.stop()
    }

    fn keep(&mut self, r: Result<Finished, RecordError>) -> Result<Finished, RecordError> {
        if let Ok(f) = &r {
            self.last_saved = Some(f.path.clone());
        }
        r
    }

    /// The popup body. Returns true when Start was clicked.
    pub fn popup_body(&mut self, ui: &mut egui::Ui, block: Option<&'static str>) -> bool {
        ui.strong("Record the target's screen");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.format, Format::Gif, "GIF (silent, short)");
            ui.selectable_value(&mut self.format, Format::WebM, "WebM (video, optional sound)");
        });
        if self.format == Format::WebM {
            if !self.audio_listed {
                self.audio = kvmit_record::list_audio_devices(None);
                self.audio_listed = true;
            }
            ui.horizontal(|ui| {
                ui.label("Sound:");
                let text = self.audio_sel.and_then(|i| self.audio.get(i)).map_or("none (off)".to_string(), |d| d.description.clone());
                egui::ComboBox::from_id_salt("rec_audio").selected_text(text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.audio_sel, None, "none (off)");
                    for (i, d) in self.audio.iter().enumerate() {
                        let label = if d.monitor { format!("{} (what this computer plays)", d.description) } else { d.description.clone() };
                        ui.selectable_value(&mut self.audio_sel, Some(i), label);
                    }
                });
                if ui.small_button("Rescan").clicked() {
                    self.audio = kvmit_record::list_audio_devices(None);
                    self.audio_sel = None;
                }
            });
            ui.small("Pick the capture card's audio input to record the target's sound. Off unless you choose one.");
        } else {
            ui.small("GIF: about 12 frames per second, at most 960 px wide, stops after 30 s. Use WebM for longer clips or sound.");
        }
        ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).warn_text, "⚠ A recording cannot be redacted: everything on the target's screen (passwords typed into it, private windows) is in the file.");
        ui.small(format!("Saved to {}", output_dir().display()));
        if let Some(p) = &self.last_saved {
            ui.small(format!("Last: {}", p.display()));
        }
        let mut clicked = false;
        ui.add_enabled_ui(block.is_none(), |ui| {
            let r = ui.button("● Start recording");
            let r = match block {
                Some(why) => r.on_disabled_hover_text(why),
                None => r,
            };
            clicked = r.clicked();
        });
        clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmit_record::AudioBackend;

    #[test]
    fn elapsed_formats() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "00:05");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "02:05");
        assert_eq!(format_elapsed(Duration::from_secs(3725)), "1:02:05");
    }

    #[test]
    fn reasons_cover_no_picture_and_no_signal() {
        assert!(record_block_reason(false, false).is_some());
        assert!(record_block_reason(true, true).is_some());
        assert!(record_block_reason(true, false).is_none());
    }

    #[test]
    fn audio_is_off_by_default_and_never_on_gif() {
        let d = AudioDevice { id: "mic".into(), description: "Mic".into(), monitor: false, backend: AudioBackend::Pulse };
        assert!(settings_for(Format::WebM, None).audio.is_none());
        assert!(settings_for(Format::WebM, Some(&d)).audio.is_some());
        assert!(settings_for(Format::Gif, Some(&d)).audio.is_none());
    }

    #[test]
    fn describe_reports_failures_and_drops() {
        let ok = Ok(Finished { path: "/v/a.gif".into(), frames: 10, dropped: 3, duration: Duration::from_secs(4) });
        let s = describe(&ok);
        assert!(s.contains("/v/a.gif") && s.contains("00:04") && s.contains("3 frames dropped"), "{s}");
        assert!(describe(&Err(RecordError::NoFrames)).starts_with("Recording failed"));
    }

    #[test]
    fn output_folder_is_kvm_it() {
        assert!(output_dir().ends_with("kvm-it"));
    }
}
