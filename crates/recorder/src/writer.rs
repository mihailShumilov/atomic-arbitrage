//! Durable hourly writer (task 002, items 1, 2, 6). Crash recovery is in
//! `recovery.rs`, file names in `layout.rs`.
//!
//! Durability model:
//! - The hourly file `<out>/YYYY/MM/DD/feed-YYYYMMDD-HH.tsv.zst` is a sequence
//!   of independent zstd frames (with content checksums). A frame is closed at
//!   least every `frame_max` (60 s by default), on hour rotation and on
//!   shutdown. Frames always end on a line boundary.
//! - Commit order: finish frame -> fsync data file -> append pending gap rows
//!   to gaps.tsv + fsync -> write last_seq.txt atomically (tmp, fsync, rename,
//!   fsync dir). So `last_seq.txt` and `gaps.tsv` never get ahead of data that
//!   is actually on disk. This holds on hour rotation too: a line that opens
//!   a new hour is accounted for (`last_seq`, its gap rows) only after the
//!   rotation commit of the previous hour (task 025 item 2; before 025 its
//!   gap row went out with that commit, ahead of the line).
//! - After `kill -9` at most the open frame (<= frame_max of data) is lost.
//!
//! Every fsync error, the directory's included, ends the writer (`shutdown
//! writer_error`, exit 2); since task 021 the error text names the file and
//! the step.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use hood_core::fsutil::{append_synced, fsync_dir, write_atomic};
use hood_core::ranges::{to_lines, GapRow};
use tracing::{info, warn};

use crate::layout::{hour_key, hour_path, GAPS_FILE, STATE_FILE};
use crate::route::Line;
use crate::seqtrack::{Seen, SeqTracker};

/// Longest the writer thread blocks on the channel between deadline checks.
pub const POLL_STEP: Duration = Duration::from_millis(500);
/// Headroom reserved for finish + fsync, so that a frame is on disk no later
/// than `frame_max` after its first line.
pub const COMMIT_GUARD: Duration = Duration::from_millis(200);

enum Slot {
    /// File open, no frame in progress.
    Idle(File),
    /// A zstd frame is being written.
    Frame(zstd::Encoder<'static, BufWriter<File>>),
}

struct HourFile {
    key: String,
    path: PathBuf,
    /// None only after a failed transition (starting or finishing a frame
    /// consumed the file and returned an error); the writer is stopping
    /// then, and any further write reports an inconsistent state.
    slot: Option<Slot>,
}

/// Counters of one writer run (logged at the end).
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

/// Owns the hourly files, the zstd encoder and the gap bookkeeping.
pub struct FeedWriter {
    root: PathBuf,
    level: i32,
    frame_max: Duration,
    cur: Option<HourFile>,
    frame_opened: Option<Instant>,
    /// Highest seq accepted (may not be durable yet).
    seq: SeqTracker,
    /// Highest seq known to be fsynced and recorded in last_seq.txt.
    durable_seq: Option<u64>,
    pending_gaps: Vec<GapRow>,
    pub stats: WriterStats,
}

impl FeedWriter {
    /// Writer for `root`, resuming gap detection after `resume_seq`.
    pub fn new(root: PathBuf, level: i32, frame_max: Duration, resume_seq: Option<u64>) -> Self {
        Self {
            root,
            level,
            frame_max,
            cur: None,
            frame_opened: None,
            seq: SeqTracker::new(resume_seq),
            durable_seq: resume_seq,
            pending_gaps: Vec::new(),
            stats: WriterStats::default(),
        }
    }

    /// Highest seq that is on disk and in last_seq.txt.
    pub fn durable_seq(&self) -> Option<u64> {
        self.durable_seq
    }

    fn ensure_hour(&mut self, t: DateTime<Utc>) -> Result<()> {
        let key = hour_key(t);
        if self.cur.as_ref().is_some_and(|c| c.key == key) {
            return Ok(());
        }
        // Hour changed: make everything so far durable before switching.
        self.commit()?;
        self.cur = None;
        let path = hour_path(&self.root, t);
        let dir = path.parent().expect("hour_path always has a YYYY/MM/DD parent");
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        // Append: after a restart within the same hour we add new frames.
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        fsync_dir(dir)?;
        info!(file = %path.display(), "writing");
        self.cur = Some(HourFile { key, path, slot: Some(Slot::Idle(file)) });
        Ok(())
    }

