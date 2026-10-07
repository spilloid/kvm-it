//! Persistent app settings. Holds device ids and paths only — never secrets (docs/security.md).
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    /// Bluetooth address of the last adapter used; reconnects automatically.
    #[serde(default)]
    pub last_device: Option<String>,
    /// Last capture device's path (`/dev/videoN`): kept for older configs and as a fallback; not stable across replugs.
    #[serde(default)]
    pub last_video: Option<String>,
    /// Stable identity of the last capture device (card name + bus info); preferred over `last_video`.
    #[serde(default)]
    pub last_video_key: Option<String>,
    #[serde(default)]
    pub script_dir: Option<PathBuf>,
}

pub fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("kvm-it").join("config.json")
}

impl Config {
    pub fn load() -> Config {
        std::fs::read_to_string(config_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self) {
        let p = config_path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
    }
    pub fn script_dir(&self) -> PathBuf {
        self.script_dir
            .clone()
            .unwrap_or_else(|| dirs::document_dir().unwrap_or_else(|| PathBuf::from(".")).join("kvm-it").join("scripts"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_ignores_unknown_fields() {
        let c = Config { last_device: Some("AA:BB".into()), last_video: None, last_video_key: Some("Cam @ usb-1".into()), script_dir: None };
        let s = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&s).unwrap(), c);
        assert!(serde_json::from_str::<Config>("{\"last_device\":\"x\",\"future\":1}").is_ok());
        assert!(!s.to_lowercase().contains("password"));
    }

    #[test]
    fn a_config_written_before_device_keys_still_loads() {
        let c: Config = serde_json::from_str("{\"last_device\":\"AA:BB\",\"last_video\":\"/dev/video2\"}").unwrap();
        assert_eq!(c.last_video.as_deref(), Some("/dev/video2"));
        assert_eq!(c.last_video_key, None);
    }
}
