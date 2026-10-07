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
    last_key
        .and_then(|k| devices.iter().position(|d| d.key == k))
        .or_else(|| last_path.and_then(|p| devices.iter().position(|d| d.path == p)))
        .or_else(|| devices.iter().position(|d| d.path.starts_with(DEMO_PREFIX)))
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
}
