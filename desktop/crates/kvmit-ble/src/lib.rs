//! Controller side of the kvm-it link.
//!  - `client`: protocol client over any byte-frame channel (handshake, ack/retry with stable `seq`,
//!    keepalive, motion accumulation). Unit-tested against a mock device; no Bluetooth needed.
//!  - `backend`: btleplug transport (scan, connect, GATT) and, on Linux, BlueZ pairing.
//!  - `diag`: plain-language reasons Bluetooth is unusable (off, blocked, no adapter, no bluetoothd).
pub mod backend;
pub mod client;
pub mod diag;

pub use client::{Device, LinkError, LinkIo};
