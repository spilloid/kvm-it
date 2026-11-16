//! Single local owner. The GUI/headless controller drains a bounded command queue.
use crate::{nonce, private_dir, private_file, ResourceClaim};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};

pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const REQUEST_TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    #[default]
    Observe,
    Workflow,
    Full,
}

#[derive(Serialize, Deserialize)]
struct Endpoint {
    address: SocketAddr,
    token: String,
}

#[derive(Serialize, Deserialize)]
struct WireRequest {
    token: String,
    tool: String,
    arguments: Value,
}

pub struct Command {
    pub tool: String,
    pub arguments: Value,
    pub expires: Instant,
    pub reply: mpsc::SyncSender<Result<Value, String>>,
}

pub struct Broker {
    pub incoming: mpsc::Receiver<Command>,
    pub access: Access,
    stop: Arc<AtomicBool>,
    path: PathBuf,
    _claim: ResourceClaim,
}

impl Broker {
    pub fn start(dir: &Path, access: Access) -> Result<Self, String> {
        private_dir(dir)?;
        let claim = ResourceClaim::at(dir, "controller", "owner")?;
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let endpoint = Endpoint {
            address: listener.local_addr().map_err(|e| e.to_string())?,
            token: nonce()?,
        };
        let path = dir.join("endpoint.json");
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        }
        let mut file = private_file(&path).map_err(|e| e.to_string())?;
        serde_json::to_writer(&mut file, &endpoint).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        let (tx, incoming) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::Builder::new().name("kvmit-control".into()).spawn(move || {
            // Serial acceptance bounds memory and worker count even for unauthenticated local traffic.
            while !stopped.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, peer)) if peer.ip().is_loopback() => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                        let result = (|| {
                            let bytes = read_message(&mut BufReader::new(stream.try_clone().map_err(|e| e.to_string())?))
                                .map_err(|e| format!("Could not read control request: {e}"))?;
                            let request: WireRequest = serde_json::from_slice(&bytes).map_err(|_| "Invalid control request".to_string())?;
                            // No HTTP parser, CORS or browser credentials. All calls require the per-owner token.
                            if request.token != endpoint.token { return Err("Unauthorized".into()); }
                            let (reply, rx) = mpsc::sync_channel(1);
                            tx.try_send(Command { tool: request.tool, arguments: request.arguments, expires: Instant::now() + REQUEST_TTL, reply })
                                .map_err(|_| "Controller is busy".to_string())?;
                            rx.recv_timeout(REQUEST_TTL + Duration::from_secs(1)).map_err(|_| "Controller did not reply; inspect an existing run before retrying".to_string())?
                        })();
                        if let Ok(mut bytes) = serde_json::to_vec(&result) { bytes.push(b'\n'); let _ = stream.write_all(&bytes); }
                    }
                    Ok(_) => {},
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                    Err(_) => break,
                }
            }
        }).map_err(|e| e.to_string())?;
        Ok(Self {
            incoming,
            access,
            stop,
            path,
            _claim: claim,
        })
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn call(dir: &Path, tool: &str, arguments: Value) -> Result<Value, String> {
    let bytes = std::fs::read(dir.join("endpoint.json"))
        .map_err(|_| "Open kvm-it and enable Agent access, or start `kvmit serve`".to_string())?;
    let endpoint: Endpoint =
        serde_json::from_slice(&bytes).map_err(|_| "Invalid controller endpoint".to_string())?;
    if !endpoint.address.ip().is_loopback() {
        return Err("Controller must be on loopback".into());
    }
    let mut stream = TcpStream::connect_timeout(&endpoint.address, Duration::from_secs(2))
        .map_err(|_| {
            "The controller is unavailable; enable Agent access in the running app".to_string()
        })?;
    stream
        .set_read_timeout(Some(REQUEST_TTL + Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let request = WireRequest {
        token: endpoint.token,
        tool: tool.into(),
        arguments,
    };
    // Serializing directly to a TCP stream produces many tiny writes. Nagle and
    // delayed ACKs can stall the terminating newline past the owner's read timeout.
    let mut message = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    message.push(b'\n');
    if message.len() > MAX_MESSAGE {
        return Err("Message exceeds 1 MiB".into());
    }
    stream.write_all(&message).map_err(|e| format!("Could not send control request: {e}"))?;
    let bytes = read_message(&mut BufReader::new(stream))
        .map_err(|e| format!("Could not read controller response: {e}"))?;
    serde_json::from_slice::<Result<Value, String>>(&bytes)
        .map_err(|_| "Invalid controller response".to_string())?
}

pub fn read_message(reader: &mut impl BufRead) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    loop {
        let buf = reader.fill_buf().map_err(|e| e.to_string())?;
        if buf.is_empty() {
            return if bytes.is_empty() {
                Ok(bytes)
            } else {
                Err("Truncated message".into())
            };
        }
        let end = buf
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(buf.len());
        if bytes.len() + end > MAX_MESSAGE {
            return Err("Message exceeds 1 MiB".into());
        }
        bytes.extend_from_slice(&buf[..end]);
        reader.consume(end);
        if bytes.last() == Some(&b'\n') {
            return Ok(bytes);
        }
    }
}
