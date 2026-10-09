//! Pure capture-negotiation and stream-health logic shared by the backends, so it is unit-tested without a card:
//! what the driver actually applied, how many stream errors mean "the card is gone", and bounded thread shutdown.
#![allow(dead_code)] // each backend uses a subset
use crate::Mode;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// Index of the best `(width, height, max fps)` by the shared preference (`mode_score`; these are decoded by the OS, so
/// never counted as MJPEG). `None` for an empty list or one without any real size.
pub(crate) fn best_mode(modes: &[(u32, u32, u32)]) -> Option<usize> {
    modes
        .iter()
        .enumerate()
        .filter(|(_, (w, h, _))| *w > 0 && *h > 0)
        .min_by_key(|(_, (w, h, fps))| crate::device::mode_score(*w, *h, *fps, false))
        .map(|(i, _)| i)
}

/// After this much error weight in a row the card is treated as gone (about a second of failed reads).
pub(crate) const MAX_CONSECUTIVE_ERRORS: u32 = 20;

/// Counts consecutive stream errors; any good frame resets it.
#[derive(Debug, Default)]
pub(crate) struct ErrorTracker {
    run: u32,
}
impl ErrorTracker {
    /// Record an error of the given weight; true once the run reaches the limit (the card has given up).
    pub(crate) fn error(&mut self, weight: u32) -> bool {
        self.run = self.run.saturating_add(weight.max(1));
        self.run >= MAX_CONSECUTIVE_ERRORS
    }
    pub(crate) fn ok(&mut self) {
        self.run = 0;
    }
}

/// Frames per second from a V4L2 frame interval (seconds per frame as num/den), rounded: 1001/60000 is 60.
pub(crate) fn fps_from_interval(num: u32, den: u32) -> Option<u32> {
    if num == 0 || den == 0 {
        return None;
    }
    Some(((u64::from(den) + u64::from(num) / 2) / u64::from(num)) as u32).filter(|f| *f > 0)
}

/// `Some(true)` for MJPEG, `Some(false)` for YUYV, `None` for anything we cannot decode.
pub(crate) fn fourcc_is_mjpeg(f: &[u8; 4]) -> Option<bool> {
    match f {
        b"MJPG" | b"JPEG" => Some(true),
        b"YUYV" => Some(false),
        _ => None,
    }
}

/// How YUYV rows are laid out in the buffer the driver filled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Layout {
    /// Bytes per row; at least `width * 2`.
    pub stride: usize,
}

/// What the driver reported after `set_format` / `set_params`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Applied {
    pub fourcc: [u8; 4],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    /// The applied frame interval (num, den) if the driver told us.
    pub interval: Option<(u32, u32)>,
}

/// Build the mode to report from what was *applied*, never from what was requested. A driver may substitute the
/// other supported pixel format (we then decode that one and say so); anything undecodable is an error.
pub(crate) fn applied_mode(requested: Mode, a: Applied) -> Result<(Mode, Layout), String> {
    let Some(mjpeg) = fourcc_is_mjpeg(&a.fourcc) else {
        return Err(format!("the driver chose pixel format {} which kvm-it cannot decode", String::from_utf8_lossy(&a.fourcc)));
    };
    if a.width == 0 || a.height == 0 {
        return Err("the driver reported an empty frame size".into());
    }
    let fps = a.interval.and_then(|(n, d)| fps_from_interval(n, d)).unwrap_or(requested.fps);
    let min_stride = a.width as usize * 2;
    let stride = if (a.stride as usize) < min_stride { min_stride } else { a.stride as usize };
    Ok((Mode { width: a.width, height: a.height, fps, mjpeg }, Layout { stride }))
}

/// Tells a dropper that a thread has finished (also on panic or early return).
pub(crate) struct DoneOnDrop(pub Sender<()>);
impl Drop for DoneOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

