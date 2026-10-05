//! Crash recovery at start-up (task 002 items 1, 2, 6; task 021 items 1, 5).
//!
//! The writer's commit order (frame -> fsync data -> gaps.tsv -> last_seq.txt,
//! see `writer.rs`) means that after `kill -9` at most the open frame is lost.
//! [`recover`] truncates the torn tail of the newest files back to the last
//! complete frame, keeps the cut bytes in `<out>/_torn/`, appends holes in the
//! data that gaps.tsv does not know about, and derives the resume point from
//! the data itself.

use std::borrow::Cow;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use hood_core::fsutil::{append_synced, fsync_dir, write_atomic};
use hood_core::ranges::{parse_ranges_file_lenient, subtract, to_lines, GapRow, LineFault, Range};
use tracing::warn;

use crate::layout::{list_feed_files, read_state, GAPS_FILE, STATE_FILE, TORN_DIR};
use crate::rawline::{file_seq_max, for_each_raw_line, line_seqs};
use crate::seqtrack::{Seen, SeqTracker};

// fs helpers (atomic write, fsync of a directory, append + fsync) are in
// `hood_core::fsutil` since task 019. Every fsync error, the directory's
// included, is returned: start-up then exits 1 instead of letting
// last_seq.txt / gaps.tsv claim data that may not be on disk.

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

/// A torn tail cut off one hourly file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TornRepair {
    /// The repaired hourly file.
    pub file: PathBuf,
    /// Bytes kept (complete frames).
    pub kept_bytes: u64,
    /// Bytes cut off.
    pub torn_bytes: u64,
    /// Where the cut bytes were saved.
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
    fs::create_dir_all(torn_dir).with_context(|| format!("create {}", torn_dir.display()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("feed");
    let saved_to = torn_dir.join(format!("{name}.at{keep}.{stamp}.torn"));
    {
        let mut f = File::create(&saved_to).with_context(|| format!("create {}", saved_to.display()))?;
        f.write_all(&data[keep..]).with_context(|| format!("write {}", saved_to.display()))?;
        f.sync_all().with_context(|| format!("fsync {}", saved_to.display()))?;
    }
    fsync_dir(torn_dir)?;
    let f =
        OpenOptions::new().write(true).open(path).with_context(|| format!("open {} for truncation", path.display()))?;
    f.set_len(keep as u64).with_context(|| format!("truncate {} to {keep} bytes", path.display()))?;
    f.sync_all().with_context(|| format!("fsync {}", path.display()))?;
    Ok(Some(TornRepair {
        file: path.to_path_buf(),
        kept_bytes: keep as u64,
        torn_bytes: (data.len() - keep) as u64,
        saved_to,
    }))
}

/// Why a line of gaps.tsv was not used at start-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GapsSkip {
    /// The last line has no `\n` (torn write, or being appended).
    Unterminated,
    /// A terminated line that does not parse; the parser's reason.
    Broken(LineFault),
}

/// A line of gaps.tsv that [`read_gap_ranges`] skipped (also logged to
/// connections.tsv as `gaps_line_skipped`, task 021).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedGapLine {
    /// 1-based line number.
    pub line_no: usize,
    /// The line as it is in the file.
    pub line: String,
    /// Why it was skipped.
    pub why: GapsSkip,
}

/// gaps.tsv as read at start-up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GapsRead {
    /// Ranges of all lines that parsed.
    pub ranges: Vec<Range>,
    /// Lines that were skipped (each also logged as a WARN).
    pub skipped: Vec<SkippedGapLine>,
}

