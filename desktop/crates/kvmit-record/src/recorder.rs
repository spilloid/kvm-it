//! The subprocess glue. Frames go to a bounded queue drained by a writer thread that owns ffmpeg's stdin, so a
//! slow or dead encoder can never block the caller (frames are dropped and counted instead), and `finish` kills
//! a stuck ffmpeg after a timeout so it cannot hang either.
use crate::args::{build_args, Format, Settings};
use crate::naming::{stamp_now, unique_path};
use crate::pacer::Pacer;
use crate::resize_rgba_nearest;
use kvmit_video::SharedFrame;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Frames queued between the caller and the writer thread.
const QUEUE_FRAMES: usize = 6;
/// How long `finish` waits for the encoder (GIF palettegen runs at end of input) before killing it.
const FINISH_TIMEOUT: Duration = Duration::from_secs(120);
const STDERR_KEEP: usize = 2000;

#[derive(Debug)]
pub enum RecordError {
    /// ffmpeg could not be started because it is not installed / not on PATH.
    FfmpegNotFound(String),
    Io(String),
    /// ffmpeg exited (or its pipe broke) before the recording ended; carries its stderr tail.
    EncoderFailed(String),
    /// `finish` had to kill an encoder that did not exit in time.
    EncoderTimeout,
    /// Frame data did not match its stated size, or a zero-sized frame.
    BadFrame,
    /// A poll recording ended without ever seeing a frame.
    NoFrames,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecordError::FfmpegNotFound(p) => write!(
                f,
                "ffmpeg was not found ({p}). Install ffmpeg and make sure it is on PATH to record."
            ),
            RecordError::Io(e) => write!(f, "recording I/O error: {e}"),
            RecordError::EncoderFailed(e) => write!(f, "ffmpeg stopped: {e}"),
            RecordError::EncoderTimeout => {
                write!(f, "ffmpeg did not finish in time and was stopped")
            }
            RecordError::BadFrame => write!(f, "invalid video frame"),
            RecordError::NoFrames => write!(f, "no video frames arrived, nothing recorded"),
        }
    }
}

impl std::error::Error for RecordError {}

fn ffmpeg_program(s: &Settings) -> PathBuf {
    s.ffmpeg_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("ffmpeg"))
}

/// Create `dir/kvmit-<stamp>[-N].<ext>` empty, atomically (`create_new`), so two recordings (threads or processes) can never
/// hold the same name. It stays an empty placeholder until the finished file is renamed over it.
fn reserve_path(dir: &Path, format: Format) -> Result<PathBuf, RecordError> {
    for _ in 0..1000 {
        let p = unique_path(dir, &stamp_now(), format, |p| p.exists());
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(_) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(RecordError::Io(format!("{}: {e}", p.display()))),
        }
    }
    Err(RecordError::Io("could not reserve a free file name".into()))
}

/// `dir/.<pid>-<final name>.part`.
fn part_path(final_path: &Path) -> PathBuf {
    let name = final_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    final_path.with_file_name(format!(".{}-{name}.part", std::process::id()))
}

