//! Reading text off a captured frame with the operating system's own recogniser: Windows OCR (`Windows.Media.Ocr`)
//! on Windows, Apple's Vision on macOS, Tesseract (if installed) on Linux. Each returns the same [`TextLine`]s, so
//! workflows match phrases the same way everywhere; only English is asked for (the built-in workflows are English).

pub use kvmit_script::text::TextLine;
use kvmit_script::Frame;

/// The longest one reading may take. A recogniser that hangs must not hold a run, nor answer once its frame is old:
/// past this the reading is refused (its thread is left to finish on its own).
pub const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// Read every line of text on `frame`. An error says why the text cannot be read on this computer (no recogniser,
/// no English language pack, too slow), not that the screen has none: a screen without text gives an empty list.
pub fn read_text(frame: &Frame) -> Result<Vec<TextLine>, String> {
    // The recognisers copy `width * height` bytes: a frame whose pixels do not match its size is refused here.
    if frame.width == 0 || frame.height == 0 || frame.width.checked_mul(frame.height) != Some(frame.gray.len()) {
        return Err("the frame's size and its pixels disagree".into());
    }
    let f = frame.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("kvmit-read-text".into())
        .spawn(move || {
            let _ = tx.send(imp::read_text(&f));
        })
        .map_err(|e| format!("could not start reading the screen: {e}"))?;
    rx.recv_timeout(READ_TIMEOUT).unwrap_or_else(|_| Err(format!("reading the screen's text took longer than {} s", READ_TIMEOUT.as_secs())))
}

#[cfg(windows)]
mod imp {
    use super::{Frame, TextLine};
    use std::sync::OnceLock;
    use windows::core::HSTRING;
    use windows::Globalization::Language;
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;

