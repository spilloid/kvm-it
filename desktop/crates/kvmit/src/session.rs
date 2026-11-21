//! Shared connection helpers (CLI and GUI).
use kvmit_ble::{backend, Device, LinkError};
use std::fmt;
use std::time::Duration;

pub type BoxErr = Box<dyn std::error::Error + Send + Sync>;

type Link = (Device, backend::Connection);

#[derive(Debug)]
pub enum LinkFailure {
    Bluetooth(backend::ConnectError),
    Handshake(LinkError),
}

impl LinkFailure {
    fn could_be_stale_bond(&self) -> bool {
        matches!(self, Self::Bluetooth(backend::ConnectError::Link(_))
            | Self::Handshake(LinkError::Closed | LinkError::Timeout))
    }
}

impl fmt::Display for LinkFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bluetooth(e) => e.fmt(f),
            Self::Handshake(e) => write!(f, "adapter handshake failed: {e}"),
        }
    }
}

impl std::error::Error for LinkFailure {}

pub async fn connect(id: &str) -> Result<Link, LinkFailure> {
    let mut access = backend::AdapterAccess::acquire(id)
        .map_err(|error| LinkFailure::Bluetooth(backend::ConnectError::Unavailable(error)))?;
    open_link(&mut access).await
}

async fn open_link(access: &mut backend::AdapterAccess) -> Result<Link, LinkFailure> {
    let mut conn = access.connect().await.map_err(LinkFailure::Bluetooth)?;
    let dev = match Device::connect(conn.take_io()).await {
        Ok(d) => d,
        Err(e) => {
            // `Connection` does not disconnect when dropped: close the link ourselves, or a failed handshake leaves the OS holding it
            conn.disconnect().await;
            return Err(LinkFailure::Handshake(e));
        }
    };
    Ok((dev, conn))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectMode {
    Connect,
    Repair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectStage {
    Closing,
    CheckingPairing,
    Pairing,
    Repairing,
    Connecting,
}

impl ConnectStage {
    pub fn status(self) -> &'static str {
        match self {
            Self::Closing => "closing",
            Self::CheckingPairing => "checking_pairing",
            Self::Pairing => "pairing",
            Self::Repairing => "repairing",
            Self::Connecting => "connecting",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Closing => "Closing the previous adapter connection…",
            Self::CheckingPairing => "Checking Bluetooth pairing…",
            Self::Pairing => "Pairing over Bluetooth…",
            Self::Repairing => "Removing this adapter’s saved pairing…",
            Self::Connecting => "Connecting to the adapter…",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ConnectFailure {
    pub stage: ConnectStage,
    pub message: String,
    pub repair_available: bool,
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConnectFailure {}

/// Explicit Connect can establish a missing bond. Only an explicit Repair may remove one.
pub async fn connect_adapter(
    id: &str,
    mode: ConnectMode,
    progress: impl Fn(ConnectStage),
) -> Result<Link, ConnectFailure> {
    let mut access = backend::AdapterAccess::acquire(id).map_err(|error| ConnectFailure {
        stage: ConnectStage::Connecting,
        message: error.to_string(),
        repair_available: false,
    })?;
    if mode == ConnectMode::Repair {
        progress(ConnectStage::Repairing);
        access.unpair().await.map_err(|error| ConnectFailure {
            stage: ConnectStage::Repairing,
            message: format!("Could not remove the saved Bluetooth pairing: {error}"),
            repair_available: false,
        })?;
    }

    let mut pairing_stage = ConnectStage::CheckingPairing;
    progress(pairing_stage);
    let pairing = access.pair(|| {
        pairing_stage = ConnectStage::Pairing;
        progress(pairing_stage);
    }).await.map_err(|error| ConnectFailure {
        stage: pairing_stage,
        message: format!("Bluetooth pairing could not be established: {error}. Tap BOOT once. When the LED flashes blue, try Connect again."),
        repair_available: false,
    })?;

    progress(ConnectStage::Connecting);
    let mut link = open_link(&mut access).await;
    if pairing == backend::Pairing::Established && link.as_ref().is_err_and(|error| {
        !matches!(error, LinkFailure::Bluetooth(backend::ConnectError::Incompatible(_))
            | LinkFailure::Handshake(LinkError::UnsupportedVersion { .. } | LinkError::Protocol(_)))
    }) {
        // Windows and other stacks may still be publishing the new bond to GATT discovery.
        tokio::time::sleep(Duration::from_millis(500)).await;
        link = open_link(&mut access).await;
    }
    link.map_err(|error| ConnectFailure {
        stage: ConnectStage::Connecting,
        message: format!("Could not open the adapter link: {error}"),
        repair_available: mode == ConnectMode::Connect && pairing == backend::Pairing::Existing
            && error.could_be_stale_bond(),
    })
}

/// Pick an adapter: the given id, else the configured last device, else the strongest one in range.
pub async fn resolve(id: Option<String>, last: Option<String>) -> Result<String, BoxErr> {
    if let Some(i) = id.or(last) {
        return Ok(i);
    }
    let found = backend::scan(Duration::from_secs(backend::DEFAULT_SCAN_SECS)).await?;
    match found.len() {
        0 => Err("no kvm-it adapter found. Plug it in (USB port to the target) and, to pair a new one run `kvmit pair` within 15 s of plugging it in (or after pressing BOOT briefly).".into()),
        _ => Ok(found[0].id.clone()),
    }
}

/// A first-time Connect with several boards nearby requires a device selection.
pub async fn resolve_for_connect(id: Option<String>, last: Option<String>) -> Result<String, BoxErr> {
    if let Some(id) = id.or(last) {
        return Ok(id);
    }
    let found = backend::scan(Duration::from_secs(backend::DEFAULT_SCAN_SECS)).await?;
    match found.as_slice() {
        [] => Err("No kvm-it adapters found. Power the adapter, briefly press BOOT and try again".into()),
        [board] => Ok(board.id.clone()),
        _ => Err("Several kvm-it adapters found. Run `kvmit scan`, then `kvmit --device <adapter-id> connect`".into()),
    }
}