fn command(prog: &Path) -> Command {
    let c = Command::new(prog);
    #[cfg(windows)]
    let mut c = c;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn not_found_or_io(e: std::io::Error, prog: &Path) -> RecordError {
    if e.kind() == std::io::ErrorKind::NotFound {
        RecordError::FfmpegNotFound(prog.display().to_string())
    } else {
        RecordError::Io(e.to_string())
    }
}

/// Run `ffmpeg -version`; returns its first line, or a clear error when ffmpeg is missing.
pub fn check_ffmpeg(settings: &Settings) -> Result<String, RecordError> {
    let prog = ffmpeg_program(settings);
    let out = command(&prog)
        .arg("-version")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| not_found_or_io(e, &prog))?;
    if !out.status.success() {
        return Err(RecordError::EncoderFailed(
            "`ffmpeg -version` failed".into(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

#[derive(Debug, Clone)]
pub struct Finished {
    pub path: PathBuf,
    /// Frames handed to the encoder at the target rate (duplicates for gaps included).
    pub frames: u64,
    /// Frames lost because the encoder could not keep up.
    pub dropped: u64,
    pub duration: Duration,
}

/// Result of one [`Recorder::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// Frame queued; carries how many copies were due for the elapsed time (0: arrived early, skipped).
    Queued(u32),
    /// The duration cap was reached; further frames are ignored. Call `finish`.
    CapReached,
}

#[derive(Default)]
struct Shared {
    failed: Mutex<Option<String>>,
    stderr_tail: Mutex<String>,
}

pub struct Recorder {
    settings: Settings,
    /// The name the finished file is published under.
    path: PathBuf,
    /// What ffmpeg actually writes (`.<pid>-<name>.part` beside it): only this recording ever owns it, so cleanup can never
    /// delete another recording, and a half-written file never carries the final name.
    part: PathBuf,
    size: (usize, usize),
    child: Child,
    tx: Option<SyncSender<Arc<Vec<u8>>>>,
    writer: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    shared: Arc<Shared>,
    pacer: Pacer,
    start: Instant,
    dropped: u64,
    /// Frame slots lost to a full queue, repaid (as repeats) when there is room again, so playback stays real-time.
    owed: u64,
    /// The picture most recently queued, kept to repay owed slots when finishing.
    last: Option<Arc<Vec<u8>>>,
    /// Set from outside (quitting) to make `finish` kill the encoder and clean up instead of waiting.
    cancel: Arc<AtomicBool>,
    /// Frames actually handed to the encoder.
    written: u64,
}

impl Recorder {
    /// Start ffmpeg for frames of `width` x `height`; output is a new timestamped file under `dir` (created if
    /// needed, never overwriting). Later frames of another size are rescaled to this size.
    pub fn start(
        settings: Settings,
        dir: &Path,
        width: usize,
        height: usize,
    ) -> Result<Recorder, RecordError> {
        if width == 0 || height == 0 {
            return Err(RecordError::BadFrame);
        }
        std::fs::create_dir_all(dir).map_err(|e| RecordError::Io(e.to_string()))?;
        let path = reserve_path(dir, settings.format)?;
        let part = part_path(&path);
        let prog = ffmpeg_program(&settings);
        let mut child = match command(&prog)
            .args(build_args(&settings, width as u32, height as u32, &part))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_file(&path); // our own placeholder
                return Err(not_found_or_io(e, &prog));
            }
        };
        let shared = Arc::new(Shared::default());

        let mut stderr = child.stderr.take().expect("piped");
        let sh = shared.clone();
        let stderr_reader = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = stderr.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut t = sh.stderr_tail.lock().unwrap();
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                if t.len() > STDERR_KEEP {
                    let mut cut = t.len() - STDERR_KEEP;
                    while !t.is_char_boundary(cut) {
                        cut += 1;
                    }
                    t.drain(..cut);
                }
            }
        });

        let mut stdin = child.stdin.take().expect("piped");
        let (tx, rx) = sync_channel::<Arc<Vec<u8>>>(QUEUE_FRAMES);
        let sh = shared.clone();
        let writer = std::thread::spawn(move || {
            while let Ok(frame) = rx.recv() {
                if let Err(e) = stdin.write_all(&frame) {
                    *sh.failed.lock().unwrap() = Some(format!("pipe to ffmpeg closed ({e})"));
                    return; // rx dropped: further sends report Disconnected
                }
            }
            let _ = stdin.flush(); // stdin dropped here: ffmpeg sees EOF and finalises
        });

        Ok(Recorder {
            pacer: Pacer::new(settings.fps),
            settings,
            path,
            part,
            size: (width, height),
            child,
            tx: Some(tx),
            writer: Some(writer),
            stderr_reader: Some(stderr_reader),
            shared,
            start: Instant::now(),
            dropped: 0,
            owed: 0,
            last: None,
            cancel: Arc::new(AtomicBool::new(false)),
            written: 0,
        })
    }

    /// A flag that, once set, makes `finish` kill the encoder and remove the files instead of waiting for it.
    pub fn canceller(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    fn failure(&mut self) -> Option<RecordError> {
        if let Some(msg) = self.shared.failed.lock().unwrap().clone() {
            return Some(RecordError::EncoderFailed(self.compose(&msg)));
        }
        if let Ok(Some(st)) = self.child.try_wait() {
            // Exited while we are still sending: ffmpeg only does that on error.
            return Some(RecordError::EncoderFailed(
                self.compose(&format!("exited early ({st})")),
            ));
        }
        None
    }

    fn compose(&self, head: &str) -> String {
        // Give the stderr reader a moment to drain after a death.
        std::thread::sleep(Duration::from_millis(50));
        let tail = self.shared.stderr_tail.lock().unwrap().trim().to_string();
        if tail.is_empty() {
            head.to_string()
        } else {
            format!("{head}: {tail}")
        }
    }

    /// Offer one frame, timestamped by the wall clock since `start`. Never blocks on the encoder: when the
    /// queue is full the copy is dropped and counted. An `Err` is permanent; call `abort`.
    pub fn push(
        &mut self,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) -> Result<PushOutcome, RecordError> {
        if width == 0 || height == 0 || rgba.len() < width * height * 4 {
            return Err(RecordError::BadFrame);
        }
        if let Some(e) = self.failure() {
            return Err(e);
        }
        let n = self
            .pacer
            .frames_due_capped(self.start.elapsed(), self.settings.max_frames());
        if n == 0 {
            let capped = self
                .settings
                .max_frames()
                .is_some_and(|m| self.pacer.emitted() >= m);
            return Ok(if capped {
                PushOutcome::CapReached
            } else {
                PushOutcome::Queued(0)
            });
        }
        let data = if (width, height) == self.size {
            rgba[..width * height * 4].to_vec()
        } else {
            resize_rgba_nearest(rgba, width, height, self.size.0, self.size.1)
        };
        let data = Arc::new(data);
        let tx = self.tx.as_ref().expect("not finished");
        // Slots lost earlier are repaid now (bounded: a long stall must not become a burst), so the clip keeps real time.
        let repay = self.owed.min(u64::from(self.settings.fps.max(1)) * 2);
        self.owed -= repay;
        self.last = Some(data.clone());
        for i in 0..(u64::from(n) + repay) {
            match tx.try_send(data.clone()) {
                Ok(()) => self.written += 1,
                Err(TrySendError::Full(_)) => {
                    if i < u64::from(n) {
                        self.dropped += 1; // a new slot lost; a failed repayment is the same loss, not a new one
                    }
                    self.owed += 1;
                }
                Err(TrySendError::Disconnected(_)) => {
                    return Err(self
                        .failure()
                        .unwrap_or_else(|| RecordError::EncoderFailed("writer stopped".into())));
                }
            }
        }
        Ok(PushOutcome::Queued(n))
    }

    /// Housekeeping that must not depend on frames arriving (the capture may be gone): is the encoder still alive,
    /// and has the duration cap run out by the wall clock?
    pub fn check(&mut self) -> Result<PushOutcome, RecordError> {
        if let Some(e) = self.failure() {
            return Err(e);
        }
        let capped = self
            .settings
            .max_duration
            .is_some_and(|d| self.start.elapsed() >= d);
        Ok(if capped { PushOutcome::CapReached } else { PushOutcome::Queued(0) })
    }

    /// True once the duration cap has been used up.
    pub fn is_capped(&self) -> bool {
        self.settings
            .max_frames()
            .is_some_and(|m| self.pacer.emitted() >= m)
    }

    fn shutdown_threads(&mut self) {
        if let Some(w) = self.writer.take() {
            let _ = w.join();
        }
        if let Some(r) = self.stderr_reader.take() {
            let _ = r.join();
        }
    }

    /// Repay slots still owed (a stall near the end must not shorten the clip), waiting a bounded time for queue room.
    fn flush_owed(&mut self) {
        let (Some(tx), Some(last)) = (self.tx.as_ref(), self.last.clone()) else { return };
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.owed > 0 && Instant::now() < deadline {
            match tx.try_send(last.clone()) {
                Ok(()) => {
                    self.owed -= 1;
                    self.written += 1;
                }
                Err(TrySendError::Full(_)) => std::thread::sleep(Duration::from_millis(10)),
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    }

    /// Close the input and wait for ffmpeg to finalise the file. On any failure the partial file is removed.
    pub fn finish(mut self) -> Result<Finished, RecordError> {
        self.flush_owed();
        let frames = self.written;
        self.tx.take(); // writer drains the queue, then closes ffmpeg's stdin
        let deadline = Instant::now() + FINISH_TIMEOUT;
        let status = loop {
            match self.child.try_wait() {
                Ok(Some(st)) => break Some(st),
                Ok(None) if Instant::now() < deadline && !self.cancel.load(Ordering::SeqCst) => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Ok(None) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break None; // killing also unblocks a writer stuck on a full pipe (timeout or cancel)
                }
                Err(e) => return Err(self.fail_cleanup(RecordError::Io(e.to_string()))),
            }
        };
        self.shutdown_threads();
        let tail = self.shared.stderr_tail.lock().unwrap().trim().to_string();
        let write_err = self.shared.failed.lock().unwrap().clone();
        match status {
            None if self.cancel.load(Ordering::SeqCst) => Err(self.fail_cleanup(RecordError::Io("cancelled while quitting".into()))),
            None => Err(self.fail_cleanup(RecordError::EncoderTimeout)),
            Some(st) if !st.success() || write_err.is_some() => {
                let head = write_err.unwrap_or_else(|| format!("exited with {st}"));
                let msg = if tail.is_empty() {
                    head
                } else {
                    format!("{head}: {tail}")
                };
                Err(self.fail_cleanup(RecordError::EncoderFailed(msg)))
            }
            Some(_) => {
                self.publish()?;
                Ok(Finished {
                path: self.path.clone(),
                frames,
                dropped: self.dropped,
                duration: Duration::from_secs_f64(frames as f64 / self.settings.fps.max(1) as f64),
                })
            }
        }
    }

    /// Give the finished part file its final name. The name was reserved (created, empty) by this recording at start, so the
    /// rename replaces only our own placeholder: it is atomic, never touches anyone else's file, and needs no hard links.
    fn publish(&mut self) -> Result<(), RecordError> {
        std::fs::rename(&self.part, &self.path).map_err(|e| self.fail_cleanup(RecordError::Io(e.to_string())))
    }

    /// Remove only this recording's own part file.
    fn fail_cleanup(&self, e: RecordError) -> RecordError {
        self.remove_own_files();
        e
    }

    /// The part file and the reserved (still empty) final name: both are this recording's alone.
    fn remove_own_files(&self) {
        let _ = std::fs::remove_file(&self.part);
        let _ = std::fs::remove_file(&self.path);
    }

    /// Stop immediately and delete the partial file.
    pub fn abort(mut self) {
        self.tx.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.shutdown_threads();
        self.remove_own_files();
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Dropped without finish/abort: do not leave an ffmpeg behind.
        if self.tx.take().is_some() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.remove_own_files();
        }
    }
}

