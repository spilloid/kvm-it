//! Reading text off a captured frame with the operating system's own recogniser: Windows OCR (`Windows.Media.Ocr`)
//! on Windows, Apple's Vision on macOS, Tesseract (if installed) on Linux. Each returns the same [`TextLine`]s, so
//! workflows match phrases the same way everywhere; only English is asked for (the built-in workflows are English).

pub use kvmit_script::text::TextLine;
use kvmit_script::Frame;

/// Read every line of text on `frame`. An error says why the text cannot be read on this computer (no recogniser,
/// no English language pack), not that the screen has none: a screen without text gives an empty list.
pub fn read_text(frame: &Frame) -> Result<Vec<TextLine>, String> {
    imp::read_text(frame)
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
        let out = child.wait_with_output().map_err(|e| format!("tesseract: {e}"))?;
        let _ = writer.join();
        if !out.status.success() {
            return Err(format!("tesseract failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(parse_tsv(&String::from_utf8_lossy(&out.stdout), frame.width as f64, frame.height as f64))
    }

    pub(super) fn parse_tsv(tsv: &str, fw: f64, fh: f64) -> Vec<TextLine> {
        // key (block, par, line) -> (words, x0, y0, x1, y1), in the order lines first appear
        let mut lines: BTreeMap<(u32, u32, u32), (Vec<String>, f64, f64, f64, f64)> = BTreeMap::new();
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

    pub fn read_text(_frame: &Frame) -> Result<Vec<TextLine>, String> {
        Err("reading text on macOS (Apple Vision) is not built yet".into())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
mod imp {
    use super::{Frame, TextLine};

    pub fn read_text(_frame: &Frame) -> Result<Vec<TextLine>, String> {
        Err("reading text is not supported on this operating system".into())
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
