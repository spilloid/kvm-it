//! Shared connection helpers (CLI and GUI).
use kvmit_ble::{backend, Device};
use std::time::Duration;

pub type BoxErr = Box<dyn std::error::Error + Send + Sync>;

pub async fn connect(id: &str) -> Result<(Device, backend::Connection), BoxErr> {
    let mut conn = backend::connect(id).await?;
    let dev = Device::connect(conn.take_io()).await.map_err(|e| {
        format!("{e}. If this is a new adapter, run `kvmit pair` first (and press BOOT on the adapter to open its pairing window).")
    })?;
    Ok((dev, conn))
}

/// Pick an adapter: the given id, else the configured last device, else the strongest one in range.
pub async fn resolve(id: Option<String>, last: Option<String>) -> Result<String, BoxErr> {
    if let Some(i) = id.or(last) {
        return Ok(i);
    }
    let found = backend::scan(Duration::from_secs(5)).await?;
    match found.len() {
        0 => Err("no kvm-it adapter found. Plug it in (USB port to the target) and, if it was never paired, press BOOT briefly to open its pairing window.".into()),
        _ => Ok(found[0].id.clone()),
    }
}
