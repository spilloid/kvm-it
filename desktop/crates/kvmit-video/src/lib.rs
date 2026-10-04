//! Video capture. Frames arrive as the card delivers them (MJPEG preferred, YUYV fallback) and are decoded
//! only for display; there is no transcoding. The Linux backend is V4L2; other platforms report "unsupported"
//! until their backend lands (docs/roadmap.md).
pub mod convert;
mod demo;
mod device;

pub use demo::PREFIX as DEMO_PREFIX;
pub use device::{list_devices, Capture, CaptureError, DeviceInfo, Mode};

use std::sync::Arc;

/// One decoded frame, RGBA8, row-major.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
    /// Monotonic per-capture counter; lets the UI skip re-uploading an unchanged frame.
    pub seq: u64,
}

impl VideoFrame {
    pub fn to_gray(&self) -> kvmit_script::Frame {
        convert::rgba_to_gray_frame(self.width, self.height, &self.rgba)
    }

    /// True when the frame is (nearly) one flat colour. Cheap USB capture cards emit a uniform fill while they
    /// have no signal or are still locking onto one (MacroSilicon 345f:2109: every pixel at luma 22), so a
    /// snapshot should not settle for such a frame. Samples a grid rather than every pixel.
    pub fn is_blank(&self) -> bool {
        let (w, h) = (self.width, self.height);
        if w == 0 || h == 0 || self.rgba.len() < w * h * 4 {
            return true;
        }
        let (mut lo, mut hi) = (u8::MAX, u8::MIN);
        for y in (0..h).step_by((h / 64).max(1)) {
            for x in (0..w).step_by((w / 64).max(1)) {
                let i = (y * w + x) * 4;
                let (r, g, b) = (self.rgba[i] as u32, self.rgba[i + 1] as u32, self.rgba[i + 2] as u32);
                let l = ((r * 77 + g * 150 + b * 29) >> 8) as u8;
                lo = lo.min(l);
                hi = hi.max(l);
            }
        }
        hi.saturating_sub(lo) < 8
    }
}


pub type SharedFrame = Arc<VideoFrame>;

#[cfg(test)]
mod blank_tests {
    use super::VideoFrame;

    fn frame(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> VideoFrame {
        let mut rgba = vec![255; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let v = f(x, y);
                rgba[(y * w + x) * 4..][..3].copy_from_slice(&[v, v, v]);
            }
        }
        VideoFrame { width: w, height: h, rgba, seq: 0 }
    }

    #[test]
    fn flat_no_signal_fill_is_blank() {
        assert!(frame(1920, 1080, |_, _| 22).is_blank());
        assert!(frame(640, 480, |x, _| 16 + (x % 3) as u8).is_blank()); // MJPEG noise on a flat field
    }

    #[test]
    fn a_picture_is_not_blank() {
        assert!(!frame(1920, 1080, |x, y| if x > 900 && y > 500 { 235 } else { 16 }).is_blank());
        assert!(!frame(1280, 720, |x, _| (x % 256) as u8).is_blank());
    }

    #[test]
    fn empty_or_short_frames_are_blank() {
        assert!(VideoFrame { width: 0, height: 0, rgba: vec![], seq: 0 }.is_blank());
        assert!(VideoFrame { width: 10, height: 10, rgba: vec![0; 12], seq: 0 }.is_blank());
    }
}
