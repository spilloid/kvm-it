//! Windows capture backend: Media Foundation Source Reader on the card's native MJPEG / YUY2 types (converters
//! disabled, so Windows never transcodes). Frames are decoded by the same code as on Linux. All COM objects live on
//! the capture thread, so nothing here needs to be `Send`.
use super::*;
use crate::{convert, SharedFrame, VideoFrame};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use windows::core::{Error as WinError, GUID, PCWSTR, PWSTR};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

const E_ACCESSDENIED: i32 = 0x8007_0005_u32 as i32;
const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

/// Media Foundation is reference-counted: every user of it holds one of these.
struct Mf;
impl Mf {
    fn start() -> windows::core::Result<Mf> {
        // COM may already be initialised on this thread (possibly in another apartment mode); that is fine.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        }
        Ok(Mf)
    }
}
impl Drop for Mf {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
        }
    }
}

fn string_attr(a: &IMFActivate, key: &GUID) -> Option<String> {
    unsafe {
        let (mut p, mut n) = (PWSTR::null(), 0u32);
        a.GetAllocatedString(key, &mut p, &mut n).ok()?;
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(p.0, n as usize));
        CoTaskMemFree(Some(p.0 as _));
        Some(s)
    }
}

fn vidcap_attrs() -> windows::core::Result<IMFAttributes> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 2)?;
        let attrs = attrs.ok_or_else(|| WinError::from(windows::Win32::Foundation::E_POINTER))?;
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        Ok(attrs)
    }
}

/// All video-capture devices; `path` is the device's symbolic link (what `Capture::open` takes).
pub fn list_devices() -> Vec<DeviceInfo> {
    let Ok(_mf) = Mf::start() else { return Vec::new() };
    let mut out = Vec::new();
    unsafe {
        let Ok(attrs) = vidcap_attrs() else { return out };
        let (mut list, mut count) = (std::ptr::null_mut(), 0u32);
        if MFEnumDeviceSources(&attrs, &mut list, &mut count).is_err() || list.is_null() {
            return out;
        }
        for act in std::slice::from_raw_parts(list, count as usize).iter().flatten() {
            let name = string_attr(act, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
            if let Some(path) = string_attr(act, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK) {
                out.push(DeviceInfo { path, name });
            }
        }
        // We own the array and the activate objects in it.
        for i in 0..count as usize {
            std::ptr::drop_in_place(list.add(i));
        }
        CoTaskMemFree(Some(list as _));
    }
    out
}

fn explain(what: &str, e: WinError) -> CaptureError {
    let hint = if e.code().0 == E_ACCESSDENIED {
        " (Windows blocked camera access: turn on Settings > Privacy & security > Camera > \"Let desktop apps access your camera\")"
    } else {
        ""
    };
    CaptureError(format!("{what}: {e}{hint}"))
}

/// Pick the best native MJPEG / YUY2 type, returning its index so it can be applied.
fn pick_mode(reader: &IMFSourceReader) -> Result<(Mode, IMFMediaType), CaptureError> {
    let mut best: Option<(i64, Mode, IMFMediaType)> = None;
    for i in 0.. {
        let Ok(t) = (unsafe { reader.GetNativeMediaType(VIDEO_STREAM, i) }) else { break };
        let Ok(sub) = (unsafe { t.GetGUID(&MF_MT_SUBTYPE) }) else { continue };
        let mjpeg = sub == MFVideoFormat_MJPG;
        if !mjpeg && sub != MFVideoFormat_YUY2 {
            continue;
        }
        let (Ok(size), Ok(rate)) = (unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }, unsafe { t.GetUINT64(&MF_MT_FRAME_RATE) }) else { continue };
        let (width, height) = ((size >> 32) as u32, size as u32);
        let (num, den) = ((rate >> 32) as u32, rate as u32);
        let fps = if den == 0 { 30 } else { num / den };
        let mode = Mode { width, height, fps, mjpeg };
        let score = mode_score(width, height, fps, mjpeg);
        if best.as_ref().is_none_or(|(b, _, _)| score < *b) {
            best = Some((score, mode, t));
        }
    }
    best.map(|(_, m, t)| (m, t)).ok_or_else(|| CaptureError("device offers no MJPEG/YUY2 mode".into()))
}

