//! BLE transport: scan, connect, GATT (btleplug: Linux, Windows, macOS) and pairing (BlueZ via bluer, WinRT, and on macOS
//! CoreBluetooth's own pairing on first encrypted access).
//!
//! Adapter ids: the Bluetooth address on Linux and Windows. macOS never reveals addresses; there the id is the UUID
//! CoreBluetooth assigns the peripheral, stable on that Mac (so a saved id does not carry over to another Mac).
//! Hardware-dependent; not unit-testable. Protocol logic lives in `client`.
use crate::client::LinkIo;
use crate::diag;
use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use std::sync::Arc;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

pub const SERVICE_UUID: Uuid = Uuid::from_u128(0x7a1e0000_4b49_4d54_8000_6b766d697401);
pub const RX_UUID: Uuid = Uuid::from_u128(0x7a1e0001_4b49_4d54_8000_6b766d697401); // controller -> device (write)
pub const TX_UUID: Uuid = Uuid::from_u128(0x7a1e0002_4b49_4d54_8000_6b766d697401); // device -> controller (notify)

#[cfg(windows)]
static WINDOWS_PAIRING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug)]
pub struct BackendError(pub String);
impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for BackendError {}
impl From<btleplug::Error> for BackendError {
    fn from(e: btleplug::Error) -> Self {
        BackendError(e.to_string())
    }
}
#[cfg(target_os = "linux")]
impl From<bluer::Error> for BackendError {
    fn from(e: bluer::Error) -> Self {
        BackendError(e.to_string())
    }
}
type Result<T> = std::result::Result<T, BackendError>;

#[derive(Debug)]
pub enum ConnectError {
    Unavailable(BackendError),
    Link(BackendError),
    Incompatible(BackendError),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(e) | Self::Link(e) | Self::Incompatible(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for ConnectError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pairing {
    Existing,
    Established,
}

/// The longest connecting, discovering the services and subscribing may take, once the adapter has been found.
pub const GATT_SETUP_TIMEOUT: Duration = Duration::from_secs(15);

const DISCONNECT_TIMEOUT: Duration = Duration::from_secs(5);
type LinkClaims = (Arc<kvmit_control::ResourceClaim>, Arc<kvmit_control::ResourceClaim>);

#[cfg(any(target_os = "macos", test))]
async fn close_macos(disconnect: impl std::future::Future, _claims: LinkClaims) {
    // btleplug waits for a CoreBluetooth callback even if it has already removed
    // the peripheral. Time out the operation that owns the claims, so the next
    // attempt can obtain them. Each attempt has its own CoreBluetooth manager.
    let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, disconnect).await;
}

#[cfg(target_os = "macos")]
async fn close_quietly(p: &Peripheral, claims: LinkClaims) {
    close_macos(p.disconnect(), claims).await;
}

/// A backend that blocks its thread (such as WinRT waiting on a service lock)
/// needs a separate thread to bound the caller's wait. It retains ownership
/// until disconnect returns.
#[cfg(not(target_os = "macos"))]
async fn close_quietly(p: &Peripheral, claims: LinkClaims) {
    let p = p.clone();
    // On a thread of its own, outside the runtime's workers and its blocking pool: a backend that blocks there holds up
    // neither other tasks nor the runtime's shutdown (which waits for its blocking pool). The thread drives the future
    // with a small runtime of its own; if it never finishes, it is abandoned and ends with the process.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let started = std::thread::Builder::new().name("kvmit-ble-disconnect".into()).spawn(move || {
        let _claims = claims;
        if let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() {
            let _ = rt.block_on(p.disconnect());
        }
        let _ = tx.send(());
    });
    if started.is_ok() {
        let _ = tokio::time::timeout(DISCONNECT_TIMEOUT, rx).await;
    }
}