    fn encoder(&mut self) -> Result<&mut zstd::Encoder<'static, BufWriter<File>>> {
        let cur = self.cur.as_mut().expect("accept opens the hour file (ensure_hour) before any write");
        match cur.slot.take() {
            Some(Slot::Idle(file)) => {
                let mut enc = zstd::Encoder::new(BufWriter::with_capacity(1 << 16, file), self.level)
                    .with_context(|| format!("start zstd frame in {}", cur.path.display()))?;
                enc.include_checksum(true).with_context(|| format!("zstd checksum for {}", cur.path.display()))?;
                cur.slot = Some(Slot::Frame(enc));
                self.frame_opened = Some(Instant::now());
            }
            other => cur.slot = other,
        }
        match &mut cur.slot {
            Some(Slot::Frame(enc)) => Ok(enc),
            _ => anyhow::bail!("writer in inconsistent state ({})", cur.path.display()),
        }
    }

    /// Append `l` to the open frame of the current hour (the caller has
    /// called `ensure_hour` for it).
    fn write_line(&mut self, l: &Line) -> Result<()> {
        // `raw` is always valid JSON here (route.rs wraps anything else in a
        // base64 `recorderFrame`). In valid JSON a raw CR/LF/TAB can only be
        // insignificant whitespace between tokens (control characters are
        // not allowed inside strings, RFC 8259 section 7), so removing it
        // keeps the value intact. TAB since task 009 (remark Р2 of the 008
        // audit): otherwise a line could get more than 4 TSV fields.
        let raw = l.raw.replace(['\n', '\r', '\t'], "");
        let enc = self.encoder()?;
        let res = writeln!(enc, "{}\t{}\t{}\t{}", l.recv_ns, l.seq_first, l.seq_last, raw);
        if let Err(e) = res {
            let path = self.cur.as_ref().map(|c| c.path.display().to_string()).unwrap_or_default();
            return Err(e).with_context(|| format!("write line to {path}"));
        }
        self.stats.lines += 1;
        Ok(())
    }

    /// Accept one line: gap bookkeeping (the shared [`SeqTracker`] rule),
    /// then append to the open frame.
    pub fn accept(&mut self, l: &Line) -> Result<()> {
        let fresh = if l.has_seq() {
            match self.seq.check(l.seq_first, l.seq_max, &l.intra_gaps) {
                // Duplicate or stale envelope (replay after reconnect): never
                // written, so it does not open or rotate an hour file either.
                Seen::Stale => {
                    self.stats.dup_skipped += 1;
                    return Ok(());
                }
                Seen::Fresh { seam, intra } => Some((seam, intra)),
            }
        } else {
            None
        };
        // Open or rotate to the line's hour *before* queueing its gap rows
        // and advancing last_seq: the rotation commits the previous hour, and
        // that commit must publish neither a gap row nor a last_seq of a line
        // that is not in a frame yet (task 025 item 2, remark Р4 of the 021
        // review; last_seq since task 021). The rows go out with the commit
        // that makes this line durable.
        self.ensure_hour(DateTime::<Utc>::from_timestamp_nanos(l.recv_ns as i64))?;
        match fresh {
            Some((seam, intra)) => {
                if let Some(g) = seam {
                    warn!(from = g.from, to = g.to, "gap in feed");
                    self.pending_gaps.push(GapRow { range: g, recv_ns: l.recv_ns });
                    self.stats.gaps += 1;
                }
                for g in intra {
                    warn!(from = g.from, to = g.to, "gap inside envelope");
                    self.pending_gaps.push(GapRow { range: g, recv_ns: l.recv_ns });
                    self.stats.intra_gaps += 1;
                }
                if l.intra_disorder > 0 {
                    warn!(count = l.intra_disorder, seq_first = l.seq_first, "sequence disorder inside envelope");
                    self.stats.intra_disorder += u64::from(l.intra_disorder);
                }
            }
            None => self.stats.unsequenced += 1,
        }
        self.write_line(l)?;
        if l.has_seq() {
            self.seq.advance(l.seq_max);
        }
        Ok(())
    }

    /// Time left until the open frame must be committed, measured at `now`
    /// (None if no frame is open). The deadline is `frame_max - COMMIT_GUARD`
    /// after the first line of the frame, so that finish + fsync also fit
    /// into `frame_max` (remark З4 of the 002 audit).
    pub fn frame_time_left(&self, now: Instant) -> Option<Duration> {
        let opened = self.frame_opened?;
        let budget = self.frame_max.saturating_sub(COMMIT_GUARD);
        Some(budget.saturating_sub(now.saturating_duration_since(opened)))
    }

