//! BLE transport: scan, connect, GATT (btleplug, Linux + Windows) and Linux/BlueZ pairing (bluer).
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
    /// Bluetooth address, "AA:BB:CC:DD:EE:FF" — the stable id used by connect/pair.
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

#[cfg_attr(target_os = "linux", allow(dead_code))]
async fn describe(p: &Peripheral) -> Option<Found> {
    let props = p.properties().await.ok()??;
    // Match on the advertised service or the name; a BlueZ discovery-filter on UUID was not reliable.
    let named = props.local_name.as_deref().is_some_and(|n| n.starts_with("kvm-it"));
    if !named && !props.services.contains(&SERVICE_UUID) {
        return None;
    }
    Some(Found {
        id: props.address.to_string(),
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

/// Scan for adapters advertising the kvm-it service. Returns when `timeout` elapses.
#[cfg(not(target_os = "linux"))]
pub async fn scan(timeout: Duration) -> Result<Vec<Found>> {
    let ad = adapter().await?;
    ad.start_scan(ScanFilter::default()).await?;
    tokio::time::sleep(timeout).await;
    let mut found = Vec::new();
    for p in ad.peripherals().await? {
        if std::env::var_os("KVMIT_DEBUG").is_some() {
            eprintln!("seen: {:?}", p.properties().await.ok().flatten().map(|x| (x.address.to_string(), x.local_name, x.services, x.rssi)));
        }
        if let Some(f) = describe(&p).await {
            found.push(f);
        }
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
                if props.address.to_string().eq_ignore_ascii_case(id) {
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

/// An open GATT connection. Dropping it disconnects.
pub struct Connection {
    pub io: Option<LinkIo>,
    peripheral: Peripheral,
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
        while let Some(n) = notifications.next().await {
            if n.uuid == TX_UUID && to_client.send(n.value).is_err() {
                break;
            }
        }
        // stream end = disconnect; dropping `to_client` tells the client the link is gone
    });
    let writer = p.clone();
    tokio::spawn(async move {
        while let Some(frame) = from_client.recv().await {
            if writer.write(&rx_char, &frame, WriteType::WithoutResponse).await.is_err() {
                break; // drops from_client; client sees sends fail as Closed
            }
        }
    });
    Ok(Connection { io: Some(LinkIo { tx: client_tx, rx: client_rx }), peripheral: p })
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

#[cfg(not(target_os = "linux"))]
pub async fn pair(_id: &str) -> Result<()> {
    // Windows pairs when the first encrypted characteristic is accessed (the OS shows its own prompt).
    Ok(())
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

#[cfg(not(target_os = "linux"))]
pub async fn unpair(_id: &str) -> Result<()> {
    err("remove the device in Windows Bluetooth settings")
}
