use kvmit_hid::Key;
use kvmit_protocol::message::{error_code, parse_reply, HelloInfo, Reply, Request, StatusInfo};
use kvmit_protocol::{decode, VERSION};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

/// One reply or retry attempt waits this long before resending the same frame.
pub const RETRY_AFTER: Duration = Duration::from_millis(350);
pub const ATTEMPTS: usize = 4;
pub const KEEPALIVE_EVERY: Duration = Duration::from_secs(1);
pub const MOTION_FLUSH_EVERY: Duration = Duration::from_millis(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    /// No reply after all retries.
    Timeout,
    /// The link is gone.
    Closed,
    /// The device answered with an ERROR frame.
    Device { code: u8 },
    /// Unexpected/invalid reply.
    Protocol(String),
    UnsupportedVersion { device_major: u8 },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Timeout => write!(f, "no reply from the adapter (link degraded)"),
            LinkError::Closed => write!(f, "adapter link closed"),
            LinkError::Device { code } => write!(f, "adapter error {code}: {}", error_code::describe(*code)),
            LinkError::Protocol(s) => write!(f, "protocol error: {s}"),
            LinkError::UnsupportedVersion { device_major } => write!(f, "adapter speaks protocol v{device_major}"),
        }
    }
}
impl std::error::Error for LinkError {}

/// A pair of byte-frame channels. One frame per message, in each direction.
pub struct LinkIo {
    pub tx: mpsc::UnboundedSender<Vec<u8>>,
    pub rx: mpsc::UnboundedReceiver<Vec<u8>>,
}

type Pending = Arc<Mutex<HashMap<u8, oneshot::Sender<Reply>>>>;

struct Inner {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    seq: AtomicU8,
    pending: Pending,
    motion: Mutex<(i32, i32)>,
    closed: AtomicBool,
    closed_tx: watch::Sender<bool>,
}

impl Inner {
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.closed_tx.send(true);
        self.pending.lock().unwrap().clear(); // wakes waiters with Closed
    }

    async fn request(&self, req: &Request) -> Result<Reply, LinkError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(LinkError::Closed);
        }
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let bytes = req.encode(seq);
        if !req.expects_reply() {
            self.tx.send(bytes).map_err(|_| LinkError::Closed)?;
            return Ok(Reply::Ack);
        }
        let (tx, mut rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(seq, tx);
        // Retries reuse the same seq and bytes: the firmware replays its earlier answer instead of re-applying.
        let attempts = if req.wants_ack() { ATTEMPTS } else { 2 };
        for _ in 0..attempts {
            self.tx.send(bytes.clone()).map_err(|_| LinkError::Closed)?;
            match tokio::time::timeout(RETRY_AFTER, &mut rx).await {
                Ok(Ok(Reply::Error { code, .. })) => return Err(LinkError::Device { code }),
                Ok(Ok(r)) => return Ok(r),
                Ok(Err(_)) => return Err(LinkError::Closed),
                Err(_) => continue,
            }
        }
        self.pending.lock().unwrap().remove(&seq);
        Err(LinkError::Timeout)
    }
}

/// A connected, handshaken adapter. Cheap to clone; all clones share the link.
#[derive(Clone)]
pub struct Device {
    inner: Arc<Inner>,
    info: Arc<HelloInfo>,
    closed_rx: watch::Receiver<bool>,
}

