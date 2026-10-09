//! Plain-language reasons why Bluetooth cannot be used, instead of a bare "no adapter" or a D-Bus error. The decisions
//! are pure functions (unit-tested); reading `/sys` and asking the OS are thin wrappers next to them.

pub const NO_ADAPTER: &str = "No Bluetooth adapter found: this computer has none, it is disabled, or (in a virtual machine) it is not passed through";
pub const POWERED_OFF: &str = "Bluetooth is turned off: turn it on in the system's Bluetooth settings";
pub const RFKILL_HARD: &str =
    "Bluetooth is switched off by a hardware switch, a function key or the firmware (rfkill hard block): turn it on there";
pub const RFKILL_SOFT: &str =
    "Bluetooth is blocked (rfkill soft block, often airplane mode): turn Bluetooth on in the system settings, or run `rfkill unblock bluetooth`";
pub const NO_DAEMON: &str =
    "The Bluetooth service (bluetoothd) is not running: start it with `sudo systemctl enable --now bluetooth` (package `bluez`)";

/// One `/sys/class/rfkill` entry of type `bluetooth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rfkill {
    pub soft: bool,
    pub hard: bool,
}

/// Linux: the likely reason a BlueZ call failed, from the Bluetooth controllers the kernel lists (`None` when that
/// could not be read, e.g. in a sandbox), their rfkill state and the raw error. `None` means no better explanation
/// than the raw error. A block outranks everything because it causes all the other symptoms, but only when every
/// Bluetooth radio is blocked: with a second, working radio (a USB dongle beside a disabled internal one) the block is
/// not the reason.
pub fn linux_reason(controllers: Option<usize>, rfkill: &[Rfkill], raw: &str) -> Option<&'static str> {
    if !rfkill.is_empty() && rfkill.iter().all(|r| r.soft || r.hard) {
        return Some(if rfkill.iter().any(|r| r.hard) { RFKILL_HARD } else { RFKILL_SOFT });
    }
    if controllers == Some(0) {
        return Some(NO_ADAPTER);
    }
    // bluer/D-Bus wording when nothing owns the org.bluez name.
    if ["ServiceUnknown", "NameHasNoOwner", "org.bluez was not provided"].iter().any(|p| raw.contains(p)) {
        return Some(NO_DAEMON);
    }
    None
}

/// The message shown for a failure: the reason first, the raw error kept for bug reports.
pub fn explain(reason: Option<&str>, raw: &str) -> String {
    match reason {
        Some(r) => format!("{r} ({raw})"),
        None => raw.to_string(),
    }
}

/// Linux: explain a BlueZ failure using what `/sys` shows right now.
#[cfg(target_os = "linux")]
pub fn linux_explain(raw: &str) -> String {
    let controllers = std::fs::read_dir("/sys/class/bluetooth")
        .ok()
        .map(|d| d.flatten().filter(|e| e.file_name().to_string_lossy().starts_with("hci")).count());
    let flag = |p: std::path::PathBuf| std::fs::read_to_string(p).is_ok_and(|s| s.trim() == "1");
    let rfkill: Vec<Rfkill> = std::fs::read_dir("/sys/class/rfkill")
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| std::fs::read_to_string(p.join("type")).is_ok_and(|t| t.trim() == "bluetooth"))
                .map(|p| Rfkill { soft: flag(p.join("soft")), hard: flag(p.join("hard")) })
                .collect()
        })
        .unwrap_or_default();
    explain(linux_reason(controllers, &rfkill, raw), raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DBUS: &str = "D-Bus error: org.freedesktop.DBus.Error.ServiceUnknown: The name org.bluez was not provided by any .service files";

    #[test]
    fn blocks_outrank_everything() {
        let hard = [Rfkill { soft: true, hard: true }];
        assert_eq!(linux_reason(Some(0), &hard, DBUS), Some(RFKILL_HARD));
        let soft = [Rfkill { soft: true, hard: false }, Rfkill { soft: true, hard: false }];
        assert_eq!(linux_reason(Some(2), &soft, "Blocked through rfkill"), Some(RFKILL_SOFT));
    }

    #[test]
    fn a_working_second_radio_is_not_blamed_on_the_blocked_one() {
        let mixed = [Rfkill { soft: true, hard: true }, Rfkill { soft: false, hard: false }];
        assert_eq!(linux_reason(Some(2), &mixed, "Page Timeout"), None);
        assert_eq!(linux_reason(Some(2), &mixed, DBUS), Some(NO_DAEMON));
    }

    #[test]
    fn no_controller_before_daemon() {
        assert_eq!(linux_reason(Some(0), &[], DBUS), Some(NO_ADAPTER));
        assert_eq!(linux_reason(Some(1), &[], DBUS), Some(NO_DAEMON));
        // unreadable /sys: still recognise the daemon from the error
        assert_eq!(linux_reason(None, &[], "org.freedesktop.DBus.Error.NameHasNoOwner"), Some(NO_DAEMON));
    }

    #[test]
    fn unknown_failures_keep_the_raw_error() {
        let ok = [Rfkill { soft: false, hard: false }];
        assert_eq!(linux_reason(Some(1), &ok, "Page Timeout"), None);
        assert_eq!(explain(None, "Page Timeout"), "Page Timeout");
        assert_eq!(explain(Some(NO_DAEMON), "x"), format!("{NO_DAEMON} (x)"));
    }
}
