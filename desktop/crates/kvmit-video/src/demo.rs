//! A synthetic capture source: serves one still PNG as if a capture card were showing it. It is off unless the
//! `KVMIT_DEMO_VIDEO` environment variable names a PNG, and it exists so documentation screenshots and tests never need a
//! real machine's screen. Its name says what it is, so it cannot be mistaken for a real card.
use crate::{CaptureError, DeviceInfo, Mode, SharedFrame, VideoFrame};
use std::sync::Arc;

/// Device paths of the demo source start with this, followed by the PNG's path.
pub const PREFIX: &str = "demo:";

/// The demo device, if `KVMIT_DEMO_VIDEO` names a picture.
pub fn devices() -> Vec<DeviceInfo> {
    match std::env::var_os("KVMIT_DEMO_VIDEO") {
        Some(p) if !p.is_empty() => vec![DeviceInfo { path: format!("{PREFIX}{}", p.to_string_lossy()), name: "Demo target (synthetic picture)".into() }],
        _ => Vec::new(),
    }
}

pub struct DemoCapture {
    frame: SharedFrame,
    pub mode: Mode,
}

impl DemoCapture {
    pub fn open(png: &str) -> Result<DemoCapture, CaptureError> {
        let img = image::open(png).map_err(|e| CaptureError(format!("demo picture {png}: {e}")))?.into_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        let frame = Arc::new(VideoFrame { width: w, height: h, rgba: img.into_raw(), seq: 1 });
        Ok(DemoCapture { frame, mode: Mode { width: w as u32, height: h as u32, fps: 30, mjpeg: false } })
    }

    pub fn latest(&self) -> Option<SharedFrame> {
        Some(self.frame.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_the_picture_it_was_given() {
        let path = std::env::temp_dir().join(format!("kvmit-demo-test-{}.png", std::process::id()));
        image::save_buffer(&path, &[10u8, 20, 30, 255].repeat(6), 3, 2, image::ColorType::Rgba8).unwrap();
        let c = DemoCapture::open(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((c.mode.width, c.mode.height), (3, 2));
        let f = c.latest().unwrap();
        assert_eq!((f.width, f.height, f.rgba.len()), (3, 2, 24));
        assert_eq!(&f.rgba[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn a_missing_picture_is_an_error_not_a_blank_card() {
        assert!(DemoCapture::open("/nonexistent/demo.png").is_err());
    }
}
