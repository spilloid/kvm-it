//! macOS capture backend: AVFoundation. The session asks for 32BGRA pixel buffers, so macOS itself decodes whatever the
//! card sends (MJPEG or YUV) and there is no decoder here; frames are converted to RGBA honouring the buffer's stride.
//! A capture card is a "camera" to macOS: the first open asks for camera permission (Info.plist carries the reason).
//!
//! Threading: nothing slow runs on the caller's (GUI) thread. Asking for camera permission does not wait for the answer;
//! the session is started and stopped on a worker thread (startRunning/stopRunning block and must stay off the main
//! thread); frames arrive on a private serial dispatch queue.
use super::*;
use crate::negotiate::best_mode;
use crate::{SharedFrame, VideoFrame};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureDeviceFormat, AVFrameRateRange, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSObject, NSString};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// No frame for this long while the device says it is connected: the stream is treated as dead.
const STALL_LIMIT: Duration = Duration::from_secs(5);

fn video_type() -> Option<&'static objc2_av_foundation::AVMediaType> {
    unsafe { AVMediaTypeVideo }
}

pub fn list_devices() -> Vec<DeviceInfo> {
    let Some(video) = video_type() else { return Vec::new() };
    // `devicesWithMediaType` is deprecated in favour of discovery sessions, whose device-type list changes between macOS
    // releases (external devices are a 14+ type); this one call lists built-in and USB (UVC) devices on every version.
    #[allow(deprecated)]
    let devices = unsafe { AVCaptureDevice::devicesWithMediaType(video) };
    devices
        .iter()
        .map(|d| {
            // uniqueID is stable on one Mac across replugs and reboots (for USB: location + vendor/product), so it is both the
            // path and the key.
            let id = unsafe { d.uniqueID() }.to_string();
            DeviceInfo::new(id, unsafe { d.localizedName() }.to_string())
        })
        .collect()
}

/// Camera permission. Never waits: if macOS has not asked yet, start its prompt and report that, so the GUI stays live while
/// the person answers; opening the card again after allowing it works.
fn ensure_permission() -> Result<(), CaptureError> {
    let Some(video) = video_type() else { return Err(CaptureError("AVFoundation is unavailable".into())) };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) } {
        AVAuthorizationStatus::Authorized => Ok(()),
        AVAuthorizationStatus::NotDetermined => {
            let block = block2::RcBlock::new(|_granted: Bool| {});
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(video, &block) };
            Err(CaptureError(
                "macOS is asking whether kvm-it may use the camera (a capture card counts as one): answer its prompt, then open the card again".into(),
            ))
        }
        _ => Err(CaptureError(
            "macOS denied camera access, which a capture card needs: allow kvm-it in System Settings > Privacy & Security > Camera, then open the card again".into(),
        )),
    }
}

/// What the delegate shares with `Capture`.
struct Shared {
    latest: Mutex<Option<SharedFrame>>,
    seq: AtomicU64,
    last_frame: Mutex<Instant>,
    /// The worker could not configure or start the session.
    start_failed: AtomicBool,
}

/// BGRA rows (with padding) -> tightly packed RGBA.
pub(crate) fn bgra_to_rgba(width: usize, height: usize, stride: usize, src: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || stride < width * 4 || src.len() < stride * (height - 1) + width * 4 {
        return None;
    }
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        let row = &src[y * stride..y * stride + width * 4];
        for (o, px) in out[y * width * 4..(y + 1) * width * 4].as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<4>().0) {
            o[0] = px[2];
            o[1] = px[1];
            o[2] = px[0];
            o[3] = 255;
        }
    }
    Some(out)
}

