//! Shared ownership, local authenticated IPC, durable receipts and MCP stdio.
//! No device operations, credentials, model calls, or GUI dependencies live here.
pub mod broker;
pub mod mcp;
pub mod runs;
pub mod tools;

use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn nonce() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "OS random source unavailable".to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Locks are per OS user, independent of config paths. Never unlink a lock file:
/// waiters must continue to address the same inode/file object.
pub struct ResourceClaim {
    _file: File,
}
impl ResourceClaim {
    /// Input links share this claim. Firmware writes take it exclusively.
    pub fn hardware(shared: bool) -> Result<Self, String> {
        let root = dirs::data_local_dir()
            .ok_or("No per-user data directory")?
            .join("kvm-it")
            .join("ownership");
        Self::at_mode(&root, "hardware", "administration", shared)
    }
    pub fn acquire(kind: &str, identity: &str) -> Result<Self, String> {
        let root = dirs::data_local_dir()
            .ok_or("No per-user data directory")?
            .join("kvm-it")
            .join("ownership");
        Self::at(&root, kind, identity)
    }
    pub fn at(root: &Path, kind: &str, identity: &str) -> Result<Self, String> {
        Self::at_mode(root, kind, identity, false)
    }
    fn at_mode(root: &Path, kind: &str, identity: &str, shared: bool) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let identity = if kind == "adapter" || cfg!(windows) {
            identity.to_lowercase()
        } else {
            identity.to_owned()
        };
        let path = root.join(format!(
            "{}.lock",
            digest(format!("{kind}\0{identity}").as_bytes())
        ));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        (if shared { file.try_lock_shared() } else { file.try_lock() }).map_err(|_| format!("This {kind} is owned by another kvm-it connection. Use its console or disconnect it first."))?;
        Ok(Self { _file: file })
    }
}

pub fn control_dir(config: &Path) -> PathBuf {
    config.with_extension("control")
}

/// Create private files rather than making permissions restrictive after writing a token.
pub fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // On Windows this inherits the per-user config directory's ACL.
    options.open(path)
}

pub fn private_dir(path: &Path) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn detached_workers_retain_claims_and_firmware_excludes_input() {
        let root = std::env::temp_dir().join(nonce().unwrap());
        let wrapper = std::sync::Arc::new(ResourceClaim::at(&root, "capture", "card").unwrap());
        let worker = wrapper.clone();
        drop(wrapper);
        assert!(ResourceClaim::at(&root, "capture", "card").is_err());
        drop(worker);
        assert!(ResourceClaim::at(&root, "capture", "card").is_ok());
        let first = ResourceClaim::at_mode(&root, "hardware", "bench", true).unwrap();
        let second = ResourceClaim::at_mode(&root, "hardware", "bench", true).unwrap();
        assert!(ResourceClaim::at_mode(&root, "hardware", "bench", false).is_err());
        drop((first, second));
        let flash = ResourceClaim::at_mode(&root, "hardware", "bench", false).unwrap();
        assert!(ResourceClaim::at_mode(&root, "hardware", "bench", true).is_err());
        drop(flash);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn resource_claims_exclude_even_the_same_process_and_release_on_drop() {
        let root = std::env::temp_dir().join(nonce().unwrap());
        let held = ResourceClaim::at(&root, "adapter", "AA:BB").unwrap();
        assert!(ResourceClaim::at(&root, "adapter", "aa:bb").is_err());
        assert!(ResourceClaim::at(&root, "adapter", "another").is_ok());
        drop(held);
        assert!(ResourceClaim::at(&root, "adapter", "aa:bb").is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
