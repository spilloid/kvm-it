//! macOS capture backend: AVFoundation. The session asks for 32BGRA pixel buffers, so macOS itself decodes whatever the
//! card sends (MJPEG or YUV) and there is no decoder here; frames are converted to RGBA honouring the buffer's stride.
//! A capture card is a "camera" to macOS: the first open asks for camera permission (Info.plist carries the reason).
//!
//! Threading: AVFoundation objects used here are documented as safe to drive from any thread (startRunning/stopRunning
//! must not run on the main thread, and do not here); frames arrive on a private serial dispatch queue.
use super::*;
use crate::negotiate::best_mode;
use crate::{SharedFrame, VideoFrame};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput, AVCaptureSession,
    AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
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

/// Ask for camera permission if it was never asked, and wait (bounded) for the answer.
fn ensure_permission() -> Result<(), CaptureError> {
    let Some(video) = video_type() else { return Err(CaptureError("AVFoundation is unavailable".into())) };
    let denied = || {
        CaptureError(
            "macOS denied camera access, which a capture card needs: allow kvm-it in System Settings > Privacy & Security > Camera, then open the card again".into(),
        )
    };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(video) } {
        AVAuthorizationStatus::Authorized => Ok(()),
        AVAuthorizationStatus::NotDetermined => {
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            let tx = Mutex::new(Some(tx));
            let block = block2::RcBlock::new(move |granted: Bool| {
                if let Some(tx) = tx.lock().unwrap().take() {
                    let _ = tx.send(granted.as_bool());
                }
            });
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(video, &block) };
            match rx.recv_timeout(Duration::from_secs(120)) {
                Ok(true) => Ok(()),
                Ok(false) => Err(denied()),
                Err(_) => Err(CaptureError("no answer to the camera-permission prompt: open the card again to retry".into())),
            }
        }
        _ => Err(denied()),
    }
}

/// What the delegate shares with `Capture`.
struct Shared {
    latest: Mutex<Option<SharedFrame>>,
    seq: AtomicU64,
    last_frame: Mutex<Instant>,
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
// SAFETY: see the struct comment.
unsafe impl Send for Session {}

pub struct Capture {
    shared: Arc<Shared>,
    session: Session,
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

        // Pick the format closest to 1080p at a usable rate (the same preference as the other backends).
        let formats = unsafe { device.formats() };
        let candidates: Vec<(u32, u32, u32)> = formats
            .iter()
            .map(|f| {
                let dims = unsafe { CMVideoFormatDescriptionGetDimensions(&f.formatDescription()) };
                let fps = unsafe { f.videoSupportedFrameRateRanges() }.iter().map(|r| unsafe { r.maxFrameRate() }).fold(0.0, f64::max);
                (dims.width.max(0) as u32, dims.height.max(0) as u32, fps.round() as u32)
            })
            .collect();
        let i = best_mode(&candidates).ok_or_else(|| CaptureError(format!("{name}: no usable video format")))?;
        let (width, height, fps) = candidates[i];
        let format = formats.objectAtIndex(i);
        unsafe {
            device
                .lockForConfiguration()
                .map_err(|e| CaptureError(format!("{name}: could not configure it ({})", e.localizedDescription())))?;
            device.setActiveFormat(&format);
            if fps > 0 {
                let frame = CMTime::new(1, fps as i32);
                device.setActiveVideoMinFrameDuration(frame);
            }
            device.unlockForConfiguration();
        }

        let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
            .map_err(|e| CaptureError(format!("{name}: {} (in use by another app?)", e.localizedDescription())))?;
        let session = unsafe { AVCaptureSession::new() };
        let output = unsafe { AVCaptureVideoDataOutput::new() };
        let shared = Arc::new(Shared { latest: Mutex::new(None), seq: AtomicU64::new(0), last_frame: Mutex::new(Instant::now()) });
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
            session.startRunning(); // blocks until running; this is never the main thread's event loop
        }
        if !unsafe { session.isRunning() } {
            return Err(CaptureError(format!("{name}: the capture session did not start")));
        }
        *shared.last_frame.lock().unwrap() = Instant::now();
        let mode = Mode { width, height, fps, mjpeg: false };
        Ok(Capture {
            shared,
            session: Session { session, device, output, _delegate: delegate, _queue: queue },
            stopped: AtomicBool::new(false),
            mode,
            info: DeviceInfo::new(path, name),
        })
    }

    pub fn latest(&self) -> Option<SharedFrame> {
        self.shared.latest.lock().unwrap().clone()
    }

    /// The card was unplugged, the session stopped, or frames stopped arriving for `STALL_LIMIT`.
    pub fn failed(&self) -> bool {
        if self.stopped.load(Ordering::SeqCst) {
            return true;
        }
        let gone = !unsafe { self.session.device.isConnected() } || !unsafe { self.session.session.isRunning() };
        let stalled = self.shared.last_frame.lock().unwrap().elapsed() > STALL_LIMIT;
        if gone || stalled {
            self.stopped.store(true, Ordering::SeqCst);
        }
        gone || stalled
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            self.session.output.setSampleBufferDelegate_queue(None, None);
            self.session.session.stopRunning(); // synchronous; AVFoundation returns promptly even for a vanished device
        }
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