    fn engine() -> Result<OcrEngine, String> {
        // Created once: the engine is thread-safe (agile) and creating one loads its language model.
        static ENGINE: OnceLock<Result<OcrEngine, String>> = OnceLock::new();
        ENGINE
            .get_or_init(|| {
                let lang = Language::CreateLanguage(&HSTRING::from("en-US")).map_err(|e| e.message())?;
                if OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
                    return OcrEngine::TryCreateFromLanguage(&lang).map_err(|e| e.message());
                }
                // Any English the user's profile has (en-GB, en-AU...) reads OOBE's English just as well.
                OcrEngine::TryCreateFromUserProfileLanguages()
                    .ok()
                    .filter(|e| e.RecognizerLanguage().and_then(|l| l.LanguageTag()).is_ok_and(|t| t.to_string().starts_with("en")))
                    .ok_or_else(|| {
                        "Windows has no English text recognition installed: add English under Settings > Time & language > \
                         Language & region (its optical character recognition is part of the basic language pack)"
                            .to_string()
                    })
            })
            .clone()
    }

    pub fn read_text(frame: &Frame) -> Result<Vec<TextLine>, String> {
        let engine = engine()?;
        let max = OcrEngine::MaxImageDimension().map_err(|e| e.message())? as usize;
        if frame.width > max || frame.height > max {
            return Err(format!("the frame ({}x{}) is larger than Windows OCR reads ({max} pixels)", frame.width, frame.height));
        }
        let ocr = || -> windows::core::Result<Vec<TextLine>> {
            let bitmap = SoftwareBitmap::Create(BitmapPixelFormat::Gray8, frame.width as i32, frame.height as i32)?;
            let writer = DataWriter::new()?;
            writer.WriteBytes(&frame.gray)?;
            bitmap.CopyFromBuffer(&writer.DetachBuffer()?)?;
            let result = engine.RecognizeAsync(&bitmap)?.get()?;
            let (fw, fh) = (frame.width as f64, frame.height as f64);
            let mut lines = Vec::new();
            for line in result.Lines()? {
                let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
                for word in line.Words()? {
                    let r = word.BoundingRect()?;
                    x0 = x0.min(r.X as f64);
                    y0 = y0.min(r.Y as f64);
                    x1 = x1.max((r.X + r.Width) as f64);
                    y1 = y1.max((r.Y + r.Height) as f64);
                }
                if x0 > x1 {
                    continue; // a line without words
                }
                lines.push(TextLine { text: line.Text()?.to_string(), rect: [x0 / fw, y0 / fh, (x1 - x0) / fw, (y1 - y0) / fh] });
            }
            Ok(lines)
        };
        ocr().map_err(|e| format!("Windows OCR failed: {}", e.message()))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Frame, TextLine};
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Tesseract reads a PGM from standard input and writes one TSV row per word; rows of one line share their
    /// block, paragraph and line numbers.
    pub fn read_text(frame: &Frame) -> Result<Vec<TextLine>, String> {
        let mut child = Command::new("tesseract")
            .args(["stdin", "stdout", "-l", "eng", "--psm", "11", "tsv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => {
                    "reading text needs Tesseract: install it (for example `sudo apt install tesseract-ocr`)".to_string()
                }
                _ => format!("could not start tesseract: {e}"),
            })?;
        let mut pgm = format!("P5\n{} {}\n255\n", frame.width, frame.height).into_bytes();
        pgm.extend_from_slice(&frame.gray);
        let mut stdin = child.stdin.take().expect("piped");
        let writer = std::thread::spawn(move || stdin.write_all(&pgm));
        let mut stdout = child.stdout.take().expect("piped");
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            std::io::Read::read_to_end(&mut stdout, &mut out).map(|_| out)
        });
        // Drained as it is written: warnings filling the pipe would otherwise stall tesseract until it is killed.
        let mut stderr = child.stderr.take().expect("piped");
        let errors = std::thread::spawn(move || {
            let mut err = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut err);
            err
        });
        // Killed if it outlives the reading's time limit, so a hung tesseract does not linger.
        let deadline = std::time::Instant::now() + super::READ_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(10)),
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("tesseract took too long".into());
                }
                Err(e) => return Err(format!("tesseract: {e}")),
            }
        };
        let _ = writer.join();
        let out = reader.join().map_err(|_| "tesseract: reading its output failed".to_string())?.map_err(|e| format!("tesseract: {e}"))?;
        let err = errors.join().unwrap_or_default();
        if !status.success() {
            return Err(format!("tesseract failed: {}", err.trim()));
        }
        Ok(parse_tsv(&String::from_utf8_lossy(&out), frame.width as f64, frame.height as f64))
    }

    /// A line being gathered from Tesseract's words: its words, and its box as (x0, y0, x1, y1) in pixels.
    type Gathered = (Vec<String>, f64, f64, f64, f64);

    pub(super) fn parse_tsv(tsv: &str, fw: f64, fh: f64) -> Vec<TextLine> {
        // key (block, par, line) -> the line so far, in the order lines first appear
        let mut lines: BTreeMap<(u32, u32, u32), Gathered> = BTreeMap::new();
        for row in tsv.lines().skip(1) {
            let c: Vec<&str> = row.split('\t').collect();
            if c.len() < 12 || c[0] != "5" || c[11].trim().is_empty() {
                continue;
            }
            let n = |i: usize| c[i].parse::<f64>().unwrap_or(0.0);
            let key = (n(2) as u32, n(3) as u32, n(4) as u32);
            let (l, t, w, h) = (n(6), n(7), n(8), n(9));
            let e = lines.entry(key).or_insert((Vec::new(), f64::MAX, f64::MAX, f64::MIN, f64::MIN));
            e.0.push(c[11].to_string());
            e.1 = e.1.min(l);
            e.2 = e.2.min(t);
            e.3 = e.3.max(l + w);
            e.4 = e.4.max(t + h);
        }
        lines
            .into_values()
            .map(|(words, x0, y0, x1, y1)| TextLine { text: words.join(" "), rect: [x0 / fw, y0 / fh, (x1 - x0) / fw, (y1 - y0) / fh] })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{Frame, TextLine};
    use objc2::AllocAnyThread;
    use objc2_core_foundation::CFRetained;
    use objc2_core_video::{
        kCVPixelFormatType_OneComponent8, CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddress,
        CVPixelBufferGetBytesPerRow, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    };
    use objc2_foundation::{NSArray, NSDictionary, NSString};
    use objc2_vision::{VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel};
    use std::ptr::NonNull;

    /// The frame copied into a one-channel 8-bit pixel buffer, row by row (the buffer's rows may be padded).
    fn pixel_buffer(frame: &Frame) -> Result<CFRetained<CVPixelBuffer>, String> {
        // Checked by `read_text` too; the copy below relies on it.
        if frame.width.checked_mul(frame.height) != Some(frame.gray.len()) || frame.width == 0 {
            return Err("the frame's size and its pixels disagree".into());
        }
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        // SAFETY: `out` is a valid place for the created buffer; no allocator or attributes are passed.
        let r = unsafe {
            CVPixelBufferCreate(None, frame.width, frame.height, kCVPixelFormatType_OneComponent8, None, NonNull::from(&mut out))
        };
        let buf = NonNull::new(out).filter(|_| r == 0).ok_or(format!("could not make a picture for Vision (CoreVideo {r})"))?;
        // SAFETY: created above with a +1 retain count, which `CFRetained` takes over.
        let buf = unsafe { CFRetained::from_raw(buf) };
        // SAFETY: locked for writing, written within its rows and row length, then unlocked.
        unsafe {
            if CVPixelBufferLockBaseAddress(&buf, CVPixelBufferLockFlags(0)) != 0 {
                return Err("could not write the picture for Vision".into());
            }
            let base = CVPixelBufferGetBaseAddress(&buf) as *mut u8;
            let stride = CVPixelBufferGetBytesPerRow(&buf);
            if !base.is_null() && stride >= frame.width {
                for (y, row) in frame.gray.chunks_exact(frame.width).enumerate() {
                    std::ptr::copy_nonoverlapping(row.as_ptr(), base.add(y * stride), frame.width);
                }
            }
            CVPixelBufferUnlockBaseAddress(&buf, CVPixelBufferLockFlags(0));
            if base.is_null() || stride < frame.width {
                return Err("the picture for Vision has no memory".into());
            }
        }
        Ok(buf)
    }

    pub fn read_text(frame: &Frame) -> Result<Vec<TextLine>, String> {
        let buf = pixel_buffer(frame)?;
        let request = VNRecognizeTextRequest::new();
        request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
        request.setRecognitionLanguages(&NSArray::from_retained_slice(&[NSString::from_str("en-US")]));
        // Setup screens are interface text, not prose: correcting words towards a dictionary would change names.
        request.setUsesLanguageCorrection(false);
        // SAFETY: an empty options dictionary is valid for any image request handler.
        let handler = unsafe { VNImageRequestHandler::initWithCVPixelBuffer_options(VNImageRequestHandler::alloc(), &buf, &NSDictionary::new()) };
        let requests: objc2::rc::Retained<NSArray<VNRequest>> = NSArray::from_slice(&[&**request]);
        handler.performRequests_error(&requests).map_err(|e| format!("Vision could not read the screen: {}", e.localizedDescription()))?;
        let mut lines = Vec::new();
        for obs in request.results().unwrap_or_default().iter() {
            let Some(top) = obs.topCandidates(1).firstObject() else { continue };
            // SAFETY: a recognised text observation always has a bounding box.
            let b = unsafe { obs.boundingBox() };
            // Vision's box is normalised with its origin at the bottom left; frames are top-left.
            let (x, w, h) = (b.origin.x, b.size.width, b.size.height);
            let y = 1.0 - b.origin.y - h;
            lines.push(TextLine { text: top.string().to_string(), rect: [x, y, w, h] });
        }
        Ok(lines)
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use super::{Frame, TextLine};

    pub fn read_text(_frame: &Frame) -> Result<Vec<TextLine>, String> {
        Err("reading text is not supported on this operating system".into())
    }
}

#[cfg(test)]
mod frame_tests {
    #[test]
    fn a_frame_whose_pixels_do_not_match_its_size_is_refused_before_any_reader() {
        let mut f = kvmit_script::Frame::new(4, 2, vec![0; 8]).unwrap();
        f.gray.clear();
        assert_eq!(super::read_text(&f).unwrap_err(), "the frame's size and its pixels disagree");
        f.width = 0;
        assert!(super::read_text(&f).is_err());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn tesseract_words_are_grouped_into_lines() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   5\t1\t1\t1\t1\t1\t100\t50\t40\t20\t96\tName\n\
                   5\t1\t1\t1\t1\t2\t150\t52\t40\t20\t95\tyour\n\
                   4\t1\t1\t1\t2\t0\t0\t0\t0\t0\t-1\t\n\
                   5\t1\t2\t1\t1\t1\t100\t300\t30\t20\t90\tSkip\n";
        let lines = super::imp::parse_tsv(tsv, 1000.0, 500.0);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Name your");
        assert_eq!(lines[0].rect, [0.1, 0.1, 0.09, 0.044]);
        assert_eq!(lines[1].text, "Skip");
    }
}
