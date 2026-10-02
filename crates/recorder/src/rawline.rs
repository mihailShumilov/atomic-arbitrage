//! The one reader of raw feed lines on disk (task 021 item 1; remark В2 of
//! the 2026-10-02 review). Before 021 recovery had two parsers that
//! disagreed: the seam used the `seq_last` column, the scan the highest seq
//! of the JSON messages.
//!
//! Line format (see `lib.rs`): `recv_ns \t seq_first \t seq_last \t JSON`.
//! The highest seq of a line ([`LineSeqs::seq_max`]) is what the writer
//! tracks: equal to `seq_first` for single-message envelopes (every envelope
//! seen so far: 142 788 of 142 788 in four closed server hours, checked
//! 2026-10-02, task 021 item 7), otherwise the maximum over the messages,
//! so an internally out-of-order envelope `[110, 108]` counts as 110.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use hood_core::{FeedEnvelope, Gap};

use crate::route::intra_envelope_gaps;

/// One parsed line; the JSON column is borrowed, not parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawLine<'a> {
    /// Receive time, unix ns.
    pub recv_ns: u128,
    /// First message's seq (0 for unsequenced lines).
    pub seq_first: u64,
    /// Last message's seq (0 for unsequenced lines).
    pub seq_last: u64,
    /// The raw envelope JSON (empty if the column is missing).
    pub json: &'a [u8],
}

/// Sequence numbers of a sequenced line, as the writer accounts for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSeqs {
    /// Highest seq of the envelope.
    pub seq_max: u64,
    /// Holes between consecutive messages of the envelope.
    pub intra_gaps: Vec<Gap>,
}

fn num<T: FromStr>(col: Option<&[u8]>) -> Option<T> {
    std::str::from_utf8(col?).ok()?.parse().ok()
}

/// Parse one line (without `\n`). None if one of the three number columns
/// is missing or not a number (a seq column that does not fit `u64` is
/// rejected, not truncated).
pub fn parse_raw_line(line: &[u8]) -> Option<RawLine<'_>> {
    let mut cols = line.splitn(4, |&b| b == b'\t');
    let recv_ns = num(cols.next())?;
    let seq_first = num(cols.next())?;
    let seq_last = num(cols.next())?;
    Some(RawLine { recv_ns, seq_first, seq_last, json: cols.next().unwrap_or_default() })
}

impl RawLine<'_> {
    /// Sequence numbers of the line; None for unsequenced lines
    /// (`seq_first = seq_last = 0`). The JSON is parsed only when
    /// `seq_first != seq_last`.
    pub fn seqs(&self) -> Option<LineSeqs> {
        if self.seq_first == 0 && self.seq_last == 0 {
            return None;
        }
        if self.seq_first == self.seq_last {
            return Some(LineSeqs { seq_max: self.seq_first, intra_gaps: Vec::new() });
        }
        let fallback = LineSeqs { seq_max: self.seq_first.max(self.seq_last), intra_gaps: Vec::new() };
        match serde_json::from_slice::<FeedEnvelope>(self.json) {
            Ok(env) if !env.messages.is_empty() => {
                let seqs: Vec<u64> = env.messages.iter().map(|m| m.sequence_number).collect();
                let (intra_gaps, _) = intra_envelope_gaps(&seqs);
                Some(LineSeqs { seq_max: seqs.iter().copied().max().unwrap_or(fallback.seq_max), intra_gaps })
            }
            _ => Some(fallback),
        }
    }
}

/// Call `f` for every parseable line of an hourly file (all zstd frames, in
/// order). Lines that do not parse are skipped. An empty file is fine
/// (created, or cut back to zero after a crash; the zstd reader would report
/// "incomplete frame").
pub fn for_each_raw_line(path: &Path, mut f: impl FnMut(RawLine<'_>)) -> Result<()> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    if file.metadata().with_context(|| format!("stat {}", path.display()))?.len() == 0 {
        return Ok(());
    }
    // zstd's reader decodes concatenated frames by default.
    let dec = zstd::stream::read::Decoder::new(file).with_context(|| format!("zstd reader for {}", path.display()))?;
    for line in BufReader::with_capacity(1 << 20, dec).split(b'\n') {
        let line = line.with_context(|| format!("read {}", path.display()))?;
        if let Some(l) = parse_raw_line(&line) {
            f(l);
        }
    }
    Ok(())
}

/// Highest seq ([`LineSeqs::seq_max`]) in an hourly file; None if it has no
/// sequenced line.
pub fn file_seq_max(path: &Path) -> Result<Option<u64>> {
    let mut best: Option<u64> = None;
    for_each_raw_line(path, |l| {
        if let Some(s) = l.seqs() {
            best = Some(best.map_or(s.seq_max, |b| b.max(s.seq_max)));
        }
    })?;
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(seqs: &[u64]) -> String {
        let m: Vec<String> = seqs.iter().map(|s| format!(r#"{{"sequenceNumber":{s}}}"#)).collect();
        format!(r#"{{"version":1,"messages":[{}]}}"#, m.join(","))
    }

    #[test]
    fn parses_columns_strictly() {
        let l = parse_raw_line(b"17\t5\t5\t{\"a\":\"x\ty\"}").unwrap();
        assert_eq!((l.recv_ns, l.seq_first, l.seq_last), (17, 5, 5));
        assert_eq!(l.json, b"{\"a\":\"x\ty\"}"); // the JSON column keeps any tab
        assert_eq!(parse_raw_line(b"17\t0\t0").unwrap().json, b"");
        assert_eq!(parse_raw_line(b"17\t5"), None);
        assert_eq!(parse_raw_line(b"x\t5\t5\t{}"), None);
        assert_eq!(parse_raw_line(b"17\t-1\t5\t{}"), None);
        // 2^64 does not fit: rejected (before 021: parsed as u128, cast to u64).
        assert_eq!(parse_raw_line(b"17\t18446744073709551616\t5\t{}"), None);
        assert_eq!(parse_raw_line(b""), None);
    }

    #[test]
    fn seqs_of_single_multi_and_out_of_order_envelopes() {
        let unseq = parse_raw_line(b"1\t0\t0\t{\"recorderFrame\":{}}").unwrap();
        assert_eq!(unseq.seqs(), None);
        let single = format!("1\t7\t7\t{}", env(&[7]));
        assert_eq!(parse_raw_line(single.as_bytes()).unwrap().seqs().unwrap().seq_max, 7);
        let disorder = format!("1\t110\t108\t{}", env(&[110, 108]));
        let s = parse_raw_line(disorder.as_bytes()).unwrap().seqs().unwrap();
        assert_eq!((s.seq_max, s.intra_gaps.len()), (110, 0));
        let holes = format!("1\t100\t104\t{}", env(&[100, 102, 104]));
        let s = parse_raw_line(holes.as_bytes()).unwrap().seqs().unwrap();
        assert_eq!(s.seq_max, 104);
        assert_eq!(s.intra_gaps, vec![Gap { from: 101, to: 101 }, Gap { from: 103, to: 103 }]);
        // Broken JSON: the larger of the two columns.
        let broken = parse_raw_line(b"1\t110\t108\t{").unwrap();
        assert_eq!(broken.seqs().unwrap().seq_max, 110);
    }
}
