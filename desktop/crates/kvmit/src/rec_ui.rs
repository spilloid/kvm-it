//! Recording UI state (docs/ux.md, #30). The encoder lives in `kvmit-record`; this holds what the Record popup and the
//! top-bar chip need, with the decisions (labels, reasons, folder, settings) as small pure functions.
//! Never records or logs keys or typed text: only the picture (and, when picked, an audio device).
use kvmit_record::{poll_record, AudioDevice, Finished, Format, PollHandle, RecordError, Settings};
use kvmit_video::SharedFrame;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Why Record cannot start right now (shown as the disabled hover text), or `None` when it can. A blank picture does not
/// block it: that is a heuristic (a black boot screen is valid video), so the popup only says so.
pub fn record_block_reason(has_picture: bool, finishing: bool) -> Option<&'static str> {
    if finishing {
        Some("The last recording is still being finished")
    } else if !has_picture {
        Some("Open the capture card first: there is no picture to record")
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
    /// Audio inputs are looked up on a worker (the lookup runs `pactl`/`ffmpeg`, which can hang): `Some` while one is out.
    audio_job: Option<Receiver<Vec<AudioDevice>>>,
    audio_listed: bool,
    active: Option<Active>,
    /// A stop in progress: finishing the file can take seconds (a GIF is encoded at the end), so it never runs on the GUI thread.
    finishing: Option<Receiver<Result<Finished, RecordError>>>,
    finish_cancel: Option<kvmit_record::Canceller>,
    pub last_saved: Option<PathBuf>,
}

impl Default for RecordUi {
    fn default() -> Self {
        RecordUi { format: Format::Gif, audio_sel: None, audio: Vec::new(), audio_job: None, audio_listed: false, active: None, finishing: None, finish_cancel: None, last_saved: None }
    }
}

impl RecordUi {
    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    /// The running time, while the clip is still being recorded (not once it is being finalised).
    pub fn elapsed(&self) -> Option<Duration> {
        self.active.as_ref().filter(|a| !a.handle.is_finalising()).map(|a| a.since.elapsed())
    }

    pub fn start(&mut self, source: impl FnMut() -> Option<SharedFrame> + Send + 'static) -> Result<(), String> {
        let dir = output_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let audio = self.audio_sel.and_then(|i| self.audio.get(i));
        let handle = poll_record(settings_for(self.format, audio), &dir, source).map_err(|e| e.to_string())?;
        self.active = Some(Active { handle, since: Instant::now() });
        Ok(())
    }

    /// Ask the recording to stop. The file is finished on a worker; collect the outcome with `poll_ended`.
    pub fn begin_stop(&mut self) {
        if let Some(a) = self.active.take() {
            self.finish_cancel = Some(a.handle.canceller());
            let (tx, rx) = channel();
            std::thread::spawn(move || {
                let _ = tx.send(a.handle.stop());
            });
            self.finishing = Some(rx);
        }
    }

    /// The clip is over and its file is being finished (after a stop, the cap, or the picture going away).
    pub fn is_finishing(&self) -> bool {
        self.finishing.is_some() || self.active.as_ref().is_some_and(|a| a.handle.is_finalising())
    }

    /// The outcome of a recording that has finished: one stopped by `begin_stop`, or one that ended by itself (duration
    /// cap, encoder died, the picture stayed gone). Never blocks.
    pub fn poll_ended(&mut self) -> Option<Result<Finished, RecordError>> {
        if self.active.as_ref().is_some_and(|a| !a.handle.is_running()) {
            self.begin_stop();
        }
        let r = match self.finishing.as_ref()?.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err(RecordError::Io("the recording thread ended unexpectedly".into())),
        };
        self.finishing = None;
        self.finish_cancel = None;
        Some(self.keep(r))
    }

    /// Quitting: stop and wait (bounded) so the file is not left cut off. If finishing outlasts `wait`, the encoder is killed and
    /// the files removed, so quitting never leaves an ffmpeg or a half-written file behind.
    pub fn finish_for_exit(&mut self, wait: Duration) -> Option<Result<Finished, RecordError>> {
        self.begin_stop();
        let rx = self.finishing.take()?;
        let r = match rx.recv_timeout(wait) {
            Ok(r) => r,
            Err(_) => {
                if let Some(c) = &self.finish_cancel {
                    c.cancel();
                }
                rx.recv_timeout(Duration::from_secs(5)).ok()?
            }
        };
        self.finish_cancel = None;
        Some(self.keep(r))
    }

    fn keep(&mut self, r: Result<Finished, RecordError>) -> Result<Finished, RecordError> {
        if let Ok(f) = &r {
            self.last_saved = Some(f.path.clone());
        }
        r
    }

    fn look_for_audio(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _ = tx.send(kvmit_record::list_audio_devices(None));
        });
        self.audio_job = Some(rx);
    }

    /// The popup body. Returns true when Start was clicked. `blank`: the picture looks like a flat fill.
    pub fn popup_body(&mut self, ui: &mut egui::Ui, block: Option<&'static str>, blank: bool) -> bool {
        ui.strong("Record the target's screen");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.format, Format::Gif, "GIF (silent, short)");
            ui.selectable_value(&mut self.format, Format::WebM, "WebM (video, optional sound)");
        });
        if self.format == Format::WebM {
            if let Some(rx) = &self.audio_job {
                match rx.try_recv() {
                    Ok(list) => {
                        self.audio = list;
                        self.audio_sel = None; // indexes into the old list mean nothing now: sound stays off until chosen
                        self.audio_job = None;
                    }
                    Err(TryRecvError::Empty) => ui.ctx().request_repaint_after(Duration::from_millis(200)),
                    Err(TryRecvError::Disconnected) => self.audio_job = None,
                }
            } else if !self.audio_listed {
                self.audio_listed = true;
                self.look_for_audio();
            }
            ui.horizontal(|ui| {
                ui.label("Sound:");
                let text = self.audio_sel.and_then(|i| self.audio.get(i)).map_or("none (off)".to_string(), |d| d.description.clone());
                ui.add_enabled_ui(self.audio_job.is_none(), |ui| egui::ComboBox::from_id_salt("rec_audio").selected_text(text).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.audio_sel, None, "none (off)");
                    for (i, d) in self.audio.iter().enumerate() {
                        let label = if d.monitor { format!("{} (what this computer plays)", d.description) } else { d.description.clone() };
                        ui.selectable_value(&mut self.audio_sel, Some(i), label);
                    }
                }));
                if self.audio_job.is_some() {
                    ui.small("looking for audio inputs…");
                } else if ui.small_button("Rescan").clicked() {
                    self.audio_sel = None;
                    self.look_for_audio();
                }
            });
            ui.small("Pick the capture card's audio input to record the target's sound. Off unless you choose one.");
        } else {
            ui.small("GIF: about 12 frames per second, at most 960 px wide, stops after 30 s. Use WebM for longer clips or sound.");
        }
        ui.colored_label(crate::theme::palette(ui.visuals().dark_mode).warn_text, "⚠ A recording cannot be redacted: everything on the target's screen (passwords typed into it, private windows) is in the file.");
        if blank {
            ui.small("The picture looks blank (no signal, or a black screen). You can still record it.");
        }
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
    fn only_a_missing_picture_or_a_recording_still_finishing_blocks_record() {
        assert!(record_block_reason(false, false).is_some());
        assert!(record_block_reason(true, true).is_some());
        assert!(record_block_reason(true, false).is_none(), "a blank picture can still be recorded");
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