impl Device {
    /// Handshake (HELLO) and clear any held input (RELEASE_ALL), then start keepalive and motion tasks.
    pub async fn connect(io: LinkIo) -> Result<Device, LinkError> {
        let LinkIo { tx, mut rx } = io;
        let pending: Pending = Arc::default();
        let (closed_tx, closed_rx) = watch::channel(false);
        let inner = Arc::new(Inner {
            tx,
            seq: AtomicU8::new(0),
            pending: pending.clone(),
            motion: Mutex::new((0, 0)),
            closed: AtomicBool::new(false),
            closed_tx,
        });

        let reader = inner.clone();
        tokio::spawn(async move {
            while let Some(bytes) = rx.recv().await {
                let Ok(frame) = decode(&bytes) else { continue };
                if let Ok(reply) = parse_reply(&frame) {
                    if let Some(w) = reader.pending.lock().unwrap().remove(&frame.seq) {
                        let _ = w.send(reply);
                    }
                }
            }
            reader.close();
        });

        let hello = inner.request(&Request::Hello { major: VERSION, minor: 0 }).await?;
        let info = match hello {
            Reply::Hello(h) if h.major == VERSION => h,
            Reply::Hello(h) => return Err(LinkError::UnsupportedVersion { device_major: h.major }),
            r => return Err(LinkError::Protocol(format!("expected HELLO reply, got {r:?}"))),
        };
        inner.request(&Request::ReleaseAll).await?;

        let ka = inner.clone();
        tokio::spawn(async move {
            let mut misses = 0;
            loop {
                tokio::time::sleep(KEEPALIVE_EVERY).await;
                if ka.closed.load(Ordering::SeqCst) {
                    return;
                }
                match ka.request(&Request::Ping(vec![])).await {
                    Ok(_) => misses = 0,
                    Err(_) => {
                        misses += 1;
                        if misses >= 3 {
                            ka.close();
                            return;
                        }
                    }
                }
            }
        });

        let mo = inner.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(MOTION_FLUSH_EVERY).await;
                if mo.closed.load(Ordering::SeqCst) {
                    return;
                }
                let (dx, dy) = {
                    let mut m = mo.motion.lock().unwrap();
                    let dx = m.0.clamp(i16::MIN as i32, i16::MAX as i32);
                    let dy = m.1.clamp(i16::MIN as i32, i16::MAX as i32);
                    *m = (m.0 - dx, m.1 - dy);
                    (dx as i16, dy as i16)
                };
                if dx != 0 || dy != 0 {
                    let _ = mo.request(&Request::MouseMove { dx, dy }).await;
                }
            }
        });

        Ok(Device { inner, info: Arc::new(info), closed_rx })
    }

    pub fn info(&self) -> &HelloInfo {
        &self.info
    }
    pub fn is_connected(&self) -> bool {
        !self.inner.closed.load(Ordering::SeqCst)
    }
    /// Resolves when the link is lost or closed.
    pub async fn closed(&self) {
        let mut rx = self.closed_rx.clone();
        while !*rx.borrow() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }

    async fn ack(&self, r: Request) -> Result<(), LinkError> {
        match self.inner.request(&r).await? {
            Reply::Ack => Ok(()),
            other => Err(LinkError::Protocol(format!("expected ack, got {other:?}"))),
        }
    }

    pub async fn key_down(&self, k: Key) -> Result<(), LinkError> {
        self.ack(Request::KeyDown(k.0)).await
    }
    pub async fn key_up(&self, k: Key) -> Result<(), LinkError> {
        self.ack(Request::KeyUp(k.0)).await
    }
    pub async fn key_tap(&self, k: Key) -> Result<(), LinkError> {
        self.ack(Request::KeyTap(k.0)).await
    }
    pub async fn button(&self, mask: u8, down: bool) -> Result<(), LinkError> {
        self.ack(if down { Request::ButtonDown(mask) } else { Request::ButtonUp(mask) }).await
    }
    pub async fn scroll(&self, v: i8, h: i8) -> Result<(), LinkError> {
        self.ack(Request::Scroll { v, h }).await
    }
    pub async fn release_all(&self) -> Result<(), LinkError> {
        self.ack(Request::ReleaseAll).await
    }
    pub async fn set_name(&self, name: &str) -> Result<(), LinkError> {
        if name.is_empty() || name.len() > 32 {
            return Err(LinkError::Protocol("name must be 1..=32 bytes".into()));
        }
        self.ack(Request::SetName(name.to_string())).await
    }
    pub async fn status(&self) -> Result<StatusInfo, LinkError> {
        match self.inner.request(&Request::Status).await? {
            Reply::Status(s) => Ok(s),
            r => Err(LinkError::Protocol(format!("expected status, got {r:?}"))),
        }
    }
    /// Round-trip time of one PING.
    pub async fn ping(&self) -> Result<Duration, LinkError> {
        let t = tokio::time::Instant::now();
        match self.inner.request(&Request::Ping(vec![1, 2, 3, 4])).await? {
            Reply::Pong(p) if p == [1, 2, 3, 4] => Ok(t.elapsed()),
            r => Err(LinkError::Protocol(format!("bad pong {r:?}"))),
        }
    }
    /// Relative motion; accumulated and flushed every few ms, never blocks, loss shows as lag not as a missed click.
    pub fn mouse_move(&self, dx: i32, dy: i32) {
        let mut m = self.inner.motion.lock().unwrap();
        m.0 = m.0.saturating_add(dx);
        m.1 = m.1.saturating_add(dy);
    }
    /// Best-effort release then close; used on capture exit, app close and abort.
    pub async fn shutdown(&self) {
        let _ = self.release_all().await;
        self.inner.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kvmit_protocol::{encode_vec, msg, Frame, FLAG_RESPONSE};

    /// Mock adapter: applies commands to a tiny HID model, dedups acked requests by (seq,type), can drop replies.
    #[derive(Default)]
    struct MockState {
        keys: Vec<u8>,
        key_downs_applied: usize,
        motion: (i32, i32),
        replies_to_drop: usize,
        seen_frames: usize,
        silent: bool,
        released: usize,
    }

    fn spawn_mock(state: Arc<Mutex<MockState>>) -> LinkIo {
        let (ctl_tx, mut dev_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (dev_tx, ctl_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            let mut dedup: HashMap<(u8, u8), Vec<u8>> = HashMap::new();
            while let Some(bytes) = dev_rx.recv().await {
                let f: Frame = match decode(&bytes) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let mut st = state.lock().unwrap();
                st.seen_frames += 1;
                if st.silent {
                    continue;
                }
                let reply: Option<Vec<u8>> = match f.msg_type {
                    msg::HELLO => {
                        let mut p = vec![1, 0, 7, 0, 0, 0, 0, 1, 0];
                        p.extend_from_slice(&[9; 16]);
                        p.push(4);
                        p.extend_from_slice(b"mock");
                        Some(encode_vec(msg::HELLO, FLAG_RESPONSE, f.seq, &p).unwrap())
                    }
                    msg::PING => Some(encode_vec(msg::PING, FLAG_RESPONSE, f.seq, f.payload).unwrap()),
                    msg::STATUS => Some(encode_vec(msg::STATUS, FLAG_RESPONSE, f.seq, &[1, st.keys.len() as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0]).unwrap()),
                    msg::MOUSE_MOVE => {
                        st.motion.0 += i16::from_le_bytes([f.payload[0], f.payload[1]]) as i32;
                        st.motion.1 += i16::from_le_bytes([f.payload[2], f.payload[3]]) as i32;
                        None
                    }
                    t => {
                        if let Some(r) = dedup.get(&(f.seq, t)) {
                            Some(r.clone())
                        } else {
                            let r = match t {
                                msg::KEY_DOWN => {
                                    st.key_downs_applied += 1;
                                    if f.payload[0] == 0x99 {
                                        encode_vec(msg::ERROR, FLAG_RESPONSE, f.seq, &[error_code::HID_NOT_MOUNTED, t]).unwrap()
                                    } else {
                                        st.keys.push(f.payload[0]);
                                        encode_vec(t, FLAG_RESPONSE, f.seq, &[]).unwrap()
                                    }
                                }
                                msg::KEY_UP => {
                                    st.keys.retain(|k| *k != f.payload[0]);
                                    encode_vec(t, FLAG_RESPONSE, f.seq, &[]).unwrap()
                                }
                                msg::RELEASE_ALL => {
                                    st.keys.clear();
                                    st.released += 1;
                                    encode_vec(t, FLAG_RESPONSE, f.seq, &[]).unwrap()
                                }
                                _ => encode_vec(msg::ERROR, FLAG_RESPONSE, f.seq, &[error_code::UNSUPPORTED, t]).unwrap(),
                            };
                            dedup.insert((f.seq, t), r.clone());
                            Some(r)
                        }
                    }
                };
                if let Some(r) = reply {
                    if st.replies_to_drop > 0 && f.msg_type != msg::HELLO {
                        st.replies_to_drop -= 1;
                    } else {
                        let _ = dev_tx.send(r);
                    }
                }
            }
        });
        LinkIo { tx: ctl_tx, rx: ctl_rx }
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_and_basic_keys() {
        let st = Arc::new(Mutex::new(MockState::default()));
        let dev = Device::connect(spawn_mock(st.clone())).await.unwrap();
        assert_eq!(dev.info().name, "mock");
        assert_eq!(st.lock().unwrap().released, 1, "connect sends RELEASE_ALL");
        dev.key_down(Key::A).await.unwrap();
        assert_eq!(st.lock().unwrap().keys, vec![4]);
        assert_eq!(dev.status().await.unwrap().keys, 1);
        dev.key_up(Key::A).await.unwrap();
        assert!(st.lock().unwrap().keys.is_empty());
        assert!(dev.ping().await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn lost_reply_is_retried_without_double_applying() {
        let st = Arc::new(Mutex::new(MockState::default()));
        let dev = Device::connect(spawn_mock(st.clone())).await.unwrap();
        st.lock().unwrap().replies_to_drop = 2;
        dev.key_down(Key::A).await.unwrap();
        let s = st.lock().unwrap();
        assert_eq!(s.key_downs_applied, 1, "retries replay the answer, never re-apply");
        assert_eq!(s.keys, vec![4]);
    }

    #[tokio::test(start_paused = true)]
    async fn device_error_surfaces_with_its_code() {
        let st = Arc::new(Mutex::new(MockState::default()));
        let dev = Device::connect(spawn_mock(st)).await.unwrap();
        let e = dev.key_down(Key(0x99)).await.unwrap_err();
        assert_eq!(e, LinkError::Device { code: error_code::HID_NOT_MOUNTED });
        assert!(e.to_string().contains("target computer"));
    }

    #[tokio::test(start_paused = true)]
    async fn silent_device_times_out_then_link_is_declared_dead() {
        let st = Arc::new(Mutex::new(MockState::default()));
        let dev = Device::connect(spawn_mock(st.clone())).await.unwrap();
        st.lock().unwrap().silent = true;
        assert_eq!(dev.key_down(Key::A).await.unwrap_err(), LinkError::Timeout);
        tokio::time::timeout(Duration::from_secs(30), dev.closed()).await.expect("keepalive must notice a dead link");
        assert!(!dev.is_connected());
        assert_eq!(dev.key_up(Key::A).await.unwrap_err(), LinkError::Closed);
    }

    #[tokio::test(start_paused = true)]
    async fn motion_is_accumulated_and_clamped() {
        let st = Arc::new(Mutex::new(MockState::default()));
        let dev = Device::connect(spawn_mock(st.clone())).await.unwrap();
        for _ in 0..10 {
            dev.mouse_move(3, -2);
        }
        dev.mouse_move(40_000, 0);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(st.lock().unwrap().motion, (40_030, -20));
    }

    #[tokio::test(start_paused = true)]
    async fn wrong_protocol_major_is_rejected() {
        let (ctl_tx, mut dev_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (dev_tx, ctl_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        tokio::spawn(async move {
            while let Some(b) = dev_rx.recv().await {
                let f = decode(&b).unwrap();
                let mut p = vec![9, 0, 0, 0, 0, 0, 0, 0, 0];
                p.extend_from_slice(&[0; 16]);
                p.push(0);
                let _ = dev_tx.send(encode_vec(msg::HELLO, FLAG_RESPONSE, f.seq, &p).unwrap());
            }
        });
        let e = Device::connect(LinkIo { tx: ctl_tx, rx: ctl_rx }).await.err().unwrap();
        assert_eq!(e, LinkError::UnsupportedVersion { device_major: 9 });
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_releases_then_closes() {
        let st = Arc::new(Mutex::new(MockState::default()));
        let dev = Device::connect(spawn_mock(st.clone())).await.unwrap();
        dev.key_down(Key::LEFT_SHIFT).await.unwrap();
        dev.shutdown().await;
        assert!(st.lock().unwrap().keys.is_empty());
        assert!(!dev.is_connected());
    }
}