/// Gives up a recording: the encoder is killed and the files removed instead of waiting for it. For quitting only.
#[derive(Clone)]
pub struct Canceller {
    stop: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<Arc<AtomicBool>>>>,
}

impl Canceller {
    pub fn cancel(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(c) = self.slot.lock().unwrap().as_ref() {
            c.store(true, Ordering::SeqCst);
        }
    }
}

/// A recording driven by a polling thread: see [`poll_record`].
pub struct PollHandle {
    stop: Arc<AtomicBool>,
    /// Set by the worker once it has begun finalising the file (after stop, the cap, or the picture going away).
    finalising: Arc<AtomicBool>,
    cancel: Arc<Mutex<Option<Arc<AtomicBool>>>>,
    thread: Option<JoinHandle<Result<Finished, RecordError>>>,
}

impl PollHandle {
    /// False once the thread has ended on its own (cap reached or encoder failure); `stop` then reports why.
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    /// True once the file is being finalised: from here the clip is over, only the encoding remains.
    pub fn is_finalising(&self) -> bool {
        self.finalising.load(Ordering::SeqCst)
    }

    /// A handle that can give up a finalisation in progress (for quitting), usable after `stop` has taken this one.
    pub fn canceller(&self) -> Canceller {
        Canceller { stop: self.stop.clone(), slot: self.cancel.clone() }
    }