/// Open the device and select a mode; runs on the capture thread.
fn start_reader(path: &str) -> Result<(IMFSourceReader, Mode), CaptureError> {
    unsafe {
        let attrs = vidcap_attrs().map_err(|e| explain("create attributes", e))?;
        let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        attrs
            .SetString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, PCWSTR(wide.as_ptr()))
            .map_err(|e| explain("set device path", e))?;
        let source = MFCreateDeviceSource(&attrs).map_err(|e| explain(&format!("open {path}"), e))?;
        let mut rattrs = None;
        MFCreateAttributes(&mut rattrs, 1).map_err(|e| explain("create attributes", e))?;
        if let Some(r) = &rattrs {
            let _ = r.SetUINT32(&MF_READWRITE_DISABLE_CONVERTERS, 1); // native types only, never a transcode
        }
        let reader = MFCreateSourceReaderFromMediaSource(&source, rattrs.as_ref()).map_err(|e| explain("create reader", e))?;
        let (mode, mtype) = pick_mode(&reader)?;
        reader.SetCurrentMediaType(VIDEO_STREAM, None, &mtype).map_err(|e| explain("set format", e))?;
        Ok((reader, mode))
    }
}

/// Copy one sample's bytes out of its (possibly multi-buffer) media buffer.
fn sample_bytes(sample: &IMFSample) -> Option<Vec<u8>> {
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().ok()?;
        let (mut ptr, mut len) = (std::ptr::null_mut(), 0u32);
        buf.Lock(&mut ptr, None, Some(&mut len)).ok()?;
        let bytes = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        let _ = buf.Unlock();
        Some(bytes)
    }
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
        let latest: Arc<Mutex<Option<SharedFrame>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (l2, s2, p2) = (latest.clone(), stop.clone(), path.to_string());
        let (tx, rx) = mpsc::channel::<Result<Mode, CaptureError>>();
        let thread = std::thread::Builder::new()
            .name("kvmit-capture".into())
            .spawn(move || {
                let _mf = match Mf::start() {
                    Ok(m) => m,
                    Err(e) => return drop(tx.send(Err(explain("start Media Foundation", e)))),
                };
                let (reader, mode) = match start_reader(&p2) {
                    Ok(r) => r,
                    Err(e) => return drop(tx.send(Err(e))),
                };
                let _ = tx.send(Ok(mode));
                let mut seq = 0u64;
                while !s2.load(Ordering::Relaxed) {
                    let (mut flags, mut sample) = (0u32, None);
                    let read = unsafe { reader.ReadSample(VIDEO_STREAM, 0, None, Some(&mut flags), None, Some(&mut sample)) };
                    let ended = flags & (MF_SOURCE_READERF_ERROR.0 | MF_SOURCE_READERF_ENDOFSTREAM.0) as u32 != 0;
                    if read.is_err() || ended {
                        std::thread::sleep(std::time::Duration::from_millis(50)); // unplugged / reset: keep the last frame
                        continue;
                    }
                    let Some(bytes) = sample.as_ref().and_then(sample_bytes) else { continue };
                    let decoded = if mode.mjpeg {
                        convert::decode_mjpeg(&bytes)
                    } else {
                        convert::yuyv_to_rgba(mode.width as usize, mode.height as usize, &bytes).map(|r| (mode.width as usize, mode.height as usize, r))
                    };
                    if let Some((width, height, rgba)) = decoded {
                        seq += 1;
                        *l2.lock().unwrap() = Some(Arc::new(VideoFrame { width, height, rgba, seq }));
                    }
                }
            })
            .map_err(|e| CaptureError(e.to_string()))?;
        let mode = rx.recv().map_err(|_| CaptureError("capture thread exited".into()))??;
        let name = list_devices().into_iter().find(|d| d.path.eq_ignore_ascii_case(path)).map(|d| d.name).unwrap_or_default();
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
