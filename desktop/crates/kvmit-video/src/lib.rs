//! Video capture. Frames arrive as the card delivers them (MJPEG preferred, YUYV fallback) and are decoded
//! only for display; there is no transcoding. The Linux backend is V4L2; other platforms report "unsupported"
//! until their backend lands (docs/roadmap.md).
pub mod convert;
mod device;

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
}

pub type SharedFrame = Arc<VideoFrame>;