/// Join `t` if it finishes within `wait`; otherwise detach it so a stalled card can never hang the caller (the GUI
/// thread). True when joined.
pub(crate) fn join_bounded(t: std::thread::JoinHandle<()>, done: &Receiver<()>, wait: Duration) -> bool {
    match done.recv_timeout(wait) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => {
            let _ = t.join();
            true
        }
        Err(RecvTimeoutError::Timeout) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Instant;

    fn req() -> Mode {
        Mode { width: 1920, height: 1080, fps: 60, mjpeg: true }
    }
    fn applied(f: &[u8; 4]) -> Applied {
        Applied { fourcc: *f, width: 1920, height: 1080, stride: 0, interval: Some((1, 30)) }
    }

    #[test]
    fn errors_trip_only_when_consecutive() {
        let mut t = ErrorTracker::default();
        for _ in 0..MAX_CONSECUTIVE_ERRORS - 1 {
            assert!(!t.error(1));
        }
        t.ok();
        for _ in 0..MAX_CONSECUTIVE_ERRORS - 1 {
            assert!(!t.error(1), "a good frame reset the run");
        }
        assert!(t.error(1));
    }

    #[test]
    fn a_timeout_weighs_more_than_a_plain_error() {
        let mut t = ErrorTracker::default();
        let mut n = 0;
        while !t.error(2) {
            n += 1;
        }
        assert_eq!(n + 1, MAX_CONSECUTIVE_ERRORS / 2);
    }

    #[test]
    fn fps_is_rounded_from_the_interval() {
        assert_eq!(fps_from_interval(1, 60), Some(60));
        assert_eq!(fps_from_interval(1001, 60000), Some(60));
        assert_eq!(fps_from_interval(1001, 30000), Some(30));
        assert_eq!(fps_from_interval(1, 5), Some(5));
        assert_eq!(fps_from_interval(0, 30), None);
        assert_eq!(fps_from_interval(30, 0), None);
        assert_eq!(fps_from_interval(10, 1), None, "slower than 1 fps rounds to nothing useful");
    }

    #[test]
    fn the_reported_mode_is_the_applied_one() {
        let (m, l) = applied_mode(req(), applied(b"MJPG")).unwrap();
        assert_eq!((m.width, m.height, m.fps, m.mjpeg), (1920, 1080, 30, true), "the card ran at 30, not the advertised 60");
        assert_eq!(l.stride, 3840, "stride 0 means tightly packed");
    }

    #[test]
    fn a_substituted_pixel_format_is_reported_truthfully() {
        let (m, _) = applied_mode(req(), applied(b"YUYV")).unwrap();
        assert!(!m.mjpeg, "driver gave YUYV although MJPEG was requested: decode YUYV and say so");
    }

    #[test]
    fn an_undecodable_format_or_empty_size_is_an_error() {
        assert!(applied_mode(req(), applied(b"NV12")).unwrap_err().contains("NV12"));
        assert!(applied_mode(req(), Applied { width: 0, ..applied(b"MJPG") }).is_err());
    }

    #[test]
    fn size_stride_and_unknown_interval_come_from_the_driver() {
        let a = Applied { width: 1280, height: 720, stride: 2560 + 64, interval: None, ..applied(b"YUYV") };
        let (m, l) = applied_mode(req(), a).unwrap();
        assert_eq!((m.width, m.height), (1280, 720));
        assert_eq!(m.fps, 60, "no interval reported: keep the requested rate");
        assert_eq!(l.stride, 2624, "padded rows are honoured");
        let (_, l) = applied_mode(req(), Applied { stride: 10, ..a }).unwrap();
        assert_eq!(l.stride, 2560, "an impossible stride falls back to packed");
    }

    #[test]
    fn dropping_a_stalled_thread_detaches_within_the_bound() {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let s2 = stop.clone();
        let t = std::thread::spawn(move || {
            let _d = DoneOnDrop(tx);
            while !s2.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let start = Instant::now();
        assert!(!join_bounded(t, &rx, Duration::from_millis(60)), "stalled: detached, not joined");
        assert!(start.elapsed() < Duration::from_secs(2));
        stop.store(true, Ordering::Relaxed);
    }

    #[test]
    fn a_finished_thread_is_joined() {
        let (tx, rx) = mpsc::channel();
        let t = std::thread::spawn(move || {
            let _d = DoneOnDrop(tx);
        });
        assert!(join_bounded(t, &rx, Duration::from_secs(5)));
    }

    #[test]
    fn best_mode_prefers_1080p_at_a_real_rate_and_skips_empty_sizes() {
        let m = [(640, 480, 30), (1920, 1080, 5), (1920, 1080, 60), (0, 0, 60), (3840, 2160, 30)];
        assert_eq!(best_mode(&m), Some(2));
        assert_eq!(best_mode(&[(0, 0, 30)]), None);
        assert_eq!(best_mode(&[]), None);
    }
}
