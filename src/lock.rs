//! `rrecall.lock` (spec §4.2), the store's run lock (§4.1) and the UTC times a run writes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const LOCK_FILE: &str = "rrecall.lock";

/// The lock file: per collector its last settled run, per check, judge and view its plugin.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LockFile {
    #[serde(default)]
    pub collectors: BTreeMap<String, CollectorLock>,
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginLock>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectorLock {
    /// The combined fingerprint of the last settled run (`sha256:<hex>`).
    pub fingerprint: String,
    /// The plugin directory: relative to the store when inside it, else absolute.
    pub plugin: String,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Only for a `min_interval` collector, so a fingerprinted one rewrites nothing when it re-settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<String>,
    /// The latest attempt did not settle (spec §4.2, I10): the collector is due on the next run
    /// whatever its fingerprint or interval, while the fields above keep the last settled run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unsettled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginLock {
    pub plugin: String,
    pub hash: String,
}

impl LockFile {
    /// The store's lock file; an absent one is empty.
    pub fn load(store: &Path) -> Result<LockFile> {
        match std::fs::read_to_string(store.join(LOCK_FILE)) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("{LOCK_FILE} does not parse"))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LockFile::default()),
            Err(e) => Err(e).with_context(|| format!("reading {LOCK_FILE}")),
        }
    }

    /// Two-space JSON, keys in a fixed order, a final newline.
    pub fn render(&self) -> String {
        // Strings, maps and structs only: serializing cannot fail.
        let mut text = serde_json::to_string_pretty(self).unwrap_or_default();
        text.push('\n');
        text
    }
}

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `YYYY-MM-DDTHH:MM:SSZ`.
pub fn fmt_utc(secs: u64) -> String {
    let (y, m, d, hh, mm, ss) = civil(secs);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Year, month, day, hour, minute, second of a Unix time (Howard Hinnant's civil_from_days).
fn civil(secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let (days, rem) = (secs / 86400, secs % 86400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// The inverse of [`fmt_utc`]; `None` for anything else.
pub fn parse_utc(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    crate::node::check_date(&s[..10]).ok()?;
    let num = |r: std::ops::Range<usize>| -> Option<u64> {
        let p = &s[r];
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hh, mm, ss) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if hh > 23 || mm > 59 || ss > 59 || y < 1970 {
        return None;
    }
    // days_from_civil, the inverse of `civil`.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y / 400;
    let yoe = y % 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = (era * 146_097 + doe).checked_sub(719_468)?;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// The run id: `YYYY-MM-DDTHHMMSSZ`, which sorts by time.
pub fn run_id(secs: u64) -> String {
    fmt_utc(secs).replace(':', "")
}

/// The time in a run's commit subject: `YYYY-MM-DDTHH:MMZ`.
pub fn subject_time(secs: u64) -> String {
    let t = fmt_utc(secs);
    format!("{}Z", &t[..16])
}

/// An exclusive lock on a store, held until dropped (or the process dies).
pub struct StoreLock(#[allow(dead_code)] std::fs::File);

/// Takes the store's run lock: waits for it, or returns `None` when another process holds it
/// and `wait` is false.
pub fn acquire(store: &Path, wait: bool) -> Result<Option<StoreLock>> {
    use std::os::fd::AsRawFd;
    // Inside `.git`, so the lock never shows in the store's working tree.
    let path = store.join(".git").join("rrecall-run.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    let op = if wait {
        libc::LOCK_EX
    } else {
        libc::LOCK_EX | libc::LOCK_NB
    };
    loop {
        // SAFETY: flock takes the descriptor of a file this function owns and an integer.
        if unsafe { libc::flock(file.as_raw_fd(), op) } == 0 {
            return Ok(Some(StoreLock(file)));
        }
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) if !wait => return Ok(None),
            _ => return Err(e).with_context(|| format!("locking {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn utc_times_round_trip() {
        assert_eq!(fmt_utc(0), "1970-01-01T00:00:00Z");
        // 2026-11-25T09:14:05Z
        let t = 1_795_598_045;
        assert_eq!(fmt_utc(t), "2026-11-25T09:14:05Z");
        assert_eq!(run_id(t), "2026-11-25T091405Z");
        assert_eq!(subject_time(t), "2026-11-25T09:14Z");
        assert_eq!(parse_utc("2026-11-25T09:14:05Z"), Some(t));
        assert_eq!(fmt_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(parse_utc("2000-02-29T00:00:00Z"), Some(951_782_400));
        for bad in [
            "",
            "2026-11-25",
            "2026-11-25T09:14:05",
            "2026-13-01T00:00:00Z",
            "x026-11-25T09:14:05Z",
            "2026-11-25T25:00:00Z",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad}");
        }
    }

    #[test]
    fn lock_file_renders_in_a_fixed_order_and_loads_back() {
        let t = TempDir::new();
        assert_eq!(LockFile::load(&t.0).unwrap(), LockFile::default());
        let mut l = LockFile::default();
        l.collectors.insert(
            "repo".into(),
            CollectorLock {
                fingerprint: "sha256:aa".into(),
                plugin: "plugins/files".into(),
                hash: "sha256:bb".into(),
                runner: None,
                model: Some("m".into()),
                settled_at: None,
                unsettled: false,
            },
        );
        l.plugins.insert(
            "no-op".into(),
            PluginLock {
                plugin: "/p/no-op".into(),
                hash: "sha256:cc".into(),
            },
        );
        let text = l.render();
        assert_eq!(
            text,
            "{\n  \"collectors\": {\n    \"repo\": {\n      \"fingerprint\": \"sha256:aa\",\n      \"plugin\": \"plugins/files\",\n      \"hash\": \"sha256:bb\",\n      \"model\": \"m\"\n    }\n  },\n  \"plugins\": {\n    \"no-op\": {\n      \"plugin\": \"/p/no-op\",\n      \"hash\": \"sha256:cc\"\n    }\n  }\n}\n"
        );
        std::fs::write(t.0.join(LOCK_FILE), &text).unwrap();
        assert_eq!(LockFile::load(&t.0).unwrap(), l);
        std::fs::write(t.0.join(LOCK_FILE), "{\"colour\":1}").unwrap();
        let e = format!("{:#}", LockFile::load(&t.0).unwrap_err());
        assert!(e.contains(LOCK_FILE), "{e}");
    }

    #[test]
    fn the_store_lock_is_exclusive() {
        let t = TempDir::new();
        std::fs::create_dir_all(t.0.join(".git")).unwrap();
        let held = acquire(&t.0, false).unwrap();
        assert!(held.is_some());
        // flock locks belong to the open file, so a second open in this process conflicts too.
        assert!(acquire(&t.0, false).unwrap().is_none());
        drop(held);
        assert!(acquire(&t.0, false).unwrap().is_some());
    }
}
