//! Pure video-state decisions for the GUI (host-tested): which device to auto-open, and what the Video chip and the
//! picture area say. Kept out of `gui.rs` so they can be unit-tested without a window.
use kvmit_video::{DeviceInfo, Mode, DEMO_PREFIX};

/// Chip colour. The GUI maps this onto its own `Health`: Good = green, Working = amber, Bad = red, Idle = grey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Good,
    Working,
    Bad,
    Idle,
}

/// What the capture is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoState {
    /// No capture device is present.
    NoDevice,
    /// Devices exist but none is open.
    NotOpened,
    /// Open, no frame has arrived yet.
    Waiting(Mode),
    /// Frames arrive but are blank (flat fill): the card has no signal or is still locking.
    NoSignal(Mode),
    /// A real picture.
    Live(Mode),
}

/// Derive the state from what the GUI knows. `blank` is `VideoFrame::is_blank` of the newest frame.
pub fn video_state(devices_present: bool, open: Option<Mode>, has_frame: bool, blank: bool) -> VideoState {
    match open {
        None if !devices_present => VideoState::NoDevice,
        None => VideoState::NotOpened,
        Some(m) if !has_frame => VideoState::Waiting(m),
        Some(m) if blank => VideoState::NoSignal(m),
        Some(m) => VideoState::Live(m),
    }
}

/// Chip colour and text. Only a real picture is green (UX principle 2: never show a healthy state that is not).
pub fn chip(s: VideoState) -> (Level, String) {
    match s {
        VideoState::NoDevice => (Level::Bad, "Video: no capture card found".into()),
        VideoState::NotOpened => (Level::Idle, "Video: not opened — click to open".into()),
        VideoState::Waiting(_) => (Level::Working, "Video: waiting for a frame…".into()),
        VideoState::NoSignal(_) => (Level::Working, "Video: no signal".into()),
        VideoState::Live(m) => (Level::Good, format!("Video: {}×{} @ {} fps", m.width, m.height, m.fps)),
    }
}

/// Text for the picture area when there is nothing (real) to show; `None` while a live picture fills it.
pub fn picture_message(s: VideoState) -> Option<&'static str> {
    match s {
        VideoState::NoDevice => Some("No capture device found. Plug in an HDMI capture card."),
        VideoState::NotOpened => Some("Open the capture card from the Video button above."),
        VideoState::Waiting(_) => Some("Waiting for the first frame from the capture card…"),
        VideoState::NoSignal(_) => Some("No signal: the capture card is sending a blank picture. Check the target is on and its HDMI cable is connected."),
        VideoState::Live(_) => None,
    }
}

/// Which device to open at startup. Only a device the user chose before is opened automatically: an exact match on the
/// stable key, else (config written before keys existed) on the old `/dev/videoN` path. The synthetic demo source
/// opens only because `KVMIT_DEMO_VIDEO` asked for it. A fresh install opens nothing: index 0 is often a webcam.
pub fn initial_video(devices: &[DeviceInfo], last_key: Option<&str>, last_path: Option<&str>) -> Option<usize> {
    // A stored key that matches nothing means the remembered card is not here: its old `/dev/videoN` may now be a webcam, so
    // the path is only a fallback for configs that never had a key.
    match last_key {
        Some(k) => devices.iter().position(|d| d.key == k),
        None => last_path.and_then(|p| devices.iter().position(|d| d.path == p)),
    }
    .or_else(|| devices.iter().position(|d| d.path.starts_with(DEMO_PREFIX)))
}

/// The device a `--video` argument names: its stable key (survives replugs and renumbering, so it is what a script
/// driving one of several cards should use), else its path. Fails with a hint rather than guessing, so a typo is caught
/// before anything is typed into a target.
pub fn select_video(devices: &[DeviceInfo], sel: &str) -> Result<usize, String> {
    let by_key: Vec<usize> = devices.iter().enumerate().filter(|(_, d)| d.key == sel).map(|(i, _)| i).collect();
    match by_key[..] {
        [i] => return Ok(i),
        // a key is only the card's name when the driver gives no bus location, so two identical cards can share one
        [_, _, ..] => return Err(format!("{sel:?} names {} capture devices: give one's path instead (`kvmit video list`)", by_key.len())),
        [] => {}
    }
    devices.iter().position(|d| d.path == sel).ok_or_else(|| {
        if devices.is_empty() {
            format!("no capture device matches {sel:?}: none are connected")
        } else {
            format!("no capture device matches {sel:?}; `kvmit video list` shows each one's path and key")
        }
    })
}

