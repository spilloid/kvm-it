//! BLE transport: scan, connect, GATT (btleplug: Linux, Windows, macOS) and pairing (BlueZ via bluer, WinRT, and on macOS
//! CoreBluetooth's own pairing on first encrypted access).
//!
//! Adapter ids: the Bluetooth address on Linux and Windows. macOS never reveals addresses; there the id is the UUID
//! CoreBluetooth assigns the peripheral, stable on that Mac (so a saved id does not carry over to another Mac).
//! Hardware-dependent; not unit-testable. Protocol logic lives in `client`.
use crate::client::LinkIo;
use btleplug::api::{Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

pub const SERVICE_UUID: Uuid = Uuid::from_u128(0x7a1e0000_4b49_4d54_8000_6b766d697401);
pub const RX_UUID: Uuid = Uuid::from_u128(0x7a1e0001_4b49_4d54_8000_6b766d697401); // controller -> device (write)
pub const TX_UUID: Uuid = Uuid::from_u128(0x7a1e0002_4b49_4d54_8000_6b766d697401); // device -> controller (notify)

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

async fn adapter() -> Result<Adapter> {
    let manager = Manager::new().await?;
    match manager.adapters().await?.into_iter().next() {
        Some(a) => Ok(a),
        None => err("no Bluetooth adapter found (is Bluetooth enabled?)"),
    }
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
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;
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
    pub io: Option<LinkIo>,
    peripheral: Peripheral,
    /// Windows: holds the fast-connection request for as long as the link is open.
    #[cfg(windows)]
    _fast: Option<FastLink>,
}

impl Connection {
    pub fn take_io(&mut self) -> LinkIo {
        self.io.take().expect("io already taken")
    }
    pub async fn disconnect(&self) {
        let _ = self.peripheral.disconnect().await;
    }
}

/// Connect to a bonded adapter by address and bridge its GATT characteristics to byte-frame channels.
pub async fn connect(id: &str) -> Result<Connection> {
    let ad = adapter().await?;
    let p = find(&ad, id, Duration::from_secs(20)).await?;
    p.connect().await?;
    #[cfg(windows)]
    let fast = request_fast_link(id).await;
    p.discover_services().await?;
    let chars = p.characteristics();
    let rx_char = chars.iter().find(|c| c.uuid == RX_UUID).cloned();
    let tx_char = chars.iter().find(|c| c.uuid == TX_UUID).cloned();
    let (Some(rx_char), Some(tx_char)) = (rx_char, tx_char) else {
        let _ = p.disconnect().await;
        return err("device does not expose the kvm-it characteristics");
    };
    p.subscribe(&tx_char).await?;
    let mut notifications = p.notifications().await?;

    let (to_client, client_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (client_tx, mut from_client) = mpsc::unbounded_channel::<Vec<u8>>();

    tokio::spawn(async move {
        let debug = std::env::var_os("KVMIT_DEBUG").is_some();
        let mut last = std::time::Instant::now();
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
    tokio::spawn(async move {
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
        io: Some(LinkIo { tx: client_tx, rx: client_rx }),
        peripheral: p,
        #[cfg(windows)]
        _fast: fast,
    })
}

/// Pair and trust an adapter through BlueZ. Pairing is Just Works, so the adapter only accepts it
/// within its physical pairing window (15 s after power-on, or after a BOOT short press).
#[cfg(target_os = "linux")]
pub async fn pair(id: &str) -> Result<()> {
    use bluer::agent::Agent;
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    adapter.set_powered(true).await?;
    let agent = Agent {
        request_confirmation: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        request_authorization: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        authorize_service: Some(Box::new(|_| Box::pin(async { Ok(()) }))),
        ..Default::default()
    };
    let _agent = session.register_agent(agent).await?;
    let addr: bluer::Address = id.parse().map_err(|_| BackendError(format!("bad address {id}")))?;
    let dev = wait_for_device(&adapter, addr, Duration::from_secs(90)).await?;
    if dev.is_paired().await? {
        dev.set_trusted(true).await?;
        return Ok(());
    }
    // A leftover unpaired link (e.g. from a status call that failed against a reset adapter) makes BlueZ's
    // pairing fail with "Authentication Canceled"; start from a clean link, and retry once on a fresh one.
    if dev.is_connected().await? {
        let _ = dev.disconnect().await;
        tokio::time::sleep(Duration::from_millis(800)).await;
    }
    if let Err(first) = dev.pair().await {
        let _ = dev.disconnect().await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        dev.pair().await.map_err(|e| BackendError(format!("{e} (first attempt: {first})")))?;
    }
    dev.set_trusted(true).await?;
    Ok(())
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
    let addr = u64::from_str_radix(&id.replace(':', ""), 16).map_err(|_| BackendError(format!("bad address {id}")))?;
    Ok(windows::Devices::Bluetooth::BluetoothLEDevice::FromBluetoothAddressAsync(addr)?.await?)
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
pub async fn pair(id: &str) -> Result<()> {
    use windows::Devices::Enumeration::{
        DeviceInformationCustomPairing, DevicePairingKinds, DevicePairingProtectionLevel, DevicePairingRequestedEventArgs,
        DevicePairingResultStatus,
    };
    use windows::Foundation::TypedEventHandler;
    let dev = le_device(id).await?;
    let pairing = dev.DeviceInformation()?.Pairing()?;
    if pairing.IsPaired()? {
        return Ok(());
    }
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
    let res = custom
        .PairWithProtectionLevelAsync(DevicePairingKinds::ConfirmOnly, DevicePairingProtectionLevel::Encryption)?
        .await;
    let _ = custom.RemovePairingRequested(token);
    match res?.Status()? {
        DevicePairingResultStatus::Paired | DevicePairingResultStatus::AlreadyPaired => Ok(()),
        s => err(format!("Windows could not pair with {id}: {s:?}. Is the adapter's pairing window open (BOOT short press, or just re-plugged)?")),
    }
}

/// macOS: there is no pairing API. CoreBluetooth pairs by itself when an encrypted characteristic is used, so pairing is:
/// connect, then a write *with response* of a harmless PING to the adapter's receive characteristic, which requires an
/// encrypted link. The write only succeeds once macOS has paired (it may show its own prompt), so success here means the
/// bond exists. The adapter must be in its pairing window.
#[cfg(target_os = "macos")]
pub async fn pair(id: &str) -> Result<()> {
    let ad = adapter().await?;
    let p = find(&ad, id, Duration::from_secs(90)).await?;
    p.connect().await?;
    let outcome = async {
        p.discover_services().await?;
        let rx = p.characteristics().into_iter().find(|c| c.uuid == RX_UUID);
        let Some(rx) = rx else { return err("device does not expose the kvm-it characteristics") };
        let ping = kvmit_protocol::message::Request::Ping(Vec::new()).encode(0);
        match tokio::time::timeout(Duration::from_secs(60), p.write(&rx, &ping, WriteType::WithResponse)).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => err(format!(
                "{e}. macOS pairs on first use: open the adapter's pairing window (re-plug it, or a short BOOT press), accept macOS's pairing prompt if one appears, and try again"
            )),
            Err(_) => err("pairing did not complete within 60 s (was macOS's pairing prompt accepted? is the adapter's pairing window open?)"),
        }
    }
    .await;
    let _ = p.disconnect().await;
    outcome
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub async fn pair(_id: &str) -> Result<()> {
    err("pairing is not implemented on this platform")
}

/// Remove the OS-level bond (Linux). The adapter-side bond is erased with a 10 s BOOT hold.
#[cfg(target_os = "linux")]
pub async fn unpair(id: &str) -> Result<()> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    let addr: bluer::Address = id.parse().map_err(|_| BackendError(format!("bad address {id}")))?;
    adapter.remove_device(addr).await?;
    Ok(())
}

#[cfg(windows)]
pub async fn unpair(id: &str) -> Result<()> {
    use windows::Devices::Enumeration::DeviceUnpairingResultStatus;
    let dev = le_device(id).await?;
    match dev.DeviceInformation()?.Pairing()?.UnpairAsync()?.await?.Status()? {
        DeviceUnpairingResultStatus::Unpaired | DeviceUnpairingResultStatus::AlreadyUnpaired => Ok(()),
        s => err(format!("Windows could not unpair {id}: {s:?}")),
    }
}

#[cfg(target_os = "macos")]
pub async fn unpair(_id: &str) -> Result<()> {
    err("macOS has no API to forget a Bluetooth LE device: remove it in System Settings > Bluetooth (and hold BOOT 10 s on the adapter to erase its side)")
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
pub async fn unpair(_id: &str) -> Result<()> {
    err("remove the device in the OS Bluetooth settings")
}
