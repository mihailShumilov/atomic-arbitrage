//! Names and directory layout of everything the recorder keeps in `--out-dir`.
//!
//! Hourly data files: `<out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst` (UTC hour of
//! the receive time). This module is the only place that knows the scheme:
//! [`hour_path`] builds it, [`list_feed_files`] walks it.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

/// Highest seq that is fsynced to disk (atomic replace).
pub const STATE_FILE: &str = "last_seq.txt";
/// Missing L2 blocks: `from \t to \t recv_ns`.
pub const GAPS_FILE: &str = "gaps.tsv";
/// Torn zstd tails cut off after a crash.
pub const TORN_DIR: &str = "_torn";

const FILE_PREFIX: &str = "feed-";
const FILE_SUFFIX: &str = ".tsv.zst";
/// `YYYY/MM/DD` below the root.
const DIR_DEPTH: u32 = 3;

/// Identity of the hourly file for time `t` (`YYYYMMDDHH`).
pub fn hour_key(t: DateTime<Utc>) -> String {
    t.format("%Y%m%d%H").to_string()
}

/// Path of the hourly data file for time `t`.
pub fn hour_path(root: &Path, t: DateTime<Utc>) -> PathBuf {
    root.join(t.format("%Y/%m/%d").to_string()).join(format!("{FILE_PREFIX}{}{FILE_SUFFIX}", t.format("%Y%m%d-%H")))
}

/// All hourly feed files under `root`, oldest first. Skips `_torn` and other
/// underscore- or dot-prefixed entries.
pub fn list_feed_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('_') || name.starts_with('.') {
                continue;
            }
            if p.is_dir() && depth < DIR_DEPTH {
                walk(&p, depth + 1, out);
            } else if depth == DIR_DEPTH && name.starts_with(FILE_PREFIX) && name.ends_with(FILE_SUFFIX) {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, 0, &mut v);
    v.sort();
    v
}

/// Unix ns of the newest modification among the hourly feed files (the two
/// newest by name are enough: older hours are closed). None without data.
/// Used as the end of the previous session when connections.tsv has nothing
/// after the last `connected` (kill -9, task 012 item 1). Must be read before
/// crash recovery: truncating a torn tail updates the mtime.
pub fn newest_data_mtime_ns(root: &Path) -> Option<u128> {
    list_feed_files(root)
        .iter()
        .rev()
        .take(2)
        .filter_map(|p| fs::metadata(p).ok()?.modified().ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .max()
}

/// Contents of `last_seq.txt`, if present and a number.
pub fn read_state(out: &Path) -> Option<u64> {
    fs::read_to_string(out.join(STATE_FILE)).ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hour_path_is_listed_and_others_are_not() {
        let root = std::env::temp_dir().join(format!("recorder-layout-{}-{}", std::process::id(), crate::now_ns()));
        let t = DateTime::from_timestamp(1_790_769_600, 0).unwrap(); // 2026-09-30T12:00Z
        let p = hour_path(&root, t);
        assert_eq!(p, root.join("2026/09/30/feed-20260930-12.tsv.zst"));
        assert_eq!(hour_key(t), "2026093012");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"").unwrap();
        fs::create_dir_all(root.join(TORN_DIR)).unwrap();
        fs::write(root.join(TORN_DIR).join("feed-20260930-12.tsv.zst"), b"").unwrap();
        fs::write(root.join("2026/09/30/.feed-20260930-13.tsv.zst"), b"").unwrap();
        fs::write(root.join("feed-20260930-14.tsv.zst"), b"").unwrap(); // wrong depth
        assert_eq!(list_feed_files(&root), vec![p]);
        fs::remove_dir_all(&root).ok();
    }
}