/// Which card a `kvmit run` uses for screen waits. Stricter than [`initial_video`]: a wrong card lets a wait pass on
/// another target's picture while keys go to this one, so anything short of a certain match is an error. The named
/// card (`--video`); else the remembered key, if exactly one card has it and it carries a location (a key that is just
/// the card's name, from a driver that reports no bus, would also match an identical card on another target); else,
/// only when nothing is remembered, the only real card. A config that has only an old path cannot prove which card it
/// meant, so it counts as nothing remembered. The demo source is used only when named.
pub fn run_video(devices: &[DeviceInfo], named: Option<&str>, last_key: Option<&str>) -> Result<usize, String> {
    if let Some(sel) = named {
        return select_video(devices, sel);
    }
    let pick = "pick one with --video (see `kvmit video list`)";
    if let Some(k) = last_key {
        return match devices.iter().enumerate().filter(|(_, d)| d.key == k).map(|(i, _)| i).collect::<Vec<_>>()[..] {
            // `device_key` trims the name, so compare with the trimmed one
            [i] if devices[i].key == devices[i].name.trim() => {
                Err(format!("the capture card used last ({k}) has no location to tell it from an identical card: {pick}"))
            }
            [i] => Ok(i),
            [] => Err(format!("the capture card used last ({k}) is not connected: {pick}")),
            _ => Err(format!("several capture cards are called {k:?}: {pick}")),
        };
    }
    match devices.iter().enumerate().filter(|(_, d)| !d.path.starts_with(DEMO_PREFIX)).map(|(i, _)| i).collect::<Vec<_>>()[..] {
        [i] => Ok(i),
        [] => Err("this script waits on the screen but no capture card is connected".into()),
        _ => Err(format!("several capture cards are connected: {pick}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m() -> Mode {
        Mode { width: 1920, height: 1080, fps: 60, mjpeg: true }
    }
    fn dev(path: &str, key: &str) -> DeviceInfo {
        DeviceInfo { path: path.into(), name: "x".into(), key: key.into() }
    }

    #[test]
    fn states_follow_what_is_known() {
        assert_eq!(video_state(false, None, false, false), VideoState::NoDevice);
        assert_eq!(video_state(true, None, false, false), VideoState::NotOpened);
        assert_eq!(video_state(true, Some(m()), false, true), VideoState::Waiting(m()), "no frame yet beats stale blank");
        assert_eq!(video_state(true, Some(m()), true, true), VideoState::NoSignal(m()));
        assert_eq!(video_state(true, Some(m()), true, false), VideoState::Live(m()));
    }

    #[test]
    fn only_a_real_picture_is_green() {
        for s in [VideoState::NoDevice, VideoState::NotOpened, VideoState::Waiting(m()), VideoState::NoSignal(m())] {
            assert_ne!(chip(s).0, Level::Good, "{s:?}");
        }
        assert_eq!(chip(VideoState::Live(m())), (Level::Good, "Video: 1920×1080 @ 60 fps".to_string()));
    }

    #[test]
    fn no_signal_is_amber_and_says_so_in_both_places() {
        assert_eq!(chip(VideoState::NoSignal(m())), (Level::Working, "Video: no signal".to_string()));
        assert!(picture_message(VideoState::NoSignal(m())).unwrap().starts_with("No signal"));
        assert!(picture_message(VideoState::Live(m())).is_none());
        assert!(picture_message(VideoState::NoDevice).is_some() && picture_message(VideoState::NotOpened).is_some());
    }

    #[test]
    fn a_fresh_install_opens_nothing() {
        let d = [dev("/dev/video0", "Webcam @ usb-1"), dev("/dev/video2", "Capture @ usb-2")];
        assert_eq!(initial_video(&d, None, None), None, "never index 0 by default");
        assert_eq!(initial_video(&[], Some("k"), Some("/dev/video0")), None);
        assert_eq!(initial_video(&d, Some("gone"), Some("/dev/video9")), None, "remembered device absent: open nothing");
        assert_eq!(initial_video(&d, Some("Gone @ usb-9"), Some("/dev/video0")), None, "a missing keyed card must not fall back to a path that is now the webcam");
    }

    #[test]
    fn the_stable_key_survives_a_renumbering() {
        let d = [dev("/dev/video0", "Webcam @ usb-1"), dev("/dev/video4", "Capture @ usb-2")];
        assert_eq!(initial_video(&d, Some("Capture @ usb-2"), Some("/dev/video2")), Some(1), "key wins over the old path");
    }

    #[test]
    fn an_old_config_with_only_a_path_still_works() {
        let d = [dev("/dev/video0", "Webcam @ usb-1"), dev("/dev/video2", "Capture @ usb-2")];
        assert_eq!(initial_video(&d, None, Some("/dev/video2")), Some(1));
    }

    #[test]
    fn the_demo_source_opens_because_it_was_asked_for() {
        let d = [dev("/dev/video0", "Webcam @ usb-1"), dev(&format!("{DEMO_PREFIX}pic.png"), "demo:pic.png")];
        assert_eq!(initial_video(&d, None, None), Some(1));
    }

    #[test]
    fn select_video_prefers_the_stable_key_then_the_path() {
        // two identical cards: the key (with bus info) tells them apart; a path that happens to equal another
        // device's key never wins over the key
        let d = [dev("/dev/video2", "USB3 Video @ usb-1"), dev("/dev/video4", "USB3 Video @ usb-2"), dev("USB3 Video @ usb-1", "other")];
        assert_eq!(select_video(&d, "USB3 Video @ usb-2"), Ok(1));
        assert_eq!(select_video(&d, "USB3 Video @ usb-1"), Ok(0));
        assert_eq!(select_video(&d, "/dev/video4"), Ok(1));
    }

    #[test]
    fn select_video_refuses_to_guess() {
        let d = [dev("/dev/video2", "USB3 Video @ usb-1")];
        assert!(select_video(&d, "/dev/video9").unwrap_err().contains("kvmit video list"));
        assert!(select_video(&d, "USB3 Video").is_err());
        assert!(select_video(&[], "x").unwrap_err().contains("none are connected"));
    }

    #[test]
    fn run_video_takes_only_a_certain_card() {
        let demo = format!("{DEMO_PREFIX}pic.png");
        let a = dev("/dev/video2", "USB3 Video @ usb-1");
        let b = dev("/dev/video4", "USB3 Video @ usb-2");
        let cam = dev("/dev/video0", "Webcam @ usb-3");
        // named wins
        assert_eq!(run_video(&[a.clone(), b.clone()], Some("USB3 Video @ usb-2"), Some("USB3 Video @ usb-1")), Ok(1));
        // remembered key, present once
        assert_eq!(run_video(&[cam.clone(), a.clone(), b.clone()], None, Some("USB3 Video @ usb-1")), Ok(1));
        // remembered key missing: never another card, never the demo
        assert!(run_video(&[b.clone(), dev(&demo, "demo")], None, Some("USB3 Video @ usb-1")).unwrap_err().contains("not connected"));
        // a bare-name key (no bus info) cannot tell this card from an identical one on another target
        let bare = DeviceInfo { path: "/dev/video2".into(), name: "USB3 Video".into(), key: "USB3 Video".into() };
        assert!(run_video(&[bare], None, Some("USB3 Video")).unwrap_err().contains("no location"));
        let padded = DeviceInfo { path: "/dev/video2".into(), name: " Cam ".into(), key: kvmit_video::device_key(" Cam ", "") };
        assert!(run_video(&[padded], None, Some("Cam")).unwrap_err().contains("no location"));
        // remembered key shared by two cards (no bus info)
        let (x, y) = (dev("/dev/video2", "USB3 Video"), dev("/dev/video4", "USB3 Video"));
        assert!(run_video(&[x, y], None, Some("USB3 Video")).unwrap_err().contains("several"));
        // nothing remembered: only a lone real card is certain; the demo does not count and is not chosen
        assert_eq!(run_video(&[dev(&demo, "demo"), a.clone()], None, None), Ok(1));
        assert!(run_video(&[cam, a], None, None).unwrap_err().contains("several"));
        assert!(run_video(&[dev(&demo, "demo")], None, None).unwrap_err().contains("no capture card"));
        assert_eq!(run_video(&[dev(&demo, "demo")], Some(&demo), None), Ok(0));
    }

    #[test]
    fn select_video_refuses_a_key_two_cards_share() {
        // no bus info: the key is the bare card name, the same for both
        let d = [dev("/dev/video2", "USB3 Video"), dev("/dev/video4", "USB3 Video")];
        assert!(select_video(&d, "USB3 Video").unwrap_err().contains("2 capture devices"));
        assert_eq!(select_video(&d, "/dev/video4"), Ok(1));
    }
}