    /// True when the open frame has reached its deadline.
    pub fn frame_due(&self) -> bool {
        self.frame_time_left(Instant::now()).is_some_and(|d| d.is_zero())
    }

    /// Close the open frame, fsync, then publish gaps and last_seq.
    pub fn commit(&mut self) -> Result<()> {
        if let Some(cur) = self.cur.as_mut() {
            match cur.slot.take() {
                Some(Slot::Frame(enc)) => {
                    let p = cur.path.display();
                    let bw = enc.finish().with_context(|| format!("finish zstd frame of {p}"))?;
                    let file = bw.into_inner().map_err(|e| e.into_error()).with_context(|| format!("flush {p}"))?;
                    file.sync_data().with_context(|| format!("fsync {p}"))?;
                    cur.slot = Some(Slot::Idle(file));
                    self.frame_opened = None;
                    self.stats.frames += 1;
                }
                other => cur.slot = other,
            }
        }
        if !self.pending_gaps.is_empty() {
            // One write + fsync of the file and of the directory (the io
            // error names the step and the path).
            append_synced(&self.root.join(GAPS_FILE), to_lines(&self.pending_gaps).as_bytes())?;
            self.pending_gaps.clear();
        }
        let last = self.seq.last();
        if last != self.durable_seq {
            if let Some(s) = last {
                write_atomic(&self.root.join(STATE_FILE), s.to_string().as_bytes())?;
                self.durable_seq = Some(s);
                info!(last_seq = s, frames = self.stats.frames, lines = self.stats.lines, "frame committed");
            }
        }
        Ok(())
    }
}

