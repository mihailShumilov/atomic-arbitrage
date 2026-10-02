//! The one rule for `last_seq`, stale envelopes and holes (task 021 item 2;
//! remark В1 of the 2026-10-02 review). Used by the writer
//! (`FeedWriter::accept`: gaps.tsv rows, `dup_skipped`), by crash recovery
//! (`scan_seq_holes`: what the writer would have written) and by the network
//! side (`Sink::push`: `stale_frames` and the next requested seq). Before 021
//! each of the three had its own copy.

use hood_core::{detect_gap, Gap};

/// Highest seq seen so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SeqTracker {
    last: Option<u64>,
}

/// What [`SeqTracker::observe`] decided about one sequenced envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// Every seq of the envelope is `<= last`: a duplicate or replay. The
    /// writer skips it (it is never on disk); `last` is unchanged.
    Stale,
    /// New data; `last` is now the envelope's `seq_max`.
    Fresh {
        /// Hole between `last` and the envelope's first seq.
        seam: Option<Gap>,
        /// Holes inside the envelope that are not entirely `<= last`.
        intra: Vec<Gap>,
    },
}

impl SeqTracker {
    /// Start from `last` (None: nothing seen yet, no seam hole possible).
    pub fn new(last: Option<u64>) -> Self {
        Self { last }
    }

    /// Highest seq seen so far.
    pub fn last(&self) -> Option<u64> {
        self.last
    }

    /// The decision for one sequenced envelope (its first seq, its highest
    /// seq, the holes between its messages) without changing `last`.
    pub fn check(&self, seq_first: u64, seq_max: u64, intra: &[Gap]) -> Seen {
        if self.last.is_some_and(|l| seq_max <= l) {
            return Seen::Stale;
        }
        let seam = detect_gap(self.last, seq_first);
        let intra = intra.iter().copied().filter(|g| self.last.is_none_or(|s| g.to > s)).collect();
        Seen::Fresh { seam, intra }
    }

    /// Record that an envelope with highest seq `seq_max` is accepted.
    pub fn advance(&mut self, seq_max: u64) {
        self.last = Some(self.last.map_or(seq_max, |s| s.max(seq_max)));
    }

    /// [`check`](Self::check), then [`advance`](Self::advance) if fresh. The
    /// writer instead calls `advance` only after the line is written: its
    /// hour rotation commits `last` to last_seq.txt, which must never get
    /// ahead of the data.
    pub fn observe(&mut self, seq_first: u64, seq_max: u64, intra: &[Gap]) -> Seen {
        let seen = self.check(seq_first, seq_max, intra);
        if matches!(seen, Seen::Fresh { .. }) {
            self.advance(seq_max);
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(from: u64, to: u64) -> Gap {
        Gap { from, to }
    }

    #[test]
    fn first_envelope_has_no_seam() {
        let mut t = SeqTracker::default();
        assert_eq!(t.observe(100, 100, &[]), Seen::Fresh { seam: None, intra: vec![] });
        assert_eq!(t.last(), Some(100));
    }

    #[test]
    fn contiguous_gap_and_stale() {
        let mut t = SeqTracker::new(Some(99));
        assert_eq!(t.observe(100, 100, &[]), Seen::Fresh { seam: None, intra: vec![] });
        assert_eq!(t.observe(105, 105, &[]), Seen::Fresh { seam: Some(g(101, 104)), intra: vec![] });
        assert_eq!(t.observe(103, 103, &[]), Seen::Stale);
        assert_eq!(t.observe(105, 105, &[]), Seen::Stale);
        assert_eq!(t.last(), Some(105));
    }

    #[test]
    fn overlapping_envelope_keeps_only_new_intra_holes() {
        // last = 110; envelope 104, 106, 112, 115: holes 105, 107..111, 113..114.
        let mut t = SeqTracker::new(Some(110));
        let seen = t.observe(104, 115, &[g(105, 105), g(107, 111), g(113, 114)]);
        assert_eq!(seen, Seen::Fresh { seam: None, intra: vec![g(107, 111), g(113, 114)] });
        assert_eq!(t.last(), Some(115));
    }

    #[test]
    fn out_of_order_envelope_counts_by_its_max() {
        // Envelope [110, 108]: seq_first = 110, seq_max = 110. After 107 the
        // seam is taken from seq_first (108..=109), as the writer has always
        // done; after 109 there is none, and 111 follows without a hole.
        let mut t = SeqTracker::new(Some(107));
        assert_eq!(t.observe(110, 110, &[]), Seen::Fresh { seam: Some(g(108, 109)), intra: vec![] });
        let mut t = SeqTracker::new(Some(109));
        assert_eq!(t.observe(110, 110, &[]), Seen::Fresh { seam: None, intra: vec![] });
        assert_eq!(t.observe(111, 111, &[]), Seen::Fresh { seam: None, intra: vec![] });
    }

    #[test]
    fn check_does_not_advance() {
        let mut t = SeqTracker::new(Some(10));
        assert_eq!(t.check(12, 12, &[]), Seen::Fresh { seam: Some(g(11, 11)), intra: vec![] });
        assert_eq!(t.last(), Some(10));
        t.advance(12);
        t.advance(11);
        assert_eq!(t.last(), Some(12));
        assert_eq!(t.check(12, 12, &[]), Seen::Stale);
    }

    #[test]
    fn no_overflow_at_u64_max() {
        let mut t = SeqTracker::new(Some(u64::MAX - 1));
        assert_eq!(t.observe(u64::MAX, u64::MAX, &[]), Seen::Fresh { seam: None, intra: vec![] });
        assert_eq!(t.observe(u64::MAX, u64::MAX, &[]), Seen::Stale);
    }
}
