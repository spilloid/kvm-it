//! Persistent app settings. Holds device ids and paths only — never secrets (docs/security.md).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

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
    /// Light, dark, or follow the system (the default).
    #[serde(default)]
    pub theme: crate::theme::ThemeChoice,
}

pub fn config_path() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("kvm-it").join("config.json")
}

impl Config {
    pub fn load() -> Config {
        Config::load_from(&config_path())
    }
    /// Change settings: `f` is applied to the file's current contents (not this possibly stale copy), which are saved,
    /// and then to this copy, so newer settings saved by another `kvmit` process (one per adapter, or the GUI) survive.
    /// Not locked: two updates at the same instant can still race, but only over one read and one write.
    pub fn update(&mut self, f: impl Fn(&mut Config)) {
        self.update_at(&config_path(), f)
    }
    fn update_at(&mut self, p: &Path, f: impl Fn(&mut Config)) {
        let mut fresh = base_for_update(read_config(p), self);
        f(&mut fresh);
        fresh.save_to(p);
        f(self);
    }
    fn load_from(p: &Path) -> Config {
        read_config(p).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    /// Write a temp file unique to this save and rename it over the config: several `kvmit` processes (one per
    /// adapter) may save at once, and a torn file would make `load` silently fall back to defaults.
    fn save_to(&self, p: &Path) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let tmp = p.with_extension(format!("json.{}.{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        if std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default()).is_err() || retry_while_replaced(|| std::fs::rename(&tmp, p)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
    pub fn script_dir(&self) -> PathBuf {
        self.script_dir.clone().unwrap_or_else(|| default_script_dir(dirs::document_dir(), dirs::home_dir()))
    }
}

/// `<Documents>/kvm-it/scripts` on every platform. Linux reports a Documents folder only when `xdg-user-dirs` has set one up
/// (minimal and server installs often have none); the folder then is `~/Documents/kvm-it/scripts` as documented, never a
/// path relative to wherever the app was started from.
fn default_script_dir(documents: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    documents
        .or_else(|| home.map(|h| h.join("Documents")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kvm-it")
        .join("scripts")
}

/// Windows: opening (or renaming onto) the config at the instant another `kvmit` process renames its new copy over it fails
/// with "Access is denied" (os error 5) while the replaced file is pending deletion; reproduced by
/// `concurrent_saves_never_leave_a_torn_file` on Windows 11 (CI never runs the tests on Windows). The same test also saw a
/// read report "not found" for an instant during the swap, which `update` would take for "no config yet". Both last
/// microseconds, so retry briefly. The limits are wall-clock time, not a retry count: Windows rounds short sleeps up to its
/// ~15.6 ms timer tick, so a count multiplied that into seconds per read under contention. Worst case per call: 100 ms
/// (a genuinely missing file costs 20 ms once, on first run). Elsewhere a rename is atomic for readers and both errors are
/// returned at once.
fn retry_while_replaced<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    use std::io::ErrorKind::{NotFound, PermissionDenied};
    use std::time::{Duration, Instant};
    let start = Instant::now();
    loop {
        let waited = start.elapsed();
        match op() {
            Err(e) if cfg!(windows) && e.kind() == PermissionDenied && waited < Duration::from_millis(100) => {}
            Err(e) if cfg!(windows) && e.kind() == NotFound && waited < Duration::from_millis(20) => {}
            r => return r,
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn read_config(p: &Path) -> std::io::Result<String> {
    retry_while_replaced(|| std::fs::read_to_string(p))
}

/// What an update starts from: the file's current settings; defaults when there is no file yet (or it is not valid JSON,
/// which the save then replaces); and this process's own copy when the file exists but could not be read, so a read
/// failure can never write defaults over everyone's settings.
fn base_for_update(read: std::io::Result<String>, current: &Config) -> Config {
    match read {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(_) => current.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scripts_folder_is_never_relative_to_the_working_directory_when_a_home_exists() {
        let home = PathBuf::from("/home/tech");
        assert_eq!(default_script_dir(Some(PathBuf::from("/home/tech/Dokumente")), Some(home.clone())), PathBuf::from("/home/tech/Dokumente/kvm-it/scripts"));
        // Linux without xdg-user-dirs: dirs::document_dir() is None
        assert_eq!(default_script_dir(None, Some(home)), PathBuf::from("/home/tech/Documents/kvm-it/scripts"));
        assert!(default_script_dir(None, None).is_relative(), "only with no home at all");
    }

    #[test]
    fn an_unreadable_config_is_never_replaced_with_defaults() {
        let mine = Config { last_device: Some("AA:BB".into()), theme: crate::theme::ThemeChoice::Dark, ..Default::default() };
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(base_for_update(Err(denied), &mine), mine, "unreadable: keep this process's settings");
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(base_for_update(Err(missing), &mine), Config::default(), "no file yet");
        assert_eq!(base_for_update(Ok("{\"last_device\":\"CC\"}".into()), &mine).last_device.as_deref(), Some("CC"), "the file wins");
    }

    #[test]
    fn round_trips_and_ignores_unknown_fields() {
        let c = Config { last_device: Some("AA:BB".into()), last_video: None, last_video_key: Some("Cam @ usb-1".into()), script_dir: None, theme: crate::theme::ThemeChoice::Dark };
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

    fn temp_config(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kvmit-cfg-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("config.json")
    }

    #[test]
    fn concurrent_saves_never_leave_a_torn_file() {
        let p = temp_config("torn");
        Config::default().save_to(&p);
        let (reading, saves) = (std::sync::atomic::AtomicBool::new(true), AtomicU64::new(0));
        std::thread::scope(|s| {
            for i in 0..8 {
                let (p, reading, saves) = (&p, &reading, &saves);
                s.spawn(move || {
                    // keep saving until the reader is done, so its reads run while saves are going on
                    while reading.load(Ordering::SeqCst) {
                        Config { last_device: Some(format!("dev-{i}")), ..Default::default() }.save_to(p);
                        saves.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
            // stops the writers even if a read below panics, or the scope would wait for them forever
            struct Stop<'a>(&'a std::sync::atomic::AtomicBool);
            impl Drop for Stop<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            let _stop = Stop(&reading);
            while saves.load(Ordering::SeqCst) == 0 {
                std::thread::yield_now();
            }
            for _ in 0..500 {
                let body = read_config(&p).expect("config readable during saves");
                assert!(serde_json::from_str::<Config>(&body).is_ok(), "torn config: {body:?}");
            }
        });
        assert!(Config::load_from(&p).last_device.is_some_and(|d| d.starts_with("dev-")));
        let leftovers = std::fs::read_dir(p.parent().unwrap()).unwrap().count();
        assert_eq!(leftovers, 1, "temp files left behind");
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn an_update_keeps_settings_another_process_saved_since() {
        let p = temp_config("update");
        let mut gui = Config::default();
        gui.save_to(&p);
        // another process (a CLI run) records its adapter after the GUI loaded its copy
        Config { last_device: Some("AA:BB".into()), ..Default::default() }.save_to(&p);
        gui.update_at(&p, |c| c.theme = crate::theme::ThemeChoice::Dark);
        let on_disk = Config::load_from(&p);
        assert_eq!(on_disk.last_device.as_deref(), Some("AA:BB"));
        assert_eq!(on_disk.theme, crate::theme::ThemeChoice::Dark);
        assert_eq!(gui.theme, crate::theme::ThemeChoice::Dark);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