/// Ranges already listed in `gaps.tsv`, read with the shared policy of
/// `hood_core::ranges`: an unterminated last line is ignored with a WARN.
///
/// One deliberate difference from the enricher: a broken line that ends
/// with `\n` is skipped with a WARN here instead of failing. This runs at
/// start-up of the live recorder; failing would crash-loop it under systemd
/// over its own state file, i.e. lose feed data, while a skipped line costs
/// at most a duplicate gap row (the enricher merges overlapping ranges). The
/// enricher (`--gaps`) still rejects such a file, so the line gets noticed.
///
/// Bytes that are not valid UTF-8 (task 025 item 4, decided by the same
/// argument; before 025 the whole file was an error and start-up exited 1,
/// i.e. a crash loop under systemd): the file is decoded lossily with one
/// WARN. Each invalid sequence becomes U+FFFD, so a line with such bytes in
/// `from` or `to` is a broken line (WARN + `gaps_line_skipped broken`); bytes
/// in the third column (`recv_ns`) are not looked at, the range still counts.
/// Any other read error (permissions, IO) is an error: exit 1.
pub fn read_gap_ranges(out: &Path) -> Result<GapsRead> {
    let path = out.join(GAPS_FILE);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(GapsRead::default()),
        Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    if matches!(text, Cow::Owned(_)) {
        warn!(file = %path.display(), "gaps.tsv is not valid UTF-8; invalid bytes read as U+FFFD, lines they break are skipped");
    }
    let p = parse_ranges_file_lenient(&text);
    let mut skipped = Vec::new();
    for e in &p.broken {
        warn!(
            file = %path.display(), line_no = e.line_no, line = %e.line.escape_debug(), reason = %e.reason,
            "skipping broken line of gaps.tsv"
        );
        skipped.push(SkippedGapLine { line_no: e.line_no, line: e.line.clone(), why: GapsSkip::Broken(e.reason) });
    }
    if let Some(line) = p.unterminated {
        warn!(file = %path.display(), line = %line.escape_debug(), "ignoring unterminated last line of gaps.tsv");
        skipped.push(SkippedGapLine {
            line_no: text.lines().count(),
            line: line.to_owned(),
            why: GapsSkip::Unterminated,
        });
    }
    Ok(GapsRead { ranges: p.ranges, skipped })
}

/// Replays the writer's gap bookkeeping (the shared [`SeqTracker`] rule, as
/// in `FeedWriter::accept`) over the lines of one hourly file, continuing
/// from `seq` (highest seq seen before this file). Holes found are appended
/// to `holes`. Returns the highest seq in the file. An empty file is fine
/// (cut back to zero after a crash).
pub fn scan_seq_holes(path: &Path, seq: &mut SeqTracker, holes: &mut Vec<GapRow>) -> Result<Option<u64>> {
    let mut best: Option<u64> = None;
    for_each_raw_line(path, |l| {
        let Some(s) = line_seqs(&l) else { return };
        best = Some(best.map_or(s.seq_max, |b| b.max(s.seq_max)));
        // A stale line is skipped by the writer: it is never on disk.
        if let Seen::Fresh { seam, intra } = seq.observe(l.seq_first, s.seq_max, &s.intra_gaps) {
            holes.extend(seam.into_iter().chain(intra).map(|range| GapRow { range, recv_ns: l.recv_ns }));
        }
    })?;
    Ok(best)
}

/// Remark З1 of the 002 audit: the writer fsyncs data before appending the
/// gap row, so a crash in between leaves a hole in the data that gaps.tsv
/// does not know about. Holes found in `holes` but not covered by gaps.tsv
/// are appended (fsync) and returned, with the lines of gaps.tsv that were
/// skipped while reading it. Idempotent: a second call adds nothing.
pub fn reconcile_gaps(out: &Path, holes: &[GapRow]) -> Result<(Vec<GapRow>, Vec<SkippedGapLine>)> {
    let listed = read_gap_ranges(out)?;
    let mut missing = Vec::new();
    for h in holes {
        for range in subtract(&[h.range], &listed.ranges) {
            missing.push(GapRow { range, recv_ns: h.recv_ns });
        }
    }
    if !missing.is_empty() {
        append_synced(&out.join(GAPS_FILE), to_lines(&missing).as_bytes())?;
    }
    Ok((missing, listed.skipped))
}

