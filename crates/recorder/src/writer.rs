//! Durable hourly writer and crash recovery (task 002, items 1, 2, 6).
//!
//! Durability model:
//! - The hourly file `<out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst` is a sequence
//!   of independent zstd frames (with content checksums). A frame is closed at
//!   least every `frame_max` (60 s by default), on hour rotation and on
//!   shutdown. Frames always end on a line boundary.
//! - Commit order: finish frame -> fsync data file -> append pending gap rows
//!   to gaps.tsv + fsync -> write last_seq.txt atomically (tmp, fsync, rename,
//!   fsync dir). So `last_seq.txt` and `gaps.tsv` never get ahead of data that
//!   is actually on disk.
//! - After `kill -9` at most the open frame (<= frame_max of data) is lost.
//!   On start, [`recover`] truncates the torn tail of the newest files back
//!   to the last complete frame, keeps the cut bytes in `<out>/_torn/`, and
//!   derives the resume point from the data itself.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use hood_core::detect_gap;
use tracing::{info, warn};

use crate::route::Line;

pub const STATE_FILE: &str = "last_seq.txt";
pub const GAPS_FILE: &str = "gaps.tsv";
pub const TORN_DIR: &str = "_torn";

// ------------------------------------------------------------- fs helpers ---

/// fsync a directory so a rename/create inside it is durable. Best effort:
/// some platforms refuse to open directories for sync.
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Atomically replace `path` with `contents`: tmp file, fsync, rename, fsync dir.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("state")
    ));
    {
        let mut f = File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    sync_dir(dir);
    Ok(())
}

