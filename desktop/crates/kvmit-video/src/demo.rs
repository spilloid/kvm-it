//! A synthetic capture source: serves one still PNG as if a capture card were showing it. It is off unless the
//! `KVMIT_DEMO_VIDEO` environment variable names a PNG, and it exists so documentation screenshots and tests never need a
//! real machine's screen. Its name says what it is, so it cannot be mistaken for a real card.
use crate::{CaptureError, DeviceInfo, Mode, SharedFrame, VideoFrame};
use std::sync::Arc;

/// Device paths of the demo source start with this, followed by the PNG's path.
pub const PREFIX: &str = "demo:";

/// The demo device, if `KVMIT_DEMO_VIDEO` names a picture.
pub fn devices() -> Vec<DeviceInfo> {
    match enabled_path() {
        Some(p) => vec![DeviceInfo::new(format!("{PREFIX}{p}"), "Demo target (synthetic picture)")],
        None => Vec::new(),
    }
}

/// The picture named by `KVMIT_DEMO_VIDEO`; none if unset, empty, or not valid UTF-8 (a lossy path would open a
/// different file than the one named).
fn enabled_path() -> Option<String> {
    std::env::var_os("KVMIT_DEMO_VIDEO").filter(|p| !p.is_empty()).and_then(|p| p.into_string().ok())
}

/// Larger than any GPU's texture limit we would upload; also bounds the decode.
const MAX_SIDE: u32 = 8192;

pub struct DemoCapture {
    frame: SharedFrame,
    pub mode: Mode,
}

impl DemoCapture {
    pub fn open(png: &str) -> Result<DemoCapture, CaptureError> {
        // opening is as opt-in as listing: a saved "demo:" path does nothing unless the environment variable is set
        if enabled_path().is_none() {
            return Err(CaptureError("the demo video source is off (KVMIT_DEMO_VIDEO is not set)".into()));
        }
        Self::open_checked(png)
    }

    fn open_checked(png: &str) -> Result<DemoCapture, CaptureError> {
        if !std::fs::metadata(png).map(|m| m.is_file()).unwrap_or(false) {
            return Err(CaptureError(format!("demo picture {png}: not a regular file")));
        }
        let (iw, ih) = image::image_dimensions(png).map_err(|e| CaptureError(format!("demo picture {png}: {e}")))?;
        if iw == 0 || ih == 0 || iw > MAX_SIDE || ih > MAX_SIDE {
            return Err(CaptureError(format!("demo picture {png}: {iw}x{ih} is outside 1..={MAX_SIDE} per side")));
        }
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
        let c = DemoCapture::open_checked(path.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((c.mode.width, c.mode.height), (3, 2));
        let f = c.latest().unwrap();
        assert_eq!((f.width, f.height, f.rgba.len()), (3, 2, 24));
        assert_eq!(&f.rgba[0..4], &[10, 20, 30, 255]);
    }

    #[test]
    fn a_missing_picture_is_an_error_not_a_blank_card() {
        assert!(DemoCapture::open_checked("/nonexistent/demo.png").is_err());
    }

    #[test]
    fn a_picture_wider_than_any_texture_is_refused() {
        let path = std::env::temp_dir().join(format!("kvmit-demo-wide-{}.png", std::process::id()));
        image::save_buffer(&path, &vec![0u8; 9000 * 4], 9000, 1, image::ColorType::Rgba8).unwrap();
        let r = DemoCapture::open_checked(path.to_str().unwrap());
        let _ = std::fs::remove_file(&path);
        assert!(r.is_err());
    }

    #[test]
    fn a_directory_is_not_a_picture() {
        assert!(DemoCapture::open_checked(std::env::temp_dir().to_str().unwrap()).is_err());
    }
}
