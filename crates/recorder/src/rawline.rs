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

use anyhow::{Context, Result};
use hood_core::{FeedEnvelope, Gap};

use crate::route::envelope_seqs;

/// The line itself and its column parser live in `hood_core::feedline`
/// (task 032: the loader reads the same lines for `feed_recv_ns`).
pub use hood_core::feedline::{parse_raw_line, RawLine};

/// Sequence numbers of a sequenced line, as the writer accounts for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSeqs {
    /// Highest seq of the envelope.
    pub seq_max: u64,
    /// Holes between consecutive messages of the envelope.
    pub intra_gaps: Vec<Gap>,
}

/// Sequence numbers of the line; None for unsequenced lines
/// (`seq_first = seq_last = 0`). The JSON is always parsed (only at
/// start-up, at most three hourly files): until the 025 review a line
/// with `seq_first == seq_last` skipped it, so an envelope like
/// `[7, 9, 7]` counted as 7 without the hole 8 here but as 9 with the
/// hole in the live writer.
pub fn line_seqs(l: &RawLine<'_>) -> Option<LineSeqs> {
    if l.is_unsequenced() {
        return None;
    }
    // Same derivation as for live frames (`route::envelope_seqs`); the
    // larger column if the JSON is broken or has no messages.
    let parsed = serde_json::from_slice::<FeedEnvelope>(l.json).ok();
    Some(match parsed.as_ref().and_then(envelope_seqs) {
        Some(s) => LineSeqs { seq_max: s.seq_max, intra_gaps: s.intra_gaps },
        None => LineSeqs { seq_max: l.seq_first.max(l.seq_last), intra_gaps: Vec::new() },
    })
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
        if let Some(s) = line_seqs(&l) {
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
    fn seqs_of_single_multi_and_out_of_order_envelopes() {
        let unseq = parse_raw_line(b"1\t0\t0\t{\"recorderFrame\":{}}").unwrap();
        assert_eq!(line_seqs(&unseq), None);
        let single = format!("1\t7\t7\t{}", env(&[7]));
        assert_eq!(line_seqs(&parse_raw_line(single.as_bytes()).unwrap()).unwrap().seq_max, 7);
        let disorder = format!("1\t110\t108\t{}", env(&[110, 108]));
        let s = line_seqs(&parse_raw_line(disorder.as_bytes()).unwrap()).unwrap();
        assert_eq!((s.seq_max, s.intra_gaps.len()), (110, 0));
        let holes = format!("1\t100\t104\t{}", env(&[100, 102, 104]));
        let s = line_seqs(&parse_raw_line(holes.as_bytes()).unwrap()).unwrap();
        assert_eq!(s.seq_max, 104);
        assert_eq!(s.intra_gaps, vec![Gap { from: 101, to: 101 }, Gap { from: 103, to: 103 }]);
        // Broken JSON: the larger of the two columns.
        let broken = parse_raw_line(b"1\t110\t108\t{").unwrap();
        assert_eq!(line_seqs(&broken).unwrap().seq_max, 110);
    }

    /// Task 025 item 1: live routing and the disk reader derive seq_max and
    /// the holes in one place (`route::envelope_seqs`), so they agree.
    #[test]
    fn disk_reader_agrees_with_live_routing() {
        let cases: [&[u64]; 8] =
            [&[7], &[100, 101, 105, 106, 108], &[110, 108], &[10, 12, 11], &[5, 5], &[3, 9, 4], &[7, 9, 7], &[7, 3, 7]];
        for seqs in cases {
            let raw = env(seqs);
            let live = crate::route::route_text(1, raw.clone());
            let line = format!("1\t{}\t{}\t{raw}", live.seq_first, live.seq_last);
            let s = line_seqs(&parse_raw_line(line.as_bytes()).unwrap()).unwrap();
            assert_eq!((s.seq_max, s.intra_gaps), (live.seq_max, live.intra_gaps), "{seqs:?}");
        }
        // No messages: the columns decide (unchanged).
        let empty = parse_raw_line(b"1\t4\t6\t{\"version\":1,\"messages\":[]}").unwrap();
        assert_eq!(line_seqs(&empty), Some(LineSeqs { seq_max: 6, intra_gaps: vec![] }));
    }
}
