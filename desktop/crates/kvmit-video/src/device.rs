#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// What `Capture::open` takes (`/dev/videoN` on Linux, the symbolic link on Windows). Can change across replugs.
    pub path: String,
    pub name: String,
    /// Stable identity to remember the device by: see [`device_key`].
    pub key: String,
}

impl DeviceInfo {
    /// A device whose path is already its stable identity.
    pub fn new(path: impl Into<String>, name: impl Into<String>) -> DeviceInfo {
        let path = path.into();
        DeviceInfo { key: path.clone(), path, name: name.into() }
    }
}

/// Stable key for a device: the card name plus its bus location (V4L2 `bus_info`, e.g. `usb-0000:00:14.0-2`), which
/// survives replugs into the same port, unlike `/dev/videoN`. Without bus info the name alone is used.
pub fn device_key(card: &str, bus_info: &str) -> String {
    let (card, bus) = (card.trim(), bus_info.trim());
    if bus.is_empty() {
        card.to_string()
    } else {
        format!("{card} @ {bus}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mjpeg: bool,
}

/// Mode preference shared by every backend (lower is better): closest to 1080p, heavily penalise < 25 fps (cards
/// only deliver high resolutions at high fps compressed), and prefer MJPEG on ties.
pub(crate) fn mode_score(width: u32, height: u32, fps: u32, mjpeg: bool) -> i64 {
    let area = i64::from(width) * i64::from(height);
    (area - 1920 * 1080).abs() + if fps < 25 { 50_000_000 } else { 0 } + if mjpeg { 0 } else { 1_000 }
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
    use crate::negotiate::{self, DoneOnDrop};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
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
                let key = device_key(&caps.card, &caps.bus);
                out.push(DeviceInfo { path, name: caps.card, key });
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
                    let score = mode_score(s.width, s.height, fps, f.fourcc == mjpg);
                    let mode = Mode { width: s.width, height: s.height, fps, mjpeg: f.fourcc == mjpg };
                    if best.as_ref().is_none_or(|(b, _, _)| score < *b) {
                        best = Some((score, mode, f.fourcc));
                    }
                }
            }
        }
        best.map(|(_, m, f)| (m, f)).ok_or_else(|| CaptureError("device offers no MJPEG/YUYV mode".into()))
    }

    /// How long one blocking dequeue waits for a frame before it counts as a stall.
    const FRAME_TIMEOUT: Duration = Duration::from_millis(500);
    /// A stall weighs this much against the consecutive-error limit (so ~5 s of silence marks the card gone).
    const TIMEOUT_WEIGHT: u32 = 2;
    /// How long dropping a capture waits for its thread before detaching it (see `negotiate::join_bounded`).
    const SHUTDOWN_WAIT: Duration = Duration::from_millis(1500);

    pub struct Capture {
        latest: Arc<Mutex<Option<SharedFrame>>>,
        stop: Arc<AtomicBool>,
        /// Set when the capture thread gave up: the card was unplugged or reset, or the stream could not run.
        failed: Arc<AtomicBool>,
        done: Receiver<()>,
        pub mode: Mode,
        pub info: DeviceInfo,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    /// Marks the capture failed when its thread ends for any reason other than being asked to stop (error limit,
    /// or a panic such as the v4l `Stream` drop panicking on an unexpected ioctl error).
    struct FailOnExit {
        stop: Arc<AtomicBool>,
        failed: Arc<AtomicBool>,
    }
    impl Drop for FailOnExit {
        fn drop(&mut self) {
            if !self.stop.load(Ordering::SeqCst) {
                self.failed.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Request the frame rate, then read back what the driver applied.
    fn applied_interval(dev: &Device, fps: u32) -> Option<(u32, u32)> {
        let p = dev.set_params(&v4l::video::capture::Parameters::with_fps(fps)).or_else(|_| dev.params()).ok()?;
        Some((p.interval.numerator, p.interval.denominator))
    }

    impl Capture {
        pub fn open(path: &str) -> Result<Capture, CaptureError> {
            let dev = Device::with_path(path).map_err(|e| CaptureError(format!("{path}: {e}")))?;
            let caps = dev.query_caps().ok();
            let name = caps.as_ref().map(|c| c.card.clone()).unwrap_or_default();
            let key = caps.as_ref().map(|c| device_key(&c.card, &c.bus)).unwrap_or_else(|| path.to_string());
            let (wanted, fourcc) = pick_mode(&dev)?;
            let fmt = dev.set_format(&v4l::Format::new(wanted.width, wanted.height, fourcc)).map_err(|e| CaptureError(format!("set format: {e}")))?;
            // Ask for the frame interval (otherwise the card runs at its default) and read back what was applied.
            let interval = applied_interval(&dev, wanted.fps);
            let (mode, layout) = negotiate::applied_mode(
                wanted,
                negotiate::Applied { fourcc: fmt.fourcc.repr, width: fmt.width, height: fmt.height, stride: fmt.stride, interval },
            )
            .map_err(CaptureError)?;
            let latest: Arc<Mutex<Option<SharedFrame>>> = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let failed = Arc::new(AtomicBool::new(false));
            let (l2, s2, f2) = (latest.clone(), stop.clone(), failed.clone());
            let (tx, rx) = mpsc::channel::<Result<(), CaptureError>>();
            let (done_tx, done) = mpsc::channel::<()>();
            let thread = std::thread::Builder::new()
                .name("kvmit-capture".into())
                .spawn(move || {
                    let _signal_done = DoneOnDrop(done_tx);
                    let _fail = FailOnExit { stop: s2.clone(), failed: f2 };
                    let mut stream = match Stream::with_buffers(&dev, Type::VideoCapture, 4) {
                        Ok(s) => s,
                        Err(e) => return drop(tx.send(Err(CaptureError(format!("start streaming: {e}"))))),
                    };
                    stream.set_timeout(FRAME_TIMEOUT);
                    let _ = tx.send(Ok(()));
                    let mut seq = 0u64;
                    let mut errors = negotiate::ErrorTracker::default();
                    while !s2.load(Ordering::Relaxed) {
                        let (buf, meta) = match stream.next() {
                            Ok(b) => b,
                            Err(e) => {
                                let timed_out = e.kind() == std::io::ErrorKind::TimedOut;
                                if errors.error(if timed_out { TIMEOUT_WEIGHT } else { 1 }) {
                                    break; // FailOnExit marks the capture failed
                                }
                                if !timed_out {
                                    std::thread::sleep(Duration::from_millis(50));
                                }
                                continue;
                            }
                        };
                        errors.ok();
                        let used = &buf[..(meta.bytesused as usize).min(buf.len())];
                        let decoded = if mode.mjpeg {
                            convert::decode_mjpeg(used)
                        } else {
                            convert::yuyv_to_rgba_strided(mode.width as usize, mode.height as usize, layout.stride, used)
                                .map(|r| (mode.width as usize, mode.height as usize, r))
                        };
                        if let Some((width, height, rgba)) = decoded {
                            seq += 1;
                            *l2.lock().unwrap() = Some(Arc::new(VideoFrame { width, height, rgba, seq }));
                        }
                    }
                })
                .map_err(|e| CaptureError(e.to_string()))?;
            // A failed stream start is the open's error, not a silent "waiting for a frame…" forever.
            rx.recv().map_err(|_| CaptureError("capture thread exited".into()))??;
            Ok(Capture { latest, stop, failed, done, mode, info: DeviceInfo { path: path.into(), name, key }, thread: Some(thread) })
        }

        pub fn latest(&self) -> Option<SharedFrame> {
            self.latest.lock().unwrap().clone()
        }

        /// True once the capture thread has given up (card unplugged or reset, stream errors, thread panic): the
        /// last frame is stale and the capture should be dropped and reopened.
        pub fn failed(&self) -> bool {
            self.failed.load(Ordering::SeqCst)
        }
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                // The thread wakes at least every FRAME_TIMEOUT; past SHUTDOWN_WAIT it is detached, never hanging the UI.
                negotiate::join_bounded(t, &self.done, SHUTDOWN_WAIT);
            }
        }
    }
}

#[cfg(windows)]
#[path = "mf.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", windows)))]
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
        pub fn failed(&self) -> bool {
            false
        }
    }
}

