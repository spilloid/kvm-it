//! Pure output naming: UTC timestamp, never overwrites an existing file.
use crate::args::Format;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// `YYYYMMDD-HHMMSS` (UTC) for a unix time in seconds.
pub fn stamp_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn stamp_now() -> String {
    stamp_from_unix(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    )
}

/// First of `kvmit-<stamp>.<ext>`, `kvmit-<stamp>-2.<ext>`, ... for which `exists` is false.
pub fn unique_path(
    dir: &Path,
    stamp: &str,
    format: Format,
    exists: impl Fn(&Path) -> bool,
) -> PathBuf {
    let ext = format.extension();
    let first = dir.join(format!("kvmit-{stamp}.{ext}"));
    if !exists(&first) {
        return first;
    }
    (2u32..)
        .map(|n| dir.join(format!("kvmit-{stamp}-{n}.{ext}")))
        .find(|p| !exists(p))
        .expect("unbounded range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps() {
        assert_eq!(stamp_from_unix(0), "19700101-000000");
        assert_eq!(stamp_from_unix(951_782_400 + 86_399), "20000229-235959"); // leap day
        assert_eq!(stamp_from_unix(1_791_323_351), "20261006-214911");
    }

    #[test]
    fn collision_suffix() {
        let d = Path::new("/r");
        let taken = [d.join("kvmit-S.gif"), d.join("kvmit-S-2.gif")];
        let p = unique_path(d, "S", Format::Gif, |p| taken.iter().any(|t| t == p));
        assert_eq!(p, d.join("kvmit-S-3.gif"));
        assert_eq!(
            unique_path(d, "S", Format::WebM, |_| false),
            d.join("kvmit-S.webm")
        );
    }
}