/// Writer thread body: owns the files so the network task never blocks on
/// disk or compression. Returns once the channel is closed and drained.
pub fn run(rx: Receiver<Line>, mut w: FeedWriter) -> Result<WriterStats> {
    loop {
        // Never sleep past the frame deadline: the wait is clipped to the
        // time left, so the frame is committed on time even in silence.
        let wait = w.frame_time_left(Instant::now()).map_or(POLL_STEP, |left| left.min(POLL_STEP));
        match rx.recv_timeout(wait) {
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
    use crate::layout::{list_feed_files, read_state};
    use crate::rawline::file_seq_max;
    use crate::recovery::recover;
    use crate::recovery::tests::{decode_all_frames, gaps_rows, tmpdir};
    use crate::recovery::valid_prefix_len;
    use hood_core::Gap;

    fn line(recv_ns: u128, seq: u64) -> Line {
        Line {
            recv_ns,
            seq_first: seq,
            seq_last: seq,
            seq_max: seq,
            intra_gaps: vec![],
            intra_disorder: 0,
            kind3_ts: None,
            raw: format!(r#"{{"version":1,"messages":[{{"sequenceNumber":{seq}}}]}}"#),
        }
    }

    /// The live seam (end of data -> first seq of the new session) is still
    /// written by `accept`, and recover does not duplicate it afterwards.
    #[test]
    fn live_gap_is_written_once_by_accept_not_by_recover() {
        let dir = tmpdir("reconcile3");
        let t0: u128 = 1_790_769_600 * 1_000_000_000; // 2026-09-30T12:00Z
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), None);
        w.accept(&line(t0, 100)).unwrap();
        w.commit().unwrap();
        let rec = recover(&dir).unwrap();
        assert!(rec.reconciled.is_empty());
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), rec.resume_seq);
        w.accept(&line(t0 + 10, 150)).unwrap();
        w.commit().unwrap();
        assert_eq!(gaps_rows(&dir), vec![format!("101\t149\t{}", t0 + 10)]);
        assert!(recover(&dir).unwrap().reconciled.is_empty());
        assert_eq!(gaps_rows(&dir).len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    /// Item 5 (З4), pure boundary: the deadline is frame_max - COMMIT_GUARD
    /// after the first line of the frame.
    #[test]
    fn frame_deadline_boundary() {
        let dir = tmpdir("deadline");
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), None);
        assert_eq!(w.frame_time_left(Instant::now()), None);
        w.accept(&line(t0, 1)).unwrap();
        let opened = w.frame_opened.unwrap();
        let budget = Duration::from_secs(60) - COMMIT_GUARD;
        assert_eq!(w.frame_time_left(opened), Some(budget));
        let just_before = opened + budget - Duration::from_millis(1);
        assert_eq!(w.frame_time_left(just_before), Some(Duration::from_millis(1)));
        assert_eq!(w.frame_time_left(opened + budget), Some(Duration::ZERO));
        assert_eq!(w.frame_time_left(opened + Duration::from_secs(61)), Some(Duration::ZERO));
        // The writer thread never blocks past the deadline.
        let wait = w.frame_time_left(just_before).map_or(POLL_STEP, |l| l.min(POLL_STEP));
        assert!(wait <= Duration::from_millis(1));
        w.commit().unwrap();
        assert_eq!(w.frame_time_left(Instant::now()), None);
        fs::remove_dir_all(&dir).ok();
    }

    /// Item 5 (З4), timing: with no further input the frame is on disk
    /// (last_seq.txt written after fsync) no later than frame_max after the
    /// first line, and not much earlier than frame_max - COMMIT_GUARD.
    #[test]
    fn frame_committed_within_frame_max_in_silence() {
        let dir = tmpdir("deadline2");
        let frame_max = Duration::from_millis(1500);
        let w = FeedWriter::new(dir.clone(), 3, frame_max, None);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Line>(16);
        let h = std::thread::spawn(move || run(rx, w).unwrap());
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let sent = Instant::now();
        tx.send(line(t0, 42)).unwrap();
        while read_state(&dir).is_none() {
            assert!(sent.elapsed() < Duration::from_secs(5), "never committed");
            std::thread::sleep(Duration::from_millis(5));
        }
        let took = sent.elapsed();
        assert!(took <= frame_max, "committed after {took:?}");
        assert!(took + Duration::from_millis(50) >= frame_max - COMMIT_GUARD, "committed too early: {took:?}");
        drop(tx);
        h.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 009 item 4 (Р2): insignificant CR/LF/TAB in valid JSON are
    /// removed, so every line has exactly 4 TSV fields and the JSON value is
    /// unchanged.
    #[test]
    fn whitespace_in_valid_json_is_removed_value_kept() {
        let dir = tmpdir("tabs");
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let raw = "{\"version\":1,\t\"messages\":[\r\n\t{\"sequenceNumber\":5,\"s\":\"a b\"}\n]}";
        let before: serde_json::Value = serde_json::from_str(raw).unwrap();
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), None);
        w.accept(&Line { raw: raw.into(), ..line(t0, 5) }).unwrap();
        w.commit().unwrap();
        let text = decode_all_frames(&list_feed_files(&dir)[0]);
        let row = text.lines().next().unwrap();
        let cols: Vec<&str> = row.split('\t').collect();
        assert_eq!(cols.len(), 4, "{row:?}");
        assert_eq!(cols[3], "{\"version\":1,\"messages\":[{\"sequenceNumber\":5,\"s\":\"a b\"}]}");
        let after: serde_json::Value = serde_json::from_str(cols[3]).unwrap();
        assert_eq!(before, after);
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
        let conf = Line { seq_first: 0, seq_last: 0, seq_max: 0, raw: "{\"conf\":1}".into(), ..line(t0 + 4, 0) };
        w.accept(&conf).unwrap();
        assert!(!dir.join(GAPS_FILE).exists(), "gaps must wait for fsync of data");
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
        assert_eq!(valid_prefix_len(&fs::read(&files[0]).unwrap()), fs::metadata(&files[0]).unwrap().len() as usize);
        assert_eq!(file_seq_max(&files[1]).unwrap(), Some(112));
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 021: last_seq.txt never names a line that is not on disk, also
    /// when the line itself triggers the hour rotation (whose commit
    /// publishes last_seq). Regression found while moving the writer to
    /// SeqTracker: advancing before the write made the rotation commit 112.
    #[test]
    fn hour_rotation_commits_only_written_lines() {
        let dir = tmpdir("rotate");
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), Some(110));
        w.accept(&line(t0, 111)).unwrap();
        assert_eq!(read_state(&dir), None);
        // Next hour: ensure_hour commits hour 12 before 112 is written.
        w.accept(&line(t0 + 3_600_000_000_000, 112)).unwrap();
        assert_eq!(read_state(&dir), Some(111));
        w.commit().unwrap();
        assert_eq!(read_state(&dir), Some(112));
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 025 item 2: the gap row of a line that opens a new hour is not
    /// published by the rotation commit of the previous hour (before 025 it
    /// was, while the line itself was still in the open frame), but by the
    /// commit that makes the line durable. A stale line does not open an
    /// hour file.
    #[test]
    fn hour_rotation_publishes_gap_row_after_its_line() {
        let dir = tmpdir("rotategap");
        let t0: u128 = 1_790_769_600 * 1_000_000_000; // 2026-09-30T12:00Z
        let t1 = t0 + 3_600_000_000_000;
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), Some(110));
        w.accept(&line(t0, 111)).unwrap();
        w.accept(&line(t0 + 1, 113)).unwrap(); // hole 112, hour 12

        // Hole 114..=119; the first line of hour 13 triggers the rotation.
        w.accept(&line(t1, 120)).unwrap();
        // The rotation published hour 12 only.
        assert_eq!(gaps_rows(&dir), vec![format!("112\t112\t{}", t0 + 1)]);
        assert_eq!(read_state(&dir), Some(113));
        let files = list_feed_files(&dir);
        assert_eq!(files.len(), 2);
        assert_eq!(fs::metadata(&files[1]).unwrap().len(), 0, "hour 13 has no complete frame yet");
        w.commit().unwrap();
        assert_eq!(gaps_rows(&dir), vec![format!("112\t112\t{}", t0 + 1), format!("114\t119\t{t1}")]);
        assert_eq!(read_state(&dir), Some(120));
        assert_eq!(file_seq_max(&files[1]).unwrap(), Some(120));
        w.accept(&line(t1 + 3_600_000_000_000, 100)).unwrap(); // stale, hour 14
        w.commit().unwrap();
        assert_eq!(list_feed_files(&dir).len(), 2);
        assert_eq!(w.stats.dup_skipped, 1);
        fs::remove_dir_all(&dir).ok();
    }

    /// Lines in complete frames on disk, as (recv_ns, seq_max).
    fn durable_lines(dir: &std::path::Path) -> Vec<(u128, u64)> {
        let mut out = Vec::new();
        for f in list_feed_files(dir) {
            let data = fs::read(&f).unwrap();
            let complete = &data[..valid_prefix_len(&data)];
            if complete.is_empty() {
                continue;
            }
            let text = zstd::stream::decode_all(complete).unwrap();
            for l in text.split(|&b| b == b'\n').filter_map(crate::rawline::parse_raw_line) {
                if let Some(s) = crate::rawline::line_seqs(&l) {
                    out.push((l.recv_ns, s.seq_max));
                }
            }
        }
        out
    }

    /// Task 025 item 2, invariant over many rotations (the state a `kill -9`
    /// would leave after any accept): every gaps.tsv row belongs to a line
    /// in a complete frame, and last_seq.txt is not ahead of the data.
    #[test]
    fn gaps_and_last_seq_never_ahead_of_data_across_rotations() {
        let dir = tmpdir("rotateinv");
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let hour: u128 = 3_600_000_000_000;
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), Some(9));
        // (hour offset, seq): holes inside hours and on every boundary,
        // a stale line as the first line of a new hour (it must not rotate).
        let plan = [(0, 10), (0, 12), (1, 15), (1, 16), (2, 14), (2, 20), (3, 21), (3, 25), (4, 30)];
        for (i, &(h, seq)) in plan.iter().enumerate() {
            let recv = t0 + h * hour + i as u128;
            w.accept(&line(recv, seq)).unwrap();
            let on_disk = durable_lines(&dir);
            for row in gaps_rows(&dir) {
                let recv_ns: u128 = row.split('\t').nth(2).unwrap().parse().unwrap();
                assert!(on_disk.iter().any(|&(r, _)| r == recv_ns), "after #{i}: row {row:?} ahead of data");
            }
            if let Some(s) = read_state(&dir) {
                assert!(on_disk.iter().any(|&(_, m)| m >= s), "after #{i}: last_seq {s} ahead of data");
            }
        }
        w.commit().unwrap();
        let rows: Vec<String> = gaps_rows(&dir).iter().map(|r| r.rsplit_once('\t').unwrap().0.to_owned()).collect();
        assert_eq!(rows, vec!["11\t11", "13\t14", "17\t19", "22\t24", "26\t29"]);
        assert_eq!(read_state(&dir), Some(30));
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 021 item 5: a writer error names the file (here: the hour
    /// directory cannot be created because a plain file is in the way).
    #[test]
    fn writer_error_names_the_path() {
        let dir = tmpdir("werr");
        fs::write(dir.join("2026"), b"not a directory").unwrap();
        let t0: u128 = 1_790_769_600 * 1_000_000_000;
        let mut w = FeedWriter::new(dir.clone(), 3, Duration::from_secs(60), None);
        let e = format!("{:#}", w.accept(&line(t0, 1)).unwrap_err());
        assert!(e.starts_with("create ") && e.contains("2026/09/30"), "{e}");
        fs::remove_dir_all(&dir).ok();
    }
}