pub(crate) struct DelegateIvars {
    shared: Arc<Shared>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "KvmitCaptureDelegate"]
    #[ivars = DelegateIvars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn on_frame(&self, _output: &AVCaptureOutput, sample: &CMSampleBuffer, _connection: &AVCaptureConnection) {
            let Some(buf) = (unsafe { sample.image_buffer() }) else { return };
            if CVPixelBufferGetPixelFormatType(&buf) != kCVPixelFormatType_32BGRA {
                return; // the session was asked for BGRA; anything else is not ours to guess at
            }
            if unsafe { CVPixelBufferLockBaseAddress(&buf, CVPixelBufferLockFlags::ReadOnly) } != 0 {
                return;
            }
            let (w, h, stride) = (CVPixelBufferGetWidth(&buf), CVPixelBufferGetHeight(&buf), CVPixelBufferGetBytesPerRow(&buf));
            let base = CVPixelBufferGetBaseAddress(&buf) as *const u8;
            let rgba = if base.is_null() || h == 0 {
                None
            } else {
                // SAFETY: the base address is locked and covers `stride * height` bytes until the unlock below.
                let src = unsafe { std::slice::from_raw_parts(base, stride * h) };
                bgra_to_rgba(w, h, stride, src)
            };
            unsafe { CVPixelBufferUnlockBaseAddress(&buf, CVPixelBufferLockFlags::ReadOnly) };
            if let Some(rgba) = rgba {
                let sh = &self.ivars().shared;
                let seq = sh.seq.fetch_add(1, Ordering::SeqCst) + 1;
                *sh.latest.lock().unwrap() = Some(Arc::new(VideoFrame { width: w, height: h, rgba, seq }));
                *sh.last_frame.lock().unwrap() = Instant::now();
            }
        }
    }
);

impl Delegate {
    fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { shared });
        unsafe { msg_send![super(this), init] }
    }
}

/// The AVFoundation objects. They are only touched through calls AVFoundation documents as thread-safe (start/stop,
/// `isRunning`, `isConnected`), so the bundle may move between threads with the `Capture` that owns it.
struct Session {
    session: Retained<AVCaptureSession>,
    device: Retained<AVCaptureDevice>,
    output: Retained<AVCaptureVideoDataOutput>,
    _delegate: Retained<Delegate>,
    _queue: DispatchRetained<DispatchQueue>,
}

/// The chosen format and the frame-rate range it is used at, handed to the worker that applies them.
struct Chosen {
    format: Retained<AVCaptureDeviceFormat>,
    range: Option<Retained<AVFrameRateRange>>,
}
// SAFETY: immutable description objects, only read.
unsafe impl Send for Chosen {}
// SAFETY: see the struct comment.
unsafe impl Send for Session {}
unsafe impl Sync for Session {}

pub struct Capture {
    shared: Arc<Shared>,
    /// `None` only while dropping (it moves to the thread that stops it).
    session: Option<Arc<Session>>,
    stopped: AtomicBool,
    pub mode: Mode,
    pub info: DeviceInfo,
}