pub fn read_state(out: &Path) -> Option<u64> {
    fs::read_to_string(out.join(STATE_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

// --------------------------------------------------------------- recovery ---

/// Length of the longest prefix of `data` made of complete, checksum-valid
/// zstd frames. Anything after it is a torn tail.
pub fn valid_prefix_len(data: &[u8]) -> usize {
    let mut pos = 0usize;
    while pos < data.len() {
        let rest = &data[pos..];
        let n = match zstd::zstd_safe::find_frame_compressed_size(rest) {
            Ok(n) if n > 0 && n <= rest.len() => n,
            _ => break,
        };
        // Structural walk is not enough: decode to verify blocks + checksum.
        if zstd::stream::decode_all(&rest[..n]).is_err() {
            break;
        }
        pos += n;
    }
    pos
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TornRepair {
    pub file: PathBuf,
    pub kept_bytes: u64,
    pub torn_bytes: u64,
    pub saved_to: PathBuf,
}

/// If `path` ends with a torn zstd frame, copy the tail to `torn_dir` and
/// truncate the file to the last complete frame. The tail is never deleted.
pub fn repair_torn(path: &Path, torn_dir: &Path, stamp: &str) -> Result<Option<TornRepair>> {
    let data = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let keep = valid_prefix_len(&data);
    if keep == data.len() {
        return Ok(None);
    }
    fs::create_dir_all(torn_dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("feed");
    let saved_to = torn_dir.join(format!("{name}.at{keep}.{stamp}.torn"));
    {
        let mut f =
            File::create(&saved_to).with_context(|| format!("create {}", saved_to.display()))?;
        f.write_all(&data[keep..])?;
        f.sync_all()?;
    }
    sync_dir(torn_dir);
    let f = OpenOptions::new().write(true).open(path)?;
    f.set_len(keep as u64)?;
    f.sync_all()?;
    Ok(Some(TornRepair {
        file: path.to_path_buf(),
        kept_bytes: keep as u64,
        torn_bytes: (data.len() - keep) as u64,
        saved_to,
    }))
}

/// All hourly feed files under `root`, oldest first. Skips `_torn` and other
/// underscore-prefixed directories.
pub fn list_feed_files(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('_') || name.starts_with('.') {
                continue;
            }
            if p.is_dir() && depth < 3 {
                walk(&p, depth + 1, out);
            } else if depth == 3 && name.starts_with("feed-") && name.ends_with(".tsv.zst") {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, 0, &mut v);
    v.sort();
    v
}

/// Highest `seq_last` among lines of a (repaired) feed file; None if the file
/// has no sequenced lines.
pub fn max_seq_in_file(path: &Path) -> Result<Option<u64>> {
    let f = File::open(path)?;
    // Empty file (created, or cut back to zero after a crash): the zstd
    // reader would report "incomplete frame".
    if f.metadata()?.len() == 0 {
        return Ok(None);
    }
    // zstd's reader decodes concatenated frames by default.
    let dec = zstd::stream::read::Decoder::new(f)?;
    let mut best: Option<u64> = None;
    for line in BufReader::with_capacity(1 << 20, dec).split(b'\n') {
        let line = line?;
        let mut cols = line.splitn(4, |&b| b == b'\t');
        let _recv = cols.next();
        let _first = cols.next();
        let Some(last) = cols.next() else { continue };
        let Some(s) = std::str::from_utf8(last)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        if s > 0 {
            best = Some(best.map_or(s, |b| b.max(s)));
        }
    }
    Ok(best)
}

#[derive(Debug, Default)]
pub struct Recovery {
    pub repairs: Vec<TornRepair>,
    pub state_seq: Option<u64>,
    pub data_seq: Option<u64>,
    /// Where to resume gap detection from.
    pub resume_seq: Option<u64>,
}

/// Start-up recovery: repair torn tails of the newest two hourly files, then
/// derive the resume point. Data wins over `last_seq.txt`: if the state lags
/// (killed between fsync and state write) we would otherwise report a fake
/// gap; if the state leads (must not happen) we would hide a real one.
pub fn recover(out: &Path) -> Result<Recovery> {
    let files = list_feed_files(out);
    let torn_dir = out.join(TORN_DIR);
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut rec = Recovery {
        state_seq: read_state(out),
        ..Default::default()
    };
    for f in files.iter().rev().take(2) {
        if let Some(r) = repair_torn(f, &torn_dir, &stamp)? {
            warn!(
                file = %r.file.display(), kept = r.kept_bytes, torn = r.torn_bytes,
                saved = %r.saved_to.display(), "truncated torn zstd tail"
            );
            rec.repairs.push(r);
        }
    }
    for f in files.iter().rev().take(3) {
        if let Some(s) = max_seq_in_file(f)? {
            rec.data_seq = Some(s);
            break;
        }
    }
    rec.resume_seq = match (rec.data_seq, rec.state_seq) {
        (Some(d), Some(s)) if d != s => {
            warn!(
                data = d,
                state = s,
                "last_seq.txt disagrees with data, using data"
            );
            Some(d)
        }
        (Some(d), _) => Some(d),
        (None, s) => s,
    };
    // Make the state file match what is really on disk.
    if let Some(s) = rec.resume_seq {
        if rec.state_seq != Some(s) {
            write_atomic(&out.join(STATE_FILE), &s.to_string())?;
        }
    }
    Ok(rec)
}

// ----------------------------------------------------------------- writer ---

enum Slot {
    /// File open, no frame in progress.
    Idle(File),
    /// A zstd frame is being written.
    Frame(zstd::Encoder<'static, BufWriter<File>>),
    /// Transitional value while moving between the two.
    Empty,
}

struct HourFile {
    key: String,
    slot: Slot,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WriterStats {
    pub lines: u64,
    pub unsequenced: u64,
    pub dup_skipped: u64,
    pub gaps: u64,
    pub intra_gaps: u64,
    pub intra_disorder: u64,
    pub frames: u64,
}

pub struct FeedWriter {
    root: PathBuf,
    level: i32,
    frame_max: Duration,
    cur: Option<HourFile>,
    frame_opened: Option<Instant>,
    /// Highest seq accepted (may not be durable yet).
    last_seq: Option<u64>,
    /// Highest seq known to be fsynced and recorded in last_seq.txt.
    durable_seq: Option<u64>,
    pending_gaps: Vec<String>,
    pub stats: WriterStats,
}

impl FeedWriter {
    pub fn new(root: PathBuf, level: i32, frame_max: Duration, resume_seq: Option<u64>) -> Self {
        Self {
            root,
            level,
            frame_max,
            cur: None,
            frame_opened: None,
            last_seq: resume_seq,
            durable_seq: resume_seq,
            pending_gaps: Vec::new(),
            stats: WriterStats::default(),
        }
    }

    pub fn durable_seq(&self) -> Option<u64> {
        self.durable_seq
    }

    fn path_for(&self, t: DateTime<Utc>) -> PathBuf {
        self.root
            .join(t.format("%Y/%m/%d").to_string())
            .join(format!("feed-{}.tsv.zst", t.format("%Y%m%d-%H")))
    }

    fn ensure_hour(&mut self, t: DateTime<Utc>) -> Result<()> {
        let key = t.format("%Y%m%d%H").to_string();
        if self.cur.as_ref().is_some_and(|c| c.key == key) {
            return Ok(());
        }
        // Hour changed: make everything so far durable before switching.
        self.commit()?;
        self.cur = None;
        let path = self.path_for(t);
        let dir = path.parent().expect("hourly path has a parent");
        fs::create_dir_all(dir)?;
        // Append: after a restart within the same hour we add new frames.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        sync_dir(dir);
        info!(file = %path.display(), "writing");
        self.cur = Some(HourFile {
            key,
            slot: Slot::Idle(file),
        });
        Ok(())
    }

    fn encoder(&mut self) -> Result<&mut zstd::Encoder<'static, BufWriter<File>>> {
        let cur = self.cur.as_mut().expect("hour file open");
        if let Slot::Idle(_) = cur.slot {
            let Slot::Idle(file) = std::mem::replace(&mut cur.slot, Slot::Empty) else {
                unreachable!()
            };
            let mut enc = zstd::Encoder::new(BufWriter::with_capacity(1 << 16, file), self.level)?;
            enc.include_checksum(true)?;
            cur.slot = Slot::Frame(enc);
            self.frame_opened = Some(Instant::now());
        }
        match &mut cur.slot {
            Slot::Frame(enc) => Ok(enc),
            _ => anyhow::bail!("writer in inconsistent state"),
        }
    }

    fn write_line(&mut self, l: &Line) -> Result<()> {
        let t: DateTime<Utc> = DateTime::from_timestamp_nanos(l.recv_ns as i64);
        self.ensure_hour(t)?;
        // Feed JSON is compact; guard against stray newlines anyway.
        let raw = l.raw.replace(['\n', '\r'], "");
        let enc = self.encoder()?;
        writeln!(
            enc,
            "{}\t{}\t{}\t{}",
            l.recv_ns, l.seq_first, l.seq_last, raw
        )?;
        self.stats.lines += 1;
        Ok(())
    }

    /// Accept one line: gap bookkeeping, then append to the open frame.
    pub fn accept(&mut self, l: &Line) -> Result<()> {
        if l.has_seq() {
            if let Some(last) = self.last_seq {
                // Duplicate or stale envelope (replay after reconnect).
                if l.seq_max <= last {
                    self.stats.dup_skipped += 1;
                    return Ok(());
                }
            }
            if let Some(g) = detect_gap(self.last_seq, l.seq_first) {
                warn!(from = g.from, to = g.to, "gap in feed");
                self.pending_gaps
                    .push(format!("{}\t{}\t{}", g.from, g.to, l.recv_ns));
                self.stats.gaps += 1;
            }
            for g in &l.intra_gaps {
                if self.last_seq.is_none_or(|s| g.to > s) {
                    warn!(from = g.from, to = g.to, "gap inside envelope");
                    self.pending_gaps
                        .push(format!("{}\t{}\t{}", g.from, g.to, l.recv_ns));
                    self.stats.intra_gaps += 1;
                }
            }
            if l.intra_disorder > 0 {
                warn!(
                    count = l.intra_disorder,
                    seq_first = l.seq_first,
                    "sequence disorder inside envelope"
                );
                self.stats.intra_disorder += u64::from(l.intra_disorder);
            }
        } else {
            self.stats.unsequenced += 1;
        }
        self.write_line(l)?;
        if l.has_seq() {
            self.last_seq = Some(self.last_seq.map_or(l.seq_max, |s| s.max(l.seq_max)));
        }
        Ok(())
    }

    /// True when the open frame is older than `frame_max`.
    pub fn frame_due(&self) -> bool {
        self.frame_opened
            .is_some_and(|t| t.elapsed() >= self.frame_max)
    }

    /// Close the open frame, fsync, then publish gaps and last_seq.
    pub fn commit(&mut self) -> Result<()> {
        if let Some(cur) = self.cur.as_mut() {
            if let Slot::Frame(_) = cur.slot {
                let Slot::Frame(enc) = std::mem::replace(&mut cur.slot, Slot::Empty) else {
                    unreachable!()
                };
                let bw = enc.finish()?;
                let file = bw.into_inner().map_err(|e| e.into_error())?;
                file.sync_data()?;
                cur.slot = Slot::Idle(file);
                self.frame_opened = None;
                self.stats.frames += 1;
            }
        }
        if !self.pending_gaps.is_empty() {
            let path = self.root.join(GAPS_FILE);
            let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
            for g in self.pending_gaps.drain(..) {
                writeln!(f, "{g}")?;
            }
            f.sync_data()?;
        }
        if self.last_seq != self.durable_seq {
            if let Some(s) = self.last_seq {
                write_atomic(&self.root.join(STATE_FILE), &s.to_string())?;
                self.durable_seq = Some(s);
                info!(
                    last_seq = s,
                    frames = self.stats.frames,
                    lines = self.stats.lines,
                    "frame committed"
                );
            }
        }
        Ok(())
    }
}

/// Writer thread body: owns the files so the network task never blocks on
/// disk or compression. Returns once the channel is closed and drained.
pub fn run(rx: Receiver<Line>, mut w: FeedWriter) -> Result<WriterStats> {
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(l) => w.accept(&l)?,
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if w.frame_due() {
            w.commit()?;
        }
    }
    w.commit()?;
    info!(last_seq = ?w.durable_seq(), stats = ?w.stats, "writer stopped cleanly");
    Ok(w.stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hood_core::Gap;
    use std::io::Read;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "recorder-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn frame(text: &str) -> Vec<u8> {
        let mut enc = zstd::Encoder::new(Vec::new(), 3).unwrap();
        enc.include_checksum(true).unwrap();
        enc.write_all(text.as_bytes()).unwrap();
        enc.finish().unwrap()
    }

    fn decode_all_frames(path: &Path) -> String {
        let mut s = String::new();
        zstd::stream::read::Decoder::new(File::open(path).unwrap())
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    fn line(recv_ns: u128, seq: u64) -> Line {
        Line {
            recv_ns,
            seq_first: seq,
            seq_last: seq,
            seq_max: seq,
            intra_gaps: vec![],
            intra_disorder: 0,
            raw: format!(r#"{{"version":1,"messages":[{{"sequenceNumber":{seq}}}]}}"#),
        }
    }

    #[test]
    fn valid_prefix_of_clean_and_torn_data() {
        let a = frame("1\t10\t10\t{}\n");
        let b = frame("2\t11\t11\t{}\n");
        let mut data = [a.clone(), b.clone()].concat();
        assert_eq!(valid_prefix_len(&data), data.len());
        // Torn third frame: every possible cut must fall back to a + b.
        let c = frame(&"3\t12\t12\t{\"x\":\"yyyyyyyyyyyyyyyyyyyyyyyy\"}\n".repeat(50));
        for cut in 1..c.len() {
            let mut d = data.clone();
            d.extend_from_slice(&c[..cut]);
            assert_eq!(valid_prefix_len(&d), a.len() + b.len(), "cut at {cut}");
        }
        // Garbage after valid frames.
        data.extend_from_slice(b"\x00\x01garbage");
        assert_eq!(valid_prefix_len(&data), a.len() + b.len());
        assert_eq!(valid_prefix_len(&[]), 0);
    }

    #[test]
    fn repair_truncates_and_keeps_tail() {
        let dir = tmpdir("repair");
        let p = dir.join("feed-20260930-12.tsv.zst");
        let good = [frame("1\t10\t10\t{}\n"), frame("2\t11\t11\t{}\n")].concat();
        let torn_frame = frame(&"3\t12\t12\t{}\n".repeat(100));
        let tail = &torn_frame[..torn_frame.len() / 2];
        fs::write(&p, [good.as_slice(), tail].concat()).unwrap();

        let r = repair_torn(&p, &dir.join(TORN_DIR), "T").unwrap().unwrap();
        assert_eq!(r.kept_bytes as usize, good.len());
        assert_eq!(r.torn_bytes as usize, tail.len());
        assert_eq!(fs::read(&p).unwrap(), good);
        assert_eq!(fs::read(&r.saved_to).unwrap(), tail);
        assert_eq!(decode_all_frames(&p), "1\t10\t10\t{}\n2\t11\t11\t{}\n");
        // Second run: nothing to do.
        assert!(repair_torn(&p, &dir.join(TORN_DIR), "T").unwrap().is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recover_uses_data_not_stale_state() {
        let dir = tmpdir("recover");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        let p = day.join("feed-20260930-12.tsv.zst");
        let torn = frame("3\t0\t0\t{}\n3\t99\t99\t{}\n");
        let content = [
            frame("1\t10\t10\t{}\n2\t0\t0\t{\"c\":1}\n"),
            frame("2\t11\t12\t{}\n"),
            torn[..torn.len() - 3].to_vec(),
        ]
        .concat();
        fs::write(&p, content).unwrap();
        fs::write(dir.join(STATE_FILE), "10").unwrap(); // lags behind data

        let rec = recover(&dir).unwrap();
        assert_eq!(rec.repairs.len(), 1);
        assert_eq!(rec.state_seq, Some(10));
        assert_eq!(rec.data_seq, Some(12));
        assert_eq!(rec.resume_seq, Some(12));
        assert_eq!(read_state(&dir), Some(12));
        assert_eq!(list_feed_files(&dir), vec![p.clone()]); // _torn not listed
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recover_single_torn_frame_falls_back_to_state_and_older_file() {
        // Legacy layout: one frame per hour. A torn file is cut to zero bytes;
        // the resume point then comes from the previous hour's file.
        let dir = tmpdir("recover0");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("feed-20260930-11.tsv.zst"),
            frame("1\t50\t50\t{}\n"),
        )
        .unwrap();
        let torn = frame(&"2\t51\t51\t{}\n".repeat(20));
        let p = day.join("feed-20260930-12.tsv.zst");
        fs::write(&p, &torn[..torn.len() - 1]).unwrap();
        fs::write(dir.join(STATE_FILE), "51").unwrap();
        let rec = recover(&dir).unwrap();
        assert_eq!(rec.repairs.len(), 1);
        assert_eq!(fs::metadata(&p).unwrap().len(), 0);
        assert_eq!(max_seq_in_file(&p).unwrap(), None);
        assert_eq!(rec.data_seq, Some(50));
        assert_eq!(rec.resume_seq, Some(50));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn writer_commits_frames_gaps_and_state_in_order() {
        let dir = tmpdir("writer");
        // 2026-09-30T12:00:00Z
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), Some(99));
        w.accept(&line(t0, 100)).unwrap();
        // Nothing durable before commit.
        assert_eq!(read_state(&dir), None);
        assert!(!dir.join(GAPS_FILE).exists());
        w.accept(&line(t0 + 1, 105)).unwrap(); // gap 101..104
        w.accept(&line(t0 + 2, 103)).unwrap(); // stale, skipped
        let mut env = line(t0 + 3, 106);
        env.seq_last = 110;
        env.seq_max = 110;
        env.intra_gaps = vec![Gap { from: 107, to: 109 }];
        w.accept(&env).unwrap();
        let conf = Line {
            seq_first: 0,
            seq_last: 0,
            seq_max: 0,
            raw: "{\"conf\":1}".into(),
            ..line(t0 + 4, 0)
        };
        w.accept(&conf).unwrap();
        assert!(
            !dir.join(GAPS_FILE).exists(),
            "gaps must wait for fsync of data"
        );
        w.commit().unwrap();
        assert_eq!(read_state(&dir), Some(110));
        assert_eq!(
            fs::read_to_string(dir.join(GAPS_FILE)).unwrap(),
            format!("101\t104\t{}\n107\t109\t{}\n", t0 + 1, t0 + 3)
        );
        // Second frame in the same file, then hour rotation.
        w.accept(&line(t0 + 5, 111)).unwrap();
        w.accept(&line(t0 + 3_600_000_000_000, 112)).unwrap();
        w.commit().unwrap();
        assert_eq!(w.stats.dup_skipped, 1);
        assert_eq!(w.stats.unsequenced, 1);
        assert_eq!(w.stats.frames, 3);

        let files = list_feed_files(&dir);
        assert_eq!(files.len(), 2);
        let h12 = decode_all_frames(&files[0]);
        let rows: Vec<&str> = h12.lines().collect();
        assert_eq!(rows.len(), 5);
        assert!(rows[3].ends_with("\t0\t0\t{\"conf\":1}"));
        assert_eq!(
            valid_prefix_len(&fs::read(&files[0]).unwrap()),
            fs::metadata(&files[0]).unwrap().len() as usize
        );
        assert_eq!(max_seq_in_file(&files[1]).unwrap(), Some(112));
        fs::remove_dir_all(&dir).ok();
    }
}