    /// Stop recording, finalise the file and return the outcome.
    pub fn stop(mut self) -> Result<Finished, RecordError> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread
            .take()
            .expect("joined once")
            .join()
            .unwrap_or_else(|_| Err(RecordError::Io("recording thread panicked".into())))
    }
}

impl Drop for PollHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Record whatever `source` returns (e.g. `|| capture.latest()`), sampled at `settings.fps` on its own thread,
/// into a new file under `dir`. ffmpeg's presence is checked up front so a missing install fails here, not on
/// the thread. The thread waits for the first frame (the output size comes from it), then ticks until stopped,
/// capped, or the encoder dies.
/// A recording whose source has delivered no frame for this long is finished with what it has.
const NO_FRAME_LIMIT: Duration = Duration::from_secs(10);

pub fn poll_record<F>(
    settings: Settings,
    dir: &Path,
    mut source: F,
) -> Result<PollHandle, RecordError>
where
    F: FnMut() -> Option<SharedFrame> + Send + 'static,
{
    check_ffmpeg(&settings)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let finalising = Arc::new(AtomicBool::new(false));
    let fin = finalising.clone();
    let cancel_slot: Arc<Mutex<Option<Arc<AtomicBool>>>> = Arc::default();
    let slot = cancel_slot.clone();
    let dir = dir.to_path_buf();
    let thread = std::thread::spawn(move || {
        let mut rec: Option<Recorder> = None;
        let interval = Pacer::new(settings.fps).interval();
        let mut next = Instant::now();
        let mut last_frame = Instant::now();
        let outcome = loop {
            if flag.load(Ordering::SeqCst) {
                break Ok(());
            }
            if let Some(f) = source() {
                last_frame = Instant::now();
                if rec.is_none() {
                    match Recorder::start(settings.clone(), &dir, f.width, f.height) {
                        Ok(r) => {
                            *slot.lock().unwrap() = Some(r.canceller());
                            rec = Some(r)
                        }
                        Err(e) => return Err(e),
                    }
                }
                let r = rec.as_mut().unwrap();
                match r.push(f.width, f.height, &f.rgba) {
                    Ok(PushOutcome::CapReached) => break Ok(()),
                    Ok(PushOutcome::Queued(_)) => {}
                    Err(e) => break Err(e),
                }
            } else if rec.is_none() {
                if last_frame.elapsed() >= NO_FRAME_LIMIT {
                    break Err(RecordError::NoFrames); // the picture never came (or went before the first frame)
                }
            } else if let Some(r) = rec.as_mut() {
                // No picture (the capture went away): the cap and the encoder's health still count, and a source
                // that stays gone ends the clip with what exists rather than recording on forever.
                match r.check() {
                    Ok(PushOutcome::CapReached) => break Ok(()),
                    Ok(_) => {}
                    Err(e) => break Err(e),
                }
                if last_frame.elapsed() >= NO_FRAME_LIMIT {
                    break Ok(());
                }
            }
            next += interval;
            let now = Instant::now();
            if next > now {
                std::thread::sleep((next - now).min(interval));
            } else if now - next > interval * 4 {
                next = now; // fell far behind: do not spin to catch up
            }
        };
        fin.store(true, Ordering::SeqCst);
        match (outcome, rec) {
            (Ok(()), Some(r)) => r.finish(),
            (Ok(()), None) => Err(RecordError::NoFrames),
            (Err(e), Some(r)) => {
                r.abort();
                Err(e)
            }
            (Err(e), None) => Err(e),
        }
    });
    Ok(PollHandle {
        stop,
        finalising,
        cancel: cancel_slot,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_part_file_is_hidden_owned_by_this_process_and_never_the_final_name() {
        let part = part_path(Path::new("/v/kvmit-20261006-120000.gif"));
        let name = part.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(part.parent().unwrap(), Path::new("/v"));
        assert!(name.starts_with('.') && name.ends_with(".part") && name.contains(&std::process::id().to_string()), "{name}");
    }

    #[test]
    fn missing_ffmpeg_is_a_clear_error() {
        let mut s = Settings::gif();
        s.ffmpeg_path = Some(PathBuf::from("/nonexistent/definitely-not-ffmpeg"));
        let e = check_ffmpeg(&s).unwrap_err();
        assert!(matches!(e, RecordError::FfmpegNotFound(_)));
        assert!(e.to_string().contains("Install ffmpeg"));
        let dir = std::env::temp_dir().join("kvmit-record-test-missing");
        assert!(matches!(
            Recorder::start(s.clone(), &dir, 4, 4),
            Err(RecordError::FfmpegNotFound(_))
        ));
        assert!(matches!(
            poll_record(s, &dir, || None),
            Err(RecordError::FfmpegNotFound(_))
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_frame_rejected() {
        assert!(matches!(
            Recorder::start(Settings::gif(), Path::new("/tmp"), 0, 4),
            Err(RecordError::BadFrame)
        ));
    }
}