fn err<T>(s: impl Into<String>) -> Result<T> {
    Err(BackendError(s.into()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The stable id used by connect/pair: the Bluetooth address ("AA:BB:CC:DD:EE:FF"), or on macOS the peripheral's
    /// CoreBluetooth UUID.
    pub id: String,
    pub name: String,
    pub rssi: Option<i16>,
}

/// Use the same representation for a discovered or manually supplied address.
pub fn canonical_id(id: &str) -> String {
    #[cfg(any(windows, target_os = "linux"))]
    if let Ok(address) = parse_address(id) {
        return address.to_string();
    }
    id.to_owned()
}

#[cfg(any(windows, target_os = "linux"))]
fn parse_address(id: &str) -> Result<btleplug::api::BDAddr> {
    if !id.is_ascii() {
        return err(format!("bad Bluetooth address {id}"));
    }
    id.parse().map_err(|_| BackendError(format!("bad Bluetooth address {id}")))
}

/// Keep the same claims from pairing or repair through the GATT handshake.
pub struct AdapterAccess {
    id: String,
    adapter: std::sync::Arc<kvmit_control::ResourceClaim>,
    hardware: std::sync::Arc<kvmit_control::ResourceClaim>,
}

impl AdapterAccess {
    pub fn acquire(id: &str) -> Result<Self> {
        let id = canonical_id(id);
        let adapter = kvmit_control::ResourceClaim::acquire("adapter", &id).map_err(BackendError)?;
        let hardware = kvmit_control::ResourceClaim::hardware(true).map_err(BackendError)?;
        Ok(Self { id, adapter: adapter.into(), hardware: hardware.into() })
    }

    pub async fn pair(&mut self, pairing_started: impl FnOnce()) -> Result<Pairing> {
        pair_claimed(self, pairing_started).await
    }

    pub async fn unpair(&mut self) -> Result<()> {
        unpair_claimed(&self.id).await
    }

    pub async fn connect(&mut self) -> std::result::Result<Connection, ConnectError> {
        // A timed-out disconnect can still be closing the previous GATT link.
        if std::sync::Arc::strong_count(&self.adapter) > 1 {
            return Err(ConnectError::Unavailable(BackendError("The previous Bluetooth link is still closing".into())));
        }
        connect_claimed(self).await
    }
}

/// Replace a raw failure with the reason Bluetooth is unusable, when the platform can tell (see `diag`).
fn explained(e: BackendError) -> BackendError {
    #[cfg(target_os = "linux")]
    return BackendError(diag::linux_explain(&e.0));
    #[cfg(not(target_os = "linux"))]
    e
}

/// Linux: the BlueZ session and default adapter, optionally powered on (as bluetoothctl would), with failures explained.
#[cfg(target_os = "linux")]
async fn bluez(power_on: bool) -> Result<(bluer::Session, bluer::Adapter)> {
    async {
        let session = bluer::Session::new().await?;
        let adapter = session.default_adapter().await?;
        if power_on {
            adapter.set_powered(true).await?;
        }
        Ok((session, adapter))
    }
    .await
    .map_err(explained)
}

async fn adapter() -> Result<Adapter> {
    let ad = async {
        let manager = Manager::new().await?;
        match manager.adapters().await?.into_iter().next() {
            Some(a) => Ok(a),
            None => err(diag::NO_ADAPTER),
        }
    }
    .await
    .map_err(explained)?;
    // Windows/macOS: a radio that is off still lists an adapter, and scans would just find nothing.
    #[cfg(not(target_os = "linux"))]
    if matches!(ad.adapter_state().await, Ok(btleplug::api::CentralState::PoweredOff)) {
        return err(diag::POWERED_OFF);
    }
    Ok(ad)
}

/// The id an adapter is known by on this platform (see the module comment).
fn peripheral_id(p: &Peripheral, props: &btleplug::api::PeripheralProperties) -> String {
    if cfg!(target_os = "macos") {
        p.id().to_string()
    } else {
        props.address.to_string()
    }
}

#[cfg_attr(target_os = "linux", allow(dead_code))]
async fn describe(p: &Peripheral) -> Option<Found> {
    let props = p.properties().await.ok()??;
    // Match on the advertised service or the name; a BlueZ discovery-filter on UUID was not reliable.
    let named = props.local_name.as_deref().is_some_and(|n| n.starts_with("kvm-it"));
    if !named && !props.services.contains(&SERVICE_UUID) {
        return None;
    }
    Some(Found {
        id: peripheral_id(p, &props),
        name: props.local_name.unwrap_or_else(|| "kvm-it".into()),
        rssi: props.rssi,
    })
}

/// Linux: use BlueZ's own discovery (transport "auto", like bluetoothctl). btleplug's LE-only discovery filter did
/// not report the adapter on the development laptop although bluetoothctl did.
#[cfg(target_os = "linux")]
pub async fn scan(timeout: Duration) -> Result<Vec<Found>> {
    let (_session, adapter) = bluez(true).await?;
    // BlueZ forgets unconnected devices quickly and the controller scans slowly while other devices are
    // connected, so poll its cache for the whole scan and remember every hit.
    let events = adapter.discover_devices().await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut found: Vec<Found> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        for addr in adapter.device_addresses().await? {
            let id = addr.to_string();
            if std::env::var_os("KVMIT_DEBUG").is_some() && id.starts_with("28:84") {
                eprintln!("cache has {id}");
            }
            if found.iter().any(|f| f.id == id) {
                continue;
            }
            let Ok(dev) = adapter.device(addr) else { continue };
            let uuids = dev.uuids().await.ok().flatten().unwrap_or_default();
            let name = dev.name().await.ok().flatten().unwrap_or_default();
            if uuids.contains(&SERVICE_UUID) || name.starts_with("kvm-it") {
                let rssi = dev.rssi().await.ok().flatten();
                found.push(Found { id, name: if name.is_empty() { "kvm-it".into() } else { name }, rssi });
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    drop(events);
    found.sort_by_key(|f| std::cmp::Reverse(f.rssi.unwrap_or(i16::MIN)));
    Ok(found)
}

/// How long a discovery scan runs by default. WinRT surfaces advertisements slowly (a 5 s scan found the adapter
/// in about 2 of 5 runs on Windows 11, 15 s in 4 of 4), so non-Linux platforms get a longer limit.
#[cfg(target_os = "linux")]
pub const DEFAULT_SCAN_SECS: u64 = 5;
#[cfg(not(target_os = "linux"))]
pub const DEFAULT_SCAN_SECS: u64 = 15;

/// Scan for adapters advertising the kvm-it service. Returns up to `timeout` after starting, or about a second
/// after the first adapter is heard (so a longer limit costs nothing when the adapter is in range).
#[cfg(not(target_os = "linux"))]
pub async fn scan(timeout: Duration) -> Result<Vec<Found>> {
    let ad = adapter().await?;
    ad.start_scan(ScanFilter::default()).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut settle_until = None;
    let mut found = Vec::new();
    loop {
        found.clear();
        for p in ad.peripherals().await? {
            if std::env::var_os("KVMIT_DEBUG").is_some() {
                eprintln!("seen: {:?}", p.properties().await.ok().flatten().map(|x| (x.address.to_string(), x.local_name, x.services, x.rssi)));
            }
            if let Some(f) = describe(&p).await {
                found.push(f);
            }
        }
        let now = tokio::time::Instant::now();
        if !found.is_empty() {
            settle_until.get_or_insert(now + Duration::from_secs(1));
        }
        if now >= deadline || settle_until.is_some_and(|t| now >= t) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = ad.stop_scan().await;
    found.sort_by_key(|f| std::cmp::Reverse(f.rssi.unwrap_or(i16::MIN)));
    Ok(found)
}

async fn find(ad: &Adapter, id: &str, wait: Duration) -> Result<Peripheral> {
    ad.start_scan(ScanFilter::default()).await?;
    let deadline = tokio::time::Instant::now() + wait;
    let mut events = ad.events().await?;
    loop {
        for p in ad.peripherals().await? {
            if let Ok(Some(props)) = p.properties().await {
                if peripheral_id(&p, &props).eq_ignore_ascii_case(id) {
                    let _ = ad.stop_scan().await;
                    return Ok(p);
                }
            }
        }
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if left.is_zero() {
            let _ = ad.stop_scan().await;
            return err(format!("adapter {id} not found (powered, advertising and in range? it only advertises to a bonded controller or while the pairing window is open)"));
        }
        // wake on any advertisement event or after a short poll
        let _ = tokio::time::timeout(left.min(Duration::from_millis(500)), async {
            while let Some(ev) = events.next().await {
                if matches!(ev, CentralEvent::DeviceDiscovered(_) | CentralEvent::DeviceUpdated(_)) {
                    break;
                }
            }
        })
        .await;
    }
}

/// An open GATT connection. Dropping it does NOT disconnect: call [`Connection::disconnect`].
pub struct Connection {
    _claim: std::sync::Arc<kvmit_control::ResourceClaim>,
    _hardware: std::sync::Arc<kvmit_control::ResourceClaim>,
    pub io: Option<LinkIo>,
    peripheral: Peripheral,
    /// The task writing frames to the adapter: cancelled before disconnecting, so no write of ours is still pending
    /// (holding the backend's service lock) when the backend tears the link down.
    writer: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    #[cfg(target_os = "macos")]
    peer_disconnected: Arc<AtomicBool>,
    /// Windows: holds the fast-connection request for as long as the link is open.
    #[cfg(windows)]
    _fast: Option<FastLink>,
}

impl Connection {
    pub fn take_io(&mut self) -> LinkIo {
        self.io.take().expect("io already taken")
    }
    /// Bounded (see [`close_quietly`]): after the adapter lost power, the platform may never confirm, and the caller's
    /// reconnect must go on.
    pub async fn disconnect(&self) {
        // Cancel our writer and wait (briefly) until it is gone, so no write of ours still holds the backend's lock
        // when the link is torn down.
        let writer = self.writer.lock().unwrap().take();
        if let Some(w) = writer {
            w.abort();
            let _ = tokio::time::timeout(Duration::from_secs(1), w).await;
        }
        #[cfg(target_os = "macos")]
        if self.peer_disconnected.load(Ordering::Acquire) {
            return;
        }
        close_quietly(&self.peripheral, (self._claim.clone(), self._hardware.clone())).await;
    }
}

/// Connect to a bonded adapter by address and bridge its GATT characteristics to byte-frame channels.
pub async fn connect(id: &str) -> Result<Connection> {
    AdapterAccess::acquire(id)?.connect().await.map_err(|error| match error {
        ConnectError::Unavailable(e) | ConnectError::Link(e) | ConnectError::Incompatible(e) => e,
    })
}

async fn connect_claimed(access: &AdapterAccess) -> std::result::Result<Connection, ConnectError> {
    let id = &access.id;
    let claim = access.adapter.clone();
    let hardware = access.hardware.clone();
    let ad = adapter().await.map_err(ConnectError::Unavailable)?;
    // Linux: a powered-off or blocked adapter shows up here as "not found"; say why. (Not checked up front: BlueZ's
    // default adapter is not necessarily the one btleplug picked when a computer has two.)
    let p = find(&ad, id, Duration::from_secs(20)).await.map_err(|e| ConnectError::Unavailable(explained(e)))?;
    #[cfg(target_os = "macos")]
    let mut events = ad.events().await.map_err(|e| ConnectError::Unavailable(e.into()))?;
    // Bounded: an adapter that lost power (or is restarting) can leave the OS's connect or service discovery waiting
    // forever, which would stall the app's reconnect loop until someone pressed Connect again. A timed-out attempt is
    // closed and reported, and the caller's next attempt finds the adapter once it is back.
    let setup = async {
        let link_error = |e: btleplug::Error| ConnectError::Link(e.into());
        p.connect().await.map_err(link_error)?;
        #[cfg(windows)]
        let fast = request_fast_link(id).await;
        p.discover_services().await.map_err(link_error)?;
        let chars = p.characteristics();
        let rx_char = chars.iter().find(|c| c.uuid == RX_UUID).cloned();
        let tx_char = chars.iter().find(|c| c.uuid == TX_UUID).cloned();
        let (Some(rx_char), Some(tx_char)) = (rx_char, tx_char) else {
            return Err(ConnectError::Incompatible(BackendError("device does not expose the kvm-it characteristics".into())));
        };
        p.subscribe(&tx_char).await.map_err(link_error)?;
        let notifications = p.notifications().await.map_err(link_error)?;
        #[cfg(windows)]
        return Ok((rx_char, notifications, fast));
        #[cfg(not(windows))]
        Ok((rx_char, notifications))
    };
    let set_up = match tokio::time::timeout(GATT_SETUP_TIMEOUT, setup).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            close_quietly(&p,(claim.clone(),hardware.clone())).await;
            return Err(e);
        }
        Err(_) => {
            close_quietly(&p,(claim.clone(),hardware.clone())).await;
            return Err(ConnectError::Link(BackendError(format!(
                "adapter {id} did not answer within {} s (it may be restarting after a power loss)",
                GATT_SETUP_TIMEOUT.as_secs()
            ))));
        }
    };
    #[cfg(windows)]
    let (rx_char, notifications, fast) = set_up;
    #[cfg(not(windows))]
    let (rx_char, notifications) = set_up;

    // CoreBluetooth's notification stream stays open after a remote disconnect.
    // Stop it on the event for this peripheral so the client can reconnect immediately.
    #[cfg(target_os = "macos")]
    let peer_disconnected = Arc::new(AtomicBool::new(false));
    #[cfg(target_os = "macos")]
    let notifications = {
        let peripheral_id = p.id();
        let peer_disconnected = peer_disconnected.clone();
        notifications.take_until(async move {
            while let Some(event) = events.next().await {
                if matches!(event, CentralEvent::DeviceDisconnected(id) if id == peripheral_id) {
                    peer_disconnected.store(true, Ordering::Release);
                    break;
                }
            }
        })
    };

    let (to_client, client_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (client_tx, mut from_client) = mpsc::unbounded_channel::<Vec<u8>>();

    tokio::spawn(async move {
        let debug = std::env::var_os("KVMIT_DEBUG").is_some();
        let mut last = std::time::Instant::now();
        futures::pin_mut!(notifications);
        while let Some(n) = notifications.next().await {
            if debug && last.elapsed() > Duration::from_millis(1500) {
                eprintln!("[debug] no notification for {} ms", last.elapsed().as_millis());
            }
            last = std::time::Instant::now();
            if n.uuid == TX_UUID && to_client.send(n.value).is_err() {
                break;
            }
        }
        if debug {
            eprintln!("[debug] notification stream ended after {} ms of silence", last.elapsed().as_millis());
        }
        // stream end = disconnect; dropping `to_client` tells the client the link is gone
    });
    let writer = p.clone();
    // Dropping/cancelling a Connection must not release ownership while its
    // detached writer can still deliver input.
    let writer_claims = (claim.clone(), hardware.clone());
    let writer_task = tokio::spawn(async move {
        let _claims = writer_claims;
        let debug = std::env::var_os("KVMIT_DEBUG").is_some();
        while let Some(frame) = from_client.recv().await {
            let t = std::time::Instant::now();
            let r = writer.write(&rx_char, &frame, WriteType::WithoutResponse).await;
            if debug && (t.elapsed() > Duration::from_millis(100) || r.is_err()) {
                eprintln!("[debug] gatt write took {} ms, ok={}", t.elapsed().as_millis(), r.is_ok());
            }
            if r.is_err() {
                break; // drops from_client; client sees sends fail as Closed
            }
        }
    });
    Ok(Connection {
        _claim: claim,
        _hardware: hardware,
        io: Some(LinkIo { tx: client_tx, rx: client_rx }),
        peripheral: p,
        writer: std::sync::Mutex::new(Some(writer_task)),
        #[cfg(target_os = "macos")]
        peer_disconnected,
        #[cfg(windows)]
        _fast: fast,
    })
}

/// Pair with an adapter during its physical pairing window.
pub async fn pair(id: &str) -> Result<()> {
    AdapterAccess::acquire(id)?.pair(|| {}).await.map(|_| ())
}

/// Pair and trust an adapter through BlueZ. Pairing is Just Works, so the adapter only accepts it
/// within its physical pairing window (15 s after power-on, or after a BOOT short press).
#[cfg(target_os = "linux")]
async fn pair_claimed(access: &AdapterAccess, pairing_started: impl FnOnce()) -> Result<Pairing> {
    let id = &access.id;
    use bluer::agent::Agent;
    let (session, adapter) = bluez(true).await?;
    let addr: bluer::Address = id.parse().map_err(|_| BackendError(format!("bad address {id}")))?;
    let dev = wait_for_device(&adapter, addr, Duration::from_secs(20)).await?;
    if dev.is_paired().await? {
        dev.set_trusted(true).await?;
        return Ok(Pairing::Existing);
    }

    pairing_started();
    let agent = Agent {
        request_confirmation: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        request_authorization: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        authorize_service: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        ..Default::default()
    };
    let _agent = session.register_agent(agent).await?;
    // A leftover unpaired link (e.g. from a status call that failed against a reset adapter) makes BlueZ's
    // pairing fail with "Authentication Canceled"; start from a clean link, and retry once on a fresh one.
    let pairing = async {
        if dev.is_connected().await? {
            let _ = dev.disconnect().await;
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        if let Err(first) = dev.pair().await {
            let _ = dev.disconnect().await;
            tokio::time::sleep(Duration::from_millis(800)).await;
            dev.pair().await.map_err(|e| BackendError(format!("{e} (first attempt: {first})")))?;
        }
        Ok::<(), BackendError>(())
    };
    tokio::time::timeout(Duration::from_secs(45), pairing).await
        .map_err(|_| BackendError("BlueZ pairing timed out".into()))??;
    dev.set_trusted(true).await?;
    Ok(Pairing::Established)
}

/// Keep discovery running until BlueZ has the device in its cache, then hand it back immediately: the cache entry
/// can vanish again within seconds when the controller only hears the adapter occasionally.
#[cfg(target_os = "linux")]
async fn wait_for_device(adapter: &bluer::Adapter, addr: bluer::Address, wait: Duration) -> Result<bluer::Device> {
    let _events = adapter.discover_devices().await?;
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if adapter.device_addresses().await?.contains(&addr) {
            return Ok(adapter.device(addr)?);
        }
        if tokio::time::Instant::now() >= deadline {
            return err(format!("adapter {addr} was not heard within {} s (powered, advertising, in range? it only advertises to a bonded controller or while the pairing window is open)", wait.as_secs()));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(windows)]
impl From<windows::core::Error> for BackendError {
    fn from(e: windows::core::Error) -> Self {
        BackendError(e.to_string())
    }
}

#[cfg(windows)]
async fn le_device(id: &str) -> Result<windows::Devices::Bluetooth::BluetoothLEDevice> {
    cached_le_device(id).await?.ok_or_else(|| BackendError(format!("Windows returned no Bluetooth device handle for adapter {id}")))
}

#[cfg(windows)]
async fn cached_le_device(id: &str) -> Result<Option<windows::Devices::Bluetooth::BluetoothLEDevice>> {
    let addr = pairing_address(id)?;
    match windows::Devices::Bluetooth::BluetoothLEDevice::FromBluetoothAddressAsync(addr)?.await {
        Ok(dev) => Ok(Some(dev)),
        Err(error) if error.code().is_ok() => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
async fn has_saved_windows_pairing(id: &str) -> bool {
    // Windows can answer quickly for a known board. A missing GATT handle says
    // nothing about whether Windows can discover an association endpoint.
    let Ok(Ok(Some(device))) = tokio::time::timeout(Duration::from_secs(2), cached_le_device(id)).await else {
        return false;
    };
    if pairing_address(id).ok() != device.BluetoothAddress().ok() {
        return false;
    }
    device.DeviceInformation().and_then(|info| info.Pairing())
        .and_then(|pairing| pairing.IsPaired()).unwrap_or(false)
}

/// Pair/forget association endpoints directly. A BluetoothLEDevice is a GATT
/// handle and can be null precisely when a stale/missing bond needs repair.
#[cfg(windows)]
fn pairing_selector(address: u64) -> Result<windows::core::HSTRING> {
    use windows::Devices::Bluetooth::BluetoothLEDevice;
    // WinRT's selector can find unpaired endpoints that the same address expressed
    // as a System.Devices.Aep.DeviceAddress property filter never surfaces.
    Ok(BluetoothLEDevice::GetDeviceSelectorFromBluetoothAddress(address)?)
}

#[cfg(windows)]
fn pairing_address(id: &str) -> Result<u64> {
    let [a, b, c, d, e, f] = parse_address(id)?.into_inner();
    Ok(u64::from_be_bytes([0, 0, a, b, c, d, e, f]))
}

#[cfg(windows)]
fn pairing_properties() -> windows_collections::IIterable<windows::core::HSTRING> {
    Vec::<windows::core::HSTRING>::new().into()
}

#[cfg(windows)]
fn endpoint_matches(endpoint: &str, address: u64) -> bool {
    // The endpoint ID is BluetoothLE#BluetoothLE<local radio>-<remote device>.
    // Some unpaired endpoints have no Aep.DeviceAddress property yet.
    const PREFIX: &str = "BluetoothLE#BluetoothLE";
    let Some(prefix) = endpoint.get(..PREFIX.len()) else { return false; };
    if !prefix.eq_ignore_ascii_case(PREFIX) { return false; }
    let Some((radio, remote)) = endpoint[PREFIX.len()..].split_once('-') else { return false; };
    pairing_address(radio).is_ok() && pairing_address(remote).is_ok_and(|found| found == address)
}

#[cfg(windows)]
async fn pairing_endpoint(id: &str) -> Result<windows::Devices::Enumeration::DeviceInformation> {
    use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind, DeviceWatcher};
    use windows::Foundation::TypedEventHandler;
    let address = pairing_address(id)?;
    let selector = pairing_selector(address)?;
    let watcher = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(&selector, &pairing_properties(), DeviceInformationKind::AssociationEndpoint)
        .map_err(|e| BackendError(format!("Windows could not start Bluetooth pairing discovery: {e}")))?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    let added = watcher.Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, info| {
        if let Some(info) = info.as_ref() {
            if info.Id().is_ok_and(|value| endpoint_matches(&value.to_string_lossy(), address)) {
                let _ = tx.send(info.clone());
            }
        }
        Ok(())
    }))?;
    if let Err(error) = watcher.Start() { let _ = watcher.RemoveAdded(added); return err(format!("Windows could not start Bluetooth pairing discovery: {error}")); }
    let found = tokio::time::timeout(Duration::from_secs(45), rx.recv()).await.ok().flatten();
    let _ = watcher.Stop();
    let _ = watcher.RemoveAdded(added);
    found.ok_or_else(|| BackendError(format!("Windows did not discover a Bluetooth pairing entry for {id}")))
}

/// Keeps the Windows "throughput optimised" connection request alive; dropping it lets the stack relax the link.
#[cfg(windows)]
pub struct FastLink {
    _dev: windows::Devices::Bluetooth::BluetoothLEDevice,
    _req: windows::Devices::Bluetooth::BluetoothLEPreferredConnectionParametersRequest,
}

/// Windows 11: ask for the shortest connection interval while the link is open. Windows' default interval (about
/// 60 ms here) cannot carry a moving pointer's motion frames plus acks; the link collapses above ~30-60 frames/s.
/// Best effort: older Windows lacks the API, and a refusal just leaves the default behaviour.
#[cfg(windows)]
async fn request_fast_link(id: &str) -> Option<FastLink> {
    use windows::Devices::Bluetooth::{BluetoothLEDevice, BluetoothLEPreferredConnectionParameters};
    let debug = std::env::var_os("KVMIT_DEBUG").is_some();
    let interval = |d: &BluetoothLEDevice| d.GetConnectionParameters().and_then(|p| p.ConnectionInterval()).ok();
    let dev = le_device(id).await.ok()?;
    if debug {
        eprintln!("[debug] connection interval before: {:?} (x1.25 ms)", interval(&dev));
    }
    let req = dev.RequestPreferredConnectionParameters(&BluetoothLEPreferredConnectionParameters::ThroughputOptimized().ok()?).ok()?;
    if debug {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        eprintln!("[debug] fast-link request status {:?}; interval now: {:?} (x1.25 ms)", req.Status(), interval(&dev));
    }
    Some(FastLink { _dev: dev, _req: req })
}

/// Windows: pair through the WinRT custom-pairing API, accepting the Just Works confirmation in code, so no
/// toast or Settings dialog is needed. The adapter must be in its pairing window (physical presence).
#[cfg(windows)]
async fn pair_claimed(access: &AdapterAccess, pairing_started: impl FnOnce()) -> Result<Pairing> {
    let id = &access.id;
    use windows::Devices::Enumeration::{
        DeviceInformationCustomPairing, DevicePairingKinds, DevicePairingProtectionLevel, DevicePairingRequestedEventArgs,
        DevicePairingResultStatus,
    };
    use windows::Foundation::TypedEventHandler;
    let _pairing = WINDOWS_PAIRING.lock().await;
    if has_saved_windows_pairing(id).await {
        return Ok(Pairing::Existing);
    }
    let pairing = pairing_endpoint(id).await?.Pairing()?;
    if pairing.IsPaired()? {
        return Ok(Pairing::Existing);
    }
    pairing_started();
    let custom = pairing.Custom()?;
    let token = custom.PairingRequested(&TypedEventHandler::<DeviceInformationCustomPairing, DevicePairingRequestedEventArgs>::new(
        |_, args| {
            // Just Works only: a peer asking for numeric comparison would need a real comparison, which this
            // headless adapter cannot show, so it is not accepted (the pairing then fails safely).
            if let Some(a) = args.as_ref() {
                if a.PairingKind()? == DevicePairingKinds::ConfirmOnly {
                    a.Accept()?;
                }
            }
            Ok(())
        },
    ))?;
    let operation = match custom.PairWithProtectionLevelAsync(DevicePairingKinds::ConfirmOnly, DevicePairingProtectionLevel::Encryption) {
        Ok(operation) => operation,
        Err(error) => { let _ = custom.RemovePairingRequested(token); return Err(error.into()); }
    };
    let res = tokio::time::timeout(Duration::from_secs(60), operation.clone()).await;
    let _ = custom.RemovePairingRequested(token);
    let result = match res {
        Ok(result) => result?,
        Err(_) => { let _ = operation.Cancel(); return err("Windows pairing timed out"); }
    };
    match result.Status()? {
        DevicePairingResultStatus::Paired => Ok(Pairing::Established),
        DevicePairingResultStatus::AlreadyPaired => Ok(Pairing::Existing),
        s => err(format!("Windows could not pair with {id}: {s:?}")),
    }
}

/// macOS: there is no pairing API. CoreBluetooth pairs by itself when an encrypted characteristic is used, so pairing is:
/// connect, then a write *with response* of a harmless PING to the adapter's receive characteristic, which requires an
/// encrypted link. The write only succeeds once macOS has paired (it may show its own prompt), so success here means the
/// bond exists. The adapter must be in its pairing window.
#[cfg(target_os = "macos")]
async fn pair_claimed(access: &AdapterAccess, pairing_started: impl FnOnce()) -> Result<Pairing> {
    let id = &access.id;
    let ad = adapter().await?;
    let p = find(&ad, id, Duration::from_secs(20)).await?;
    pairing_started();
    let outcome = async {
        tokio::time::timeout(GATT_SETUP_TIMEOUT, async {
            p.connect().await?;
            p.discover_services().await?;
            Ok::<(), BackendError>(())
        }).await.map_err(|_| BackendError("Timed out opening the Bluetooth link for pairing".into()))??;
        let rx = p.characteristics().into_iter().find(|c| c.uuid == RX_UUID);
        let Some(rx) = rx else { return err("device does not expose the kvm-it characteristics") };
        let ping = kvmit_protocol::message::Request::Ping(Vec::new()).encode(0);
        match tokio::time::timeout(Duration::from_secs(60), p.write(&rx, &ping, WriteType::WithResponse)).await {
            Ok(Ok(())) => Ok(Pairing::Established),
            Ok(Err(e)) => err(format!(
                "{e}. macOS pairs on first use: open the adapter's pairing window (re-plug it, or a short BOOT press), accept macOS's pairing prompt if one appears, and try again"
            )),
            Err(_) => err("pairing did not complete within 60 s (was macOS's pairing prompt accepted? is the adapter's pairing window open?)"),
        }
    }
    .await;
    close_quietly(&p, (access.adapter.clone(), access.hardware.clone())).await;
    outcome
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
async fn pair_claimed(_access: &AdapterAccess, _pairing_started: impl FnOnce()) -> Result<Pairing> {
    err("pairing is not implemented on this platform")
}

/// Remove the OS-level bond. A 10 s BOOT hold erases the adapter-side bond.
pub async fn unpair(id: &str) -> Result<()> {
    AdapterAccess::acquire(id)?.unpair().await
}

#[cfg(target_os = "linux")]
async fn unpair_claimed(id: &str) -> Result<()> {
    let (_session, adapter) = bluez(false).await?;
    let addr: bluer::Address = id.parse().map_err(|_| BackendError(format!("bad address {id}")))?;
    match adapter.remove_device(addr).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind == bluer::ErrorKind::DoesNotExist => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(windows)]
async fn unpair_claimed(id: &str) -> Result<()> {
    use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind, DeviceUnpairingResultStatus};
    let _pairing = WINDOWS_PAIRING.lock().await;
    let address = pairing_address(id)?;
    let selector = pairing_selector(address)?;
    let lookup = {
        let properties = pairing_properties();
        DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&selector, &properties, DeviceInformationKind::AssociationEndpoint)
            .map_err(|e| BackendError(format!("Windows could not look up saved Bluetooth pairings; nothing was forgotten: {e}")))?
    };
    let response = match tokio::time::timeout(Duration::from_secs(45), lookup.clone()).await {
        Ok(response) => response,
        Err(_) => { let _ = lookup.Cancel(); return err("Windows timed out looking up the saved pairing; nothing was forgotten. Try Repair pairing again."); }
    };
    let entries = match response {
        Ok(entries) => entries,
        Err(error) => return err(format!("Windows could not read saved Bluetooth pairings; nothing was forgotten: {error}")),
    };
    // Includes stale/offline association records; forgetting must not require
    // opening a GATT connection to the very board whose bond is broken.
    for i in 0..entries.Size()? {
        let entry = entries.GetAt(i)?;
        if !endpoint_matches(&entry.Id()?.to_string_lossy(), address) { continue; }
        let pairing = entry.Pairing()?;
        if !pairing.IsPaired()? { continue; }
        let operation = pairing.UnpairAsync()?;
        let result = match tokio::time::timeout(Duration::from_secs(20), operation.clone()).await {
            Ok(result) => result?,
            Err(_) => { let _ = operation.Cancel(); return err("Windows did not confirm removing the pairing. Try Repair pairing again."); }
        };
        match result.Status()? {
            DeviceUnpairingResultStatus::Unpaired | DeviceUnpairingResultStatus::AlreadyUnpaired => {},
            s => return err(format!("Windows could not unpair {id}: {s:?}")),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn unpair_claimed(_id: &str) -> Result<()> {
    err("macOS has no API to forget a Bluetooth LE device: remove it in System Settings > Bluetooth (and hold BOOT 10 s on the adapter to erase its side)")
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
async fn unpair_claimed(_id: &str) -> Result<()> {
    err("remove the device in the OS Bluetooth settings")
}

#[cfg(test)]
mod disconnect_tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn missing_macos_callback_releases_ownership() {
        let lock_dir = std::env::temp_dir().join(format!(
            "kvmit-macos-disconnect-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let adapter = || kvmit_control::ResourceClaim::at(&lock_dir, "adapter", "fake-adapter");
        let hardware = || kvmit_control::ResourceClaim::at(&lock_dir, "hardware", "test");
        let claims = (Arc::new(adapter().unwrap()), Arc::new(hardware().unwrap()));
        let start = tokio::time::Instant::now();

        close_macos(
            async {
                assert!(adapter().is_err());
                assert!(hardware().is_err());
                std::future::pending::<()>().await;
            },
            claims,
        )
        .await;

        assert_eq!(start.elapsed(), DISCONNECT_TIMEOUT);
        drop(adapter().expect("the next connection can claim the adapter"));
        drop(hardware().expect("the next operation can claim the hardware"));
        std::fs::remove_dir_all(lock_dir).unwrap();
    }
}

#[cfg(all(test, windows))]
mod pairing_tests {
    use super::*;

    #[test]
    #[ignore = "requires a Windows Bluetooth adapter and KVMIT_TEST_BLE_ADAPTER"]
    fn finds_real_windows_pairing_endpoint() {
        let id = std::env::var("KVMIT_TEST_BLE_ADAPTER").expect("set the KVM adapter address");
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let entry = runtime.block_on(pairing_endpoint(&id)).unwrap();
        let pairing = entry.Pairing().unwrap();
        assert!(pairing.IsPaired().unwrap() || pairing.CanPair().unwrap());
        if pairing.IsPaired().unwrap() {
            assert!(runtime.block_on(has_saved_windows_pairing(&id)));
        }
    }

    #[test]
    fn validates_the_remote_address_in_a_windows_ble_endpoint() {
        let board = pairing_address("98:C3:77:A0:95:A2").unwrap();
        assert_eq!(board, 0x98c377a095a2);
        assert_eq!(pairing_address("98c377a095a2").unwrap(), board);
        assert!(endpoint_matches("BluetoothLE#BluetoothLE01:02:03:04:05:06-98:c3:77:a0:95:a2", board));
        for unrelated in [
            "BluetoothLE#BluetoothLE98:c3:77:a0:95:a2-01:02:03:04:05:06",
            "BluetoothLE#BluetoothLE01:02:03:04:05:06-98:c3:77:a0:95:a3",
            "Bluetooth#Bluetooth01:02:03:04:05:06-98:c3:77:a0:95:a2",
            "BluetoothLE#BluetoothLE01:02:03:04:05:06-98:c3:77:a0:95:a2-extra",
        ] {
            assert!(!endpoint_matches(unrelated, board));
        }
        for bad in ["98:c3:77:a0:a4:32\" OR true", "1000000000000", "not-a-board", "a日本00000"] {
            assert!(pairing_address(bad).is_err());
        }
    }
}