impl Capture {
    pub fn open(path: &str) -> Result<Capture, CaptureError> {
        ensure_permission()?;
        let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(path)) }
            .ok_or_else(|| CaptureError(format!("capture device {path} is not connected")))?;
        let name = unsafe { device.localizedName() }.to_string();

        // Pick the format closest to 1080p at a usable rate (the same preference as the other backends). Each format's
        // fastest frame-rate range is kept as an object: its exact minimum duration is what gets applied (a rounded rate
        // such as 1/60 for a 59.94 fps range is outside it, and AVFoundation throws on an unsupported duration).
        let formats = unsafe { device.formats() };
        let mut candidates: Vec<(u32, u32, u32)> = Vec::new();
        let mut ranges: Vec<Option<Retained<AVFrameRateRange>>> = Vec::new();
        for f in formats.iter() {
            let dims = unsafe { CMVideoFormatDescriptionGetDimensions(&f.formatDescription()) };
            let best = unsafe { f.videoSupportedFrameRateRanges() }
                .iter()
                .max_by(|a, b| unsafe { a.maxFrameRate().total_cmp(&b.maxFrameRate()) });
            let fps = best.as_ref().map_or(0.0, |r| unsafe { r.maxFrameRate() });
            candidates.push((dims.width.max(0) as u32, dims.height.max(0) as u32, fps.round() as u32));
            ranges.push(best);
        }
        let i = best_mode(&candidates).ok_or_else(|| CaptureError(format!("{name}: no usable video format")))?;
        let (width, height, fps) = candidates[i];
        let chosen = Chosen { format: formats.objectAtIndex(i), range: ranges.swap_remove(i) };

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| CaptureError(format!("{name}: {} (in use by another app?)", e.localizedDescription())))?;
        let session = unsafe { AVCaptureSession::new() };
        let output = unsafe { AVCaptureVideoDataOutput::new() };
        let shared = Arc::new(Shared {
            latest: Mutex::new(None),
            seq: AtomicU64::new(0),
            last_frame: Mutex::new(Instant::now()),
            start_failed: AtomicBool::new(false),
        });
        let delegate = Delegate::new(shared.clone());
        let queue = DispatchQueue::new("kvmit-capture", DispatchQueueAttr::SERIAL);
        unsafe {
            // SAFETY: the key is a CFString, toll-free bridged to NSString; the value is the 32BGRA pixel format.
            let key: &NSString = &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString);
            let value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[value.as_ref() as &AnyObject]);
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
            output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
            session.beginConfiguration();
            if !session.canAddInput(&input) || !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(CaptureError(format!("{name}: macOS would not attach it to a capture session")));
            }
            session.addInput(&input);
            session.addOutput(&output);
            session.commitConfiguration();
        }
        let sess = Arc::new(Session { session, device, output, _delegate: delegate, _queue: queue });

        // Start on a worker. On macOS a session picks its own format when it starts unless the device stays locked with
        // the chosen one until it is running (Apple's documented pattern: lock, set, start, unlock).
        let (worker_sess, worker_shared, worker_name) = (sess.clone(), shared.clone(), name.clone());
        std::thread::Builder::new()
            .name("kvmit-capture-start".into())
            .spawn(move || {
                let chosen = chosen; // move the whole (Send) value, not its fields one by one
                let s = &worker_sess;
                let ok = unsafe {
                    match s.device.lockForConfiguration() {
                        Ok(()) => {
                            s.device.setActiveFormat(&chosen.format);
                            if let Some(r) = &chosen.range {
                                s.device.setActiveVideoMinFrameDuration(r.minFrameDuration());
                            }
                            s.session.startRunning();
                            s.device.unlockForConfiguration();
                            s.session.isRunning()
                        }
                        Err(e) => {
                            eprintln!("kvm-it: {worker_name}: could not configure the capture device ({})", e.localizedDescription());
                            false
                        }
                    }
                };
                if !ok {
                    worker_shared.start_failed.store(true, Ordering::SeqCst);
                }
                *worker_shared.last_frame.lock().unwrap() = Instant::now(); // the stall clock starts once it runs
            })
            .map_err(|e| CaptureError(e.to_string()))?;
        let mode = Mode { width, height, fps, mjpeg: false };
        Ok(Capture { shared, session: Some(sess), stopped: AtomicBool::new(false), mode, info: DeviceInfo::new(path, name) })
    }

    pub fn latest(&self) -> Option<SharedFrame> {
        self.shared.latest.lock().unwrap().clone()
    }

    /// The card was unplugged, the session stopped, or frames stopped arriving for `STALL_LIMIT`.
    pub fn failed(&self) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return true;
        }
        let Some(s) = &self.session else { return true };
        // `isRunning` is not checked: it is false until the worker has started the session; a failed start is flagged.
        let gone = self.shared.start_failed.load(Ordering::SeqCst) || !unsafe { s.device.isConnected() };
        let stalled = self.shared.last_frame.lock().unwrap().elapsed() > STALL_LIMIT;
        if gone || stalled {
            self.stopped.store(true, Ordering::SeqCst);
        }
        gone || stalled
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // Stop on a worker: stopRunning blocks, and this may be the GUI thread. The delegate stops publishing at once.
        let Some(s) = self.session.take() else { return };
        unsafe { s.output.setSampleBufferDelegate_queue(None, None) };
        let _ = std::thread::Builder::new().name("kvmit-capture-stop".into()).spawn(move || unsafe { s.session.stopRunning() });
    }
}

#[cfg(test)]
mod tests {
    use super::bgra_to_rgba;

    #[test]
    fn bgra_rows_with_padding_become_packed_rgba() {
        // 2x2, stride 12 (4 bytes of padding per row)
        let src = [1, 2, 3, 9, 4, 5, 6, 9, 0, 0, 0, 0, 7, 8, 9, 9, 10, 11, 12, 9, 0, 0, 0, 0];
        let out = bgra_to_rgba(2, 2, 12, &src).unwrap();
        assert_eq!(out, vec![3, 2, 1, 255, 6, 5, 4, 255, 9, 8, 7, 255, 12, 11, 10, 255]);
    }

    #[test]
    fn short_buffers_are_refused() {
        assert!(bgra_to_rgba(2, 2, 12, &[0; 10]).is_none());
        assert!(bgra_to_rgba(2, 2, 4, &[0; 64]).is_none(), "stride shorter than a row");
    }
}