use imp::Capture as PlatformCapture;

/// A capture device: the platform's real backend, or the synthetic demo source (`demo::PREFIX` paths).
pub struct Capture {
    inner: Inner,
    pub mode: Mode,
    pub info: DeviceInfo,
}

enum Inner {
    Platform(PlatformCapture),
    Demo(crate::demo::DemoCapture),
}

impl Capture {
    pub fn open(path: &str) -> Result<Capture, CaptureError> {
        if let Some(png) = path.strip_prefix(crate::demo::PREFIX) {
            let c = crate::demo::DemoCapture::open(png)?;
            let info = DeviceInfo::new(path, "Demo target (synthetic picture)");
            return Ok(Capture { mode: c.mode, info, inner: Inner::Demo(c) });
        }
        let c = PlatformCapture::open(path)?;
        Ok(Capture { mode: c.mode, info: c.info.clone(), inner: Inner::Platform(c) })
    }

    pub fn latest(&self) -> Option<crate::SharedFrame> {
        match &self.inner {
            Inner::Platform(c) => c.latest(),
            Inner::Demo(c) => c.latest(),
        }
    }

    /// True once the capture gave up (card unplugged or reset, stream ended): the last frame is stale.
    pub fn failed(&self) -> bool {
        match &self.inner {
            Inner::Platform(c) => c.failed(),
            Inner::Demo(_) => false,
        }
    }
}

/// All capture devices: the platform's, plus the demo source when `KVMIT_DEMO_VIDEO` names a picture.
pub fn list_devices() -> Vec<DeviceInfo> {
    let mut v = imp::list_devices();
    v.extend(crate::demo::devices());
    v
}

#[cfg(test)]
mod tests {
    use super::{device_key, mode_score};

    #[test]
    fn the_device_key_is_name_plus_bus() {
        assert_eq!(device_key("USB3.0 Capture", "usb-0000:00:14.0-2"), "USB3.0 Capture @ usb-0000:00:14.0-2");
        assert_eq!(device_key(" Cam ", ""), "Cam");
        assert_ne!(device_key("Cam", "usb-1"), device_key("Cam", "usb-2"), "two identical cards stay distinct");
    }

    #[test]
    fn prefers_1080p_mjpeg_at_high_fps() {
        let best = mode_score(1920, 1080, 60, true);
        assert!(best < mode_score(1280, 720, 60, true), "1080p beats 720p");
        assert!(best < mode_score(1920, 1080, 60, false), "MJPEG wins a tie");
        assert!(best < mode_score(1920, 1080, 10, true), "low fps is penalised heavily");
        assert!(mode_score(1920, 1080, 30, false) < mode_score(1600, 1200, 30, true), "an exact 1080p YUY2 beats a near miss");
    }
}