/// Outcome of [`recover`].
#[derive(Debug, Default)]
pub struct Recovery {
    /// Torn tails cut off.
    pub repairs: Vec<TornRepair>,
    /// Holes in the data that were missing from gaps.tsv and got appended.
    pub reconciled: Vec<GapRow>,
    /// Lines of gaps.tsv that could not be used.
    pub skipped_gap_lines: Vec<SkippedGapLine>,
    /// `last_seq.txt` as found.
    pub state_seq: Option<u64>,
    /// Highest seq in the data.
    pub data_seq: Option<u64>,
    /// Where to resume gap detection from.
    pub resume_seq: Option<u64>,
}

/// Start-up recovery: repair torn tails of the newest two hourly files,
/// reconcile holes in those files (and at the seam with the file before
/// them) with gaps.tsv, then derive the resume point. Data wins over
/// `last_seq.txt`: if the state lags (killed between fsync and state write)
/// we would otherwise report a fake gap; if the state leads (must not happen)
/// we would hide a real one.
pub fn recover(out: &Path) -> Result<Recovery> {
    let files = list_feed_files(out);
    let torn_dir = out.join(TORN_DIR);
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let mut rec = Recovery { state_seq: read_state(out), ..Default::default() };
    for f in files.iter().rev().take(2) {
        if let Some(r) = repair_torn(f, &torn_dir, &stamp)? {
            warn!(
                file = %r.file.display(), kept = r.kept_bytes, torn = r.torn_bytes,
                saved = %r.saved_to.display(), "truncated torn zstd tail"
            );
            rec.repairs.push(r);
        }
    }
    // The two newest files (already repaired) are scanned in full; the file
    // before them only provides the seq at the seam, by the same rule
    // (highest seq of each line, task 021 item 1). It is not repaired, so a
    // read error there is logged and the seam check skipped.
    let n = files.len();
    let recent = &files[n.saturating_sub(2)..];
    let mut seam: Option<u64> = None;
    if n >= 3 {
        match file_seq_max(&files[n - 3]) {
            Ok(s) => seam = s,
            Err(e) => warn!(
                file = %files[n - 3].display(), error = %format!("{e:#}"),
                "cannot read the file before the newest two, seam not checked"
            ),
        }
    }
    let mut seq = SeqTracker::new(seam);
    let mut holes = Vec::new();
    let mut recent_max: Option<u64> = None;
    for f in recent {
        if let Some(s) = scan_seq_holes(f, &mut seq, &mut holes)? {
            recent_max = Some(recent_max.map_or(s, |b| b.max(s)));
        }
    }
    rec.data_seq = recent_max.or(seam);
    (rec.reconciled, rec.skipped_gap_lines) = reconcile_gaps(out, &holes)?;
    for g in &rec.reconciled {
        warn!(
            from = g.range.from,
            to = g.range.to,
            recv_ns = g.recv_ns as u64,
            "hole in data was missing from gaps.tsv, appended"
        );
    }
    rec.resume_seq = match (rec.data_seq, rec.state_seq) {
        (Some(d), Some(s)) if d != s => {
            warn!(data = d, state = s, "last_seq.txt disagrees with data, using data");
            Some(d)
        }
        (Some(d), _) => Some(d),
        (None, s) => s,
    };
    // Make the state file match what is really on disk.
    if let Some(s) = rec.resume_seq {
        if rec.state_seq != Some(s) {
            write_atomic(&out.join(STATE_FILE), s.to_string().as_bytes())?;
        }
    }
    Ok(rec)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Read;

    pub(crate) fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("recorder-test-{tag}-{}-{}", std::process::id(), crate::now_ns()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    pub(crate) fn frame(text: &str) -> Vec<u8> {
        let mut enc = zstd::Encoder::new(Vec::new(), 3).unwrap();
        enc.include_checksum(true).unwrap();
        enc.write_all(text.as_bytes()).unwrap();
        enc.finish().unwrap()
    }

    pub(crate) fn decode_all_frames(path: &Path) -> String {
        let mut s = String::new();
        zstd::stream::read::Decoder::new(File::open(path).unwrap()).unwrap().read_to_string(&mut s).unwrap();
        s
    }

    fn env_line(recv: u128, seqs: &[u64]) -> String {
        let m: Vec<String> = seqs.iter().map(|s| format!(r#"{{"sequenceNumber":{s}}}"#)).collect();
        format!("{recv}\t{}\t{}\t{{\"version\":1,\"messages\":[{}]}}\n", seqs[0], seqs[seqs.len() - 1], m.join(","))
    }

    pub(crate) fn gaps_rows(dir: &Path) -> Vec<String> {
        fs::read_to_string(dir.join(GAPS_FILE)).unwrap_or_default().lines().map(str::to_owned).collect()
    }

    fn gap(from: u64, to: u64, recv_ns: u128) -> GapRow {
        GapRow { range: Range { from, to }, recv_ns }
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

    /// Task 021 item 5: errors name the file and the step.
    #[test]
    fn errors_carry_the_file_name() {
        let dir = tmpdir("ctx");
        let missing = dir.join("2026/09/30/feed-20260930-12.tsv.zst");
        let e = format!("{:#}", repair_torn(&missing, &dir.join(TORN_DIR), "T").unwrap_err());
        assert!(e.contains("read ") && e.contains("feed-20260930-12.tsv.zst"), "{e}");
        let e = format!("{:#}", file_seq_max(&missing).unwrap_err());
        assert!(e.contains("open ") && e.contains("feed-20260930-12.tsv.zst"), "{e}");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recover_uses_data_not_stale_state() {
        let dir = tmpdir("recover");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        let p = day.join("feed-20260930-12.tsv.zst");
        let torn = frame("3\t0\t0\t{}\n3\t99\t99\t{}\n");
        let content =
            [frame("1\t10\t10\t{}\n2\t0\t0\t{\"c\":1}\n"), frame("2\t11\t12\t{}\n"), torn[..torn.len() - 3].to_vec()]
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
        fs::write(day.join("feed-20260930-11.tsv.zst"), frame("1\t50\t50\t{}\n")).unwrap();
        let torn = frame(&"2\t51\t51\t{}\n".repeat(20));
        let p = day.join("feed-20260930-12.tsv.zst");
        fs::write(&p, &torn[..torn.len() - 1]).unwrap();
        fs::write(dir.join(STATE_FILE), "51").unwrap();
        let rec = recover(&dir).unwrap();
        assert_eq!(rec.repairs.len(), 1);
        assert_eq!(fs::metadata(&p).unwrap().len(), 0);
        assert_eq!(file_seq_max(&p).unwrap(), None);
        assert_eq!(rec.data_seq, Some(50));
        assert_eq!(rec.resume_seq, Some(50));
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 019: one reading policy (hood_core::ranges) with the recorder's
    /// documented exception: a broken terminated line is skipped, not fatal.
    /// Task 021: the skipped lines are returned for connections.tsv.
    #[test]
    fn gap_ranges_reading_policy() {
        let dir = tmpdir("gapsread");
        assert_eq!(read_gap_ranges(&dir).unwrap(), GapsRead::default());
        fs::write(dir.join(GAPS_FILE), "51\t99\t2\nbroken\n# c\n\n102\t104\t4\n200\t2").unwrap();
        let r = read_gap_ranges(&dir).unwrap();
        assert_eq!(r.ranges, vec![Range { from: 51, to: 99 }, Range { from: 102, to: 104 }]);
        assert_eq!(r.skipped.len(), 2);
        assert_eq!((r.skipped[0].line_no, r.skipped[0].line.as_str()), (2, "broken"));
        assert!(matches!(r.skipped[0].why, GapsSkip::Broken(_)));
        assert_eq!(r.skipped[1], SkippedGapLine { line_no: 6, line: "200\t2".into(), why: GapsSkip::Unterminated });
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 025 item 4: gaps.tsv with bytes that are not UTF-8 is read
    /// lossily instead of failing start-up. A line broken by them is skipped
    /// (with its typed reason); invalid bytes in `recv_ns` keep the range.
    #[test]
    fn non_utf8_gaps_file_is_read_lossily() {
        let dir = tmpdir("gapsutf8");
        fs::write(dir.join(GAPS_FILE), b"51\t99\t2\n1\xff2\t130\t5\n104\t106\t9\xfe\n").unwrap();
        let r = read_gap_ranges(&dir).unwrap();
        assert_eq!(r.ranges, vec![Range { from: 51, to: 99 }, Range { from: 104, to: 106 }]);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(r.skipped[0].line_no, 2);
        assert_eq!(r.skipped[0].line, "1\u{FFFD}2\t130\t5");
        assert_eq!(r.skipped[0].why, GapsSkip::Broken(LineFault::NotANumber("from")));

        // Whole start-up: no error, the hole 104..=106 counts as listed (not
        // appended again), the file is not rewritten.
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("feed-20260930-12.tsv.zst"), frame(&[env_line(1, &[103]), env_line(2, &[107])].concat()))
            .unwrap();
        let before = fs::read(dir.join(GAPS_FILE)).unwrap();
        let rec = recover(&dir).unwrap();
        assert!(rec.reconciled.is_empty());
        assert_eq!(rec.skipped_gap_lines.len(), 1);
        assert_eq!(rec.resume_seq, Some(107));
        assert_eq!(fs::read(dir.join(GAPS_FILE)).unwrap(), before);
        fs::remove_dir_all(&dir).ok();
    }

    /// Acceptance test for item 1 (З1): a hole in the data without a
    /// gaps.tsv row (crash between data fsync and the gaps.tsv append) is
    /// added exactly once; a second start adds nothing.
    #[test]
    fn recover_appends_missing_gap_row_once() {
        let dir = tmpdir("reconcile");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        // Hour 11: 100..=102. Hour 12: 103, hole 104..=106, 107, 108, ping.
        fs::write(
            day.join("feed-20260930-11.tsv.zst"),
            frame(&[env_line(1, &[100]), env_line(2, &[101]), env_line(3, &[102])].concat()),
        )
        .unwrap();
        fs::write(
            day.join("feed-20260930-12.tsv.zst"),
            [
                frame(&[env_line(4, &[103]), "5\t0\t0\t{\"recorderFrame\":{}}\n".into()].concat()),
                frame(&[env_line(6, &[107]), env_line(7, &[108])].concat()),
            ]
            .concat(),
        )
        .unwrap();
        fs::write(dir.join(STATE_FILE), "108").unwrap();

        let rec = recover(&dir).unwrap();
        assert_eq!(rec.reconciled, vec![gap(104, 106, 6)]);
        assert_eq!(gaps_rows(&dir), vec!["104\t106\t6"]);
        assert_eq!(rec.resume_seq, Some(108));

        let rec2 = recover(&dir).unwrap();
        assert!(rec2.reconciled.is_empty());
        assert_eq!(gaps_rows(&dir), vec!["104\t106\t6"]);
        fs::remove_dir_all(&dir).ok();
    }

    /// Holes already in gaps.tsv are left alone; the seam with the file
    /// before the newest two and holes inside multi-message envelopes are
    /// checked; a partially listed hole gets only its missing part.
    #[test]
    fn recover_reconciles_seam_intra_and_partial_holes() {
        let dir = tmpdir("reconcile2");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("feed-20260930-10.tsv.zst"), frame(&env_line(1, &[50]))).unwrap();
        // Seam hole 51..=99 (listed), then 100, 101.
        fs::write(day.join("feed-20260930-11.tsv.zst"), frame(&[env_line(2, &[100]), env_line(3, &[101])].concat()))
            .unwrap();
        // 102..=109 missing, gaps.tsv lists only 102..=104. Then an envelope
        // 110, 112 with an intra hole 111, a stale duplicate 105, then 113.
        fs::write(
            day.join("feed-20260930-12.tsv.zst"),
            frame(&[env_line(4, &[110, 112]), env_line(5, &[105]), env_line(6, &[113])].concat()),
        )
        .unwrap();
        fs::write(dir.join(GAPS_FILE), "51\t99\t2\n102\t104\t4\n").unwrap();
        let rec = recover(&dir).unwrap();
        assert_eq!(rec.reconciled, vec![gap(105, 109, 4), gap(111, 111, 4)]);
        assert_eq!(rec.data_seq, Some(113));
        assert_eq!(gaps_rows(&dir), vec!["51\t99\t2", "102\t104\t4", "105\t109\t4", "111\t111\t4"]);
        assert!(recover(&dir).unwrap().reconciled.is_empty());

        // Seam hole missing from gaps.tsv is found too.
        fs::write(dir.join(GAPS_FILE), "").unwrap();
        let rec = recover(&dir).unwrap();
        assert_eq!(rec.reconciled.first(), Some(&gap(51, 99, 2)));
        assert_eq!(rec.reconciled.len(), 3);
        fs::remove_dir_all(&dir).ok();
    }

    /// Task 021 item 1 (remark В2): the file before the newest two ends with
    /// an internally out-of-order envelope [110, 108] (seq_last = 108,
    /// highest seq 110). The seam is 110, as the writer had it: no fake hole
    /// 109..=110 is appended, and without seqs in the newest two files the
    /// resume point is 110 (before 021: 108, so 109 and 110 would have been
    /// recorded twice after the resume request).
    #[test]
    fn out_of_order_envelope_at_the_seam() {
        let dir = tmpdir("seam-ooo");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        fs::write(
            day.join("feed-20260930-10.tsv.zst"),
            frame(&[env_line(1, &[107]), env_line(2, &[110, 108])].concat()),
        )
        .unwrap();
        fs::write(day.join("feed-20260930-11.tsv.zst"), frame(&env_line(3, &[111]))).unwrap();
        fs::write(day.join("feed-20260930-12.tsv.zst"), frame(&env_line(4, &[112]))).unwrap();
        let rec = recover(&dir).unwrap();
        assert!(rec.reconciled.is_empty(), "{:?}", rec.reconciled);
        assert_eq!(gaps_rows(&dir), Vec::<String>::new());
        assert_eq!(rec.data_seq, Some(112));

        // Newest two files without sequenced lines (only pings).
        fs::write(day.join("feed-20260930-11.tsv.zst"), frame("3\t0\t0\t{\"recorderFrame\":{}}\n")).unwrap();
        fs::write(day.join("feed-20260930-12.tsv.zst"), b"").unwrap();
        let rec = recover(&dir).unwrap();
        assert!(rec.reconciled.is_empty());
        assert_eq!(rec.data_seq, Some(110));
        assert_eq!(rec.resume_seq, Some(110));
        fs::remove_dir_all(&dir).ok();
    }

    /// Finding F1 of the 019 data audit: gaps.tsv ends with the recorder's
    /// own row for a real hole, without `\n` (torn write). Start-up ignores
    /// it (WARN), reconciles the hole and appends a row; the append first
    /// terminates the fragment, so the new row is on its own line instead of
    /// being glued into a 5-column line. A second start adds nothing.
    #[test]
    fn recover_after_own_unterminated_gap_row_does_not_glue_rows() {
        let dir = tmpdir("f1");
        let day = dir.join("2026/09/30");
        fs::create_dir_all(&day).unwrap();
        fs::write(day.join("feed-20260930-12.tsv.zst"), frame(&[env_line(1, &[100]), env_line(6, &[107])].concat()))
            .unwrap();
        fs::write(dir.join(GAPS_FILE), "101\t106\t6").unwrap();
        let rec = recover(&dir).unwrap();
        assert_eq!(rec.reconciled, vec![gap(101, 106, 6)]);
        assert_eq!(rec.skipped_gap_lines.len(), 1);
        assert_eq!(rec.skipped_gap_lines[0].why, GapsSkip::Unterminated);
        assert_eq!(fs::read_to_string(dir.join(GAPS_FILE)).unwrap(), "101\t106\t6\n101\t106\t6\n");
        let rec2 = recover(&dir).unwrap();
        assert!(rec2.reconciled.is_empty());
        assert!(rec2.skipped_gap_lines.is_empty());
        assert_eq!(gaps_rows(&dir), vec!["101\t106\t6", "101\t106\t6"]);
        fs::remove_dir_all(&dir).ok();
    }
}
