//! End-to-end against a real ffmpeg. Skips (passes, with a note) when ffmpeg is not on PATH, e.g. in the
//! build container. Set KVMIT_REC_OUT=<dir> to keep the files for ffprobe; otherwise a temp dir is used.
use kvmit_record::{Format, PushOutcome, Recorder, Settings};
use std::path::PathBuf;
use std::time::Duration;

fn have_ffmpeg() -> bool {
    kvmit_record::check_ffmpeg(&Settings::gif()).is_ok()
}

fn out_dir(name: &str) -> PathBuf {
    let base = std::env::var_os("KVMIT_REC_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kvmit-record-e2e"));
    base.join(name)
}

/// Moving gradient + box, so the encoders have something to do.
fn synth(w: usize, h: usize, t: usize) -> Vec<u8> {
    let mut v = vec![255u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            v[i] = ((x + t * 7) % 256) as u8;
            v[i + 1] = (y * 255 / h) as u8;
            v[i + 2] = if (x / 16 + y / 16 + t).is_multiple_of(2) {
                200
            } else {
                40
            };
        }
    }
    v
}

fn record(
    settings: Settings,
    dir: &std::path::Path,
    frames: usize,
    change_size_at: Option<usize>,
) -> PathBuf {
    let fps = settings.fps as u64;
    let mut r = Recorder::start(settings, dir, 640, 360).expect("start");
    for t in 0..frames {
        let (w, h) = if change_size_at.is_some_and(|c| t >= c) {
            (320, 180)
        } else {
            (640, 360)
        };
        let out = r.push(w, h, &synth(w, h, t)).expect("push");
        assert!(matches!(out, PushOutcome::Queued(_)));
        std::thread::sleep(Duration::from_millis(1000 / fps));
    }
    let done = r.finish().expect("finish");
    assert!(done.frames >= frames as u64 - 2, "frames {}", done.frames);
    assert!(std::fs::metadata(&done.path).unwrap().len() > 100);
    done.path
}

#[test]
fn gif_and_webm_with_size_change() {
    if !have_ffmpeg() {
        eprintln!("SKIP: no ffmpeg on PATH");
        return;
    }
    let dir = out_dir("ok");
    let _ = std::fs::create_dir_all(&dir);
    let g = record(Settings::gif(), &dir, 36, Some(24));
    assert_eq!(g.extension().unwrap(), "gif");
    let w = record(Settings::webm(), &dir, 45, None);
    assert_eq!(w.extension().unwrap(), "webm");
    // Same-second recordings must not overwrite each other.
    let g2 = record(Settings::gif(), &dir, 6, None);
    assert_ne!(g, g2);
    assert_eq!(Settings::gif().format, Format::Gif);
}

#[cfg(unix)]
#[test]
fn dying_encoder_surfaces_error_and_never_hangs() {
    use std::os::unix::fs::PermissionsExt;
    let dir = out_dir("dying");
    let _ = std::fs::create_dir_all(&dir);
    let fake = dir.join("fake-ffmpeg.sh");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho 'boom: simulated encoder crash' >&2\nexit 3\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut s = Settings::gif();
    s.ffmpeg_path = Some(fake);
    let mut r = Recorder::start(s, &dir, 1280, 720).expect("start");
    let frame = vec![0u8; 1280 * 720 * 4];
    let t0 = std::time::Instant::now();
    let mut err = None;
    for _ in 0..200 {
        match r.push(1280, 720, &frame) {
            Ok(_) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                err = Some(e);
                break;
            }
        }
    }
    let e = err.expect("a dead encoder must surface as an error");
    assert!(e.to_string().contains("boom"), "{e}");
    assert!(t0.elapsed() < Duration::from_secs(8));
    r.abort();
}

/// Manual: records 2 s of WebM with Opus from the first non-monitor audio input. Run with
/// `cargo test -p kvmit-record -- --ignored` on a host that has ffmpeg and a microphone.
#[test]
#[ignore]
fn webm_with_audio_manual() {
    let devs = kvmit_record::list_audio_devices(None);
    let d = devs
        .iter()
        .find(|d| !d.monitor)
        .expect("no audio input found");
    eprintln!("using audio input: {}", d.description);
    let mut s = Settings::webm();
    s.audio = Some(d.as_input());
    let dir = out_dir("audio");
    let _ = std::fs::create_dir_all(&dir);
    record(s, &dir, 60, None);
}
