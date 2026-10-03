#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mjpeg: bool,
}

#[derive(Debug)]
pub struct CaptureError(pub String);
impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CaptureError {}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use crate::{convert, SharedFrame, VideoFrame};
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicBool, Ordering};
    use v4l::buffer::Type;
    use v4l::io::mmap::Stream;
    use v4l::io::traits::CaptureStream;
    use v4l::video::Capture as _;
    use v4l::{Device, FourCC};

    pub fn list_devices() -> Vec<DeviceInfo> {
        let mut out = Vec::new();
        for node in v4l::context::enum_devices() {
            let path = node.path().to_string_lossy().into_owned();
            let Ok(dev) = Device::with_path(&path) else { continue };
            let Ok(caps) = dev.query_caps() else { continue };
            // Metadata nodes share a card name; only list nodes that can capture frames.
            if caps.capabilities.contains(v4l::capability::Flags::VIDEO_CAPTURE) && dev.enum_formats().map(|f| !f.is_empty()).unwrap_or(false) {
                out.push(DeviceInfo { path, name: caps.card });
            }
        }
        out
    }

    /// Preference: MJPEG (cards only deliver high resolutions at high fps compressed), closest to 1080p, >= 30 fps.
    fn pick_mode(dev: &Device) -> Result<(Mode, FourCC), CaptureError> {
        let mjpg = FourCC::new(b"MJPG");
        let yuyv = FourCC::new(b"YUYV");
        let mut best: Option<(i64, Mode, FourCC)> = None;
        let fmts = dev.enum_formats().map_err(|e| CaptureError(e.to_string()))?;
        for f in fmts.iter().filter(|f| f.fourcc == mjpg || f.fourcc == yuyv) {
            let Ok(sizes) = dev.enum_framesizes(f.fourcc) else { continue };
            for fs in sizes {
                for s in fs.size.to_discrete() {
                    let fps = dev
                        .enum_frameintervals(f.fourcc, s.width, s.height)
                        .ok()
                        .and_then(|iv| {
                            iv.iter()
                                .filter_map(|i| match &i.interval {
                                    v4l::frameinterval::FrameIntervalEnum::Discrete(f) if f.numerator > 0 => Some(f.denominator / f.numerator),
                                    _ => None,
                                })
                                .max()
                        })
                        .unwrap_or(30);
                    let area = (s.width * s.height) as i64;
                    let target = 1920 * 1080;
                    // lower is better: distance from 1080p, penalise <30 fps heavily, prefer MJPEG on ties
                    let score = (area - target).abs() + if fps < 25 { 50_000_000 } else { 0 } + if f.fourcc == mjpg { 0 } else { 1_000 };
                    let mode = Mode { width: s.width, height: s.height, fps, mjpeg: f.fourcc == mjpg };
                    if best.as_ref().is_none_or(|(b, _, _)| score < *b) {
                        best = Some((score, mode, f.fourcc));
                    }
                }
            }
        }
        best.map(|(_, m, f)| (m, f)).ok_or_else(|| CaptureError("device offers no MJPEG/YUYV mode".into()))
    }

    pub struct Capture {
        latest: Arc<Mutex<Option<SharedFrame>>>,
        stop: Arc<AtomicBool>,
        pub mode: Mode,
        pub info: DeviceInfo,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Capture {
        pub fn open(path: &str) -> Result<Capture, CaptureError> {
            let dev = Device::with_path(path).map_err(|e| CaptureError(format!("{path}: {e}")))?;
            let name = dev.query_caps().map(|c| c.card).unwrap_or_default();
            let (mode, fourcc) = pick_mode(&dev)?;
            let applied = dev
                .set_format(&v4l::Format::new(mode.width, mode.height, fourcc))
                .map_err(|e| CaptureError(format!("set format: {e}")))?;
            let mode = Mode { width: applied.width, height: applied.height, ..mode };
            let latest: Arc<Mutex<Option<SharedFrame>>> = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let (l2, s2) = (latest.clone(), stop.clone());
            let thread = std::thread::Builder::new()
                .name("kvmit-capture".into())
                .spawn(move || {
                    let Ok(mut stream) = Stream::with_buffers(&dev, Type::VideoCapture, 4) else { return };
                    let mut seq = 0u64;
                    while !s2.load(Ordering::Relaxed) {
                        let Ok((buf, meta)) = stream.next() else {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            continue;
                        };
                        let used = &buf[..(meta.bytesused as usize).min(buf.len())];
                        let decoded = if mode.mjpeg {
                            convert::decode_mjpeg(used)
                        } else {
                            convert::yuyv_to_rgba(mode.width as usize, mode.height as usize, used).map(|r| (mode.width as usize, mode.height as usize, r))
                        };
                        if let Some((width, height, rgba)) = decoded {
                            seq += 1;
                            *l2.lock().unwrap() = Some(Arc::new(VideoFrame { width, height, rgba, seq }));
                        }
                    }
                })
                .map_err(|e| CaptureError(e.to_string()))?;
            Ok(Capture { latest, stop, mode, info: DeviceInfo { path: path.into(), name }, thread: Some(thread) })
        }

        pub fn latest(&self) -> Option<SharedFrame> {
            self.latest.lock().unwrap().clone()
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::*;
    use crate::SharedFrame;
    pub fn list_devices() -> Vec<DeviceInfo> {
        Vec::new()
    }
    pub struct Capture {
        pub mode: Mode,
        pub info: DeviceInfo,
    }
    impl Capture {
        pub fn open(_path: &str) -> Result<Capture, CaptureError> {
            Err(CaptureError("video capture is not implemented on this platform yet (Linux/V4L2 only)".into()))
        }
        pub fn latest(&self) -> Option<SharedFrame> {
            None
        }
    }
}

pub use imp::{list_devices, Capture};
