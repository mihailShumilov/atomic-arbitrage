//! Resume from the last recorded block via `Arbitrum-Requested-Sequence-Number`
//! (task 009, items 1 and 2). Pure bookkeeping, no I/O.
//!
//! Verified 2026-09-30 (task 005, 3 probe connections): the public feed starts
//! at the requested sequence number if it is inside its backlog (~615-690
//! blocks, ~63-69 s in two measurements), otherwise it silently sends the
//! whole backlog from its start. Without the header it starts at the tip.
//! The Nitro client sends exactly `Arbitrum-Feed-Client-Version: 2` and
//! `Arbitrum-Requested-Sequence-Number: <decimal>`.
//!
//! What counts as "backlog" in the statistics: the sequenced frames of a
//! connection before the first frame whose newest kind-3 `header.timestamp`
//! lags the local receive time by less than [`LIVE_LAG`] (2 s). That frame is
//! the first "live" one and is not counted. Live frames lag 0.2-1.5 s
//! (1-s timestamp resolution); backlog frames lag up to ~65 s.
//!
//! Why not "first pause > 30 ms" (the rule of the 005 data audit): in the
//! 009 live check (2026-10-01, session B) the backlog of ~630 blocks arrived
//! with pauses of 98.8 ms after 6 blocks and 38.2 ms after 19 blocks (TCP
//! slow start at the beginning of the transfer, presumably), so that rule
//! ended the backlog at 6 blocks. The lag rule depends on the local clock
//! being NTP-synced (an error of a few seconds shifts the boundary) and only
//! looks at kind-3 messages: delayed messages (kind 9/13) carry an L1 inbox
//! time hundreds of seconds old. Without the header the first frame is
//! already live, so `backlog_blocks` is 0 there.

use std::time::Duration;

/// A frame whose kind-3 `header.timestamp` lags receive time by less than
/// this is live; it ends the backlog.
pub const LIVE_LAG: Duration = Duration::from_secs(2);

/// Why a connection does or does not carry the resume header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMode {
    /// Header sent with `last_seq + 1`.
    Header,
    /// No data yet (fresh out-dir): no header, the stream starts at the tip.
    NoData,
    /// `--no-requested-seq`.
    Disabled,
}

impl ResumeMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ResumeMode::Header => "header",
            ResumeMode::NoData => "no_data",
            ResumeMode::Disabled => "disabled",
        }
    }
}

/// The value of `Arbitrum-Requested-Sequence-Number` for the next connection.
/// `last_seq` is the highest seq the process has handed to the writer (at
/// start-up: the highest seq found in the data by `recover`).
pub fn requested_seq(last_seq: Option<u64>, enabled: bool) -> (Option<u64>, ResumeMode) {
    if !enabled {
        return (None, ResumeMode::Disabled);
    }
    match last_seq.and_then(|s| s.checked_add(1)) {
        Some(n) => (Some(n), ResumeMode::Header),
        None => (None, ResumeMode::NoData),
    }
}

/// Per-connection statistics of the backlog (see module docs).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backlog {
    pub requested: Option<u64>,
    /// `last_seq` of the process when the connection was made.
    pub last_seq_before: Option<u64>,
    pub first_seq: Option<u64>,
    /// Lag of the first sequenced frame (recv - kind-3 header.timestamp), ms.
    pub first_lag_ms: Option<i64>,
    /// Blocks (messages) in sequenced frames before the first live frame.
    pub blocks: u64,
    /// Highest seq of the backlog.
    pub end_seq: Option<u64>,
    /// Receive time of the first sequenced frame.
    pub first_ns: Option<u128>,
    /// First live frame: its seq_first, lag and arrival after the first frame.
    pub live_seq: Option<u64>,
    pub live_lag_ms: Option<i64>,
    pub live_after: Duration,
    /// Frames whose seqs were all <= last_seq at arrival (the writer skips
    /// them, `dup_skipped`), counted over the whole connection.
    pub stale_frames: u64,
    pub done: bool,
}

fn lag_ms(recv_ns: u128, ts: u64) -> i64 {
    (recv_ns / 1_000_000) as i64 - (ts as i64) * 1000
}

impl Backlog {
    pub fn new(requested: Option<u64>, last_seq_before: Option<u64>) -> Self {
        Self { requested, last_seq_before, ..Default::default() }
    }

    /// Feed one sequenced frame (`kind3_ts`: its newest kind-3
    /// header.timestamp, if any). Returns true exactly once, when this frame
    /// is the first live one and so ends the backlog.
    pub fn observe(&mut self, recv_ns: u128, seq_first: u64, seq_max: u64, kind3_ts: Option<u64>, stale: bool) -> bool {
        if stale {
            self.stale_frames += 1;
        }
        if self.done {
            return false;
        }
        let lag = kind3_ts.map(|ts| lag_ms(recv_ns, ts));
        if self.first_seq.is_none() {
            self.first_seq = Some(seq_first);
            self.first_ns = Some(recv_ns);
            self.first_lag_ms = lag;
        }
        if lag.is_some_and(|l| l < LIVE_LAG.as_millis() as i64) {
            self.done = true;
            self.live_seq = Some(seq_first);
            self.live_lag_ms = lag;
            let first = self.first_ns.unwrap_or(recv_ns);
            self.live_after = Duration::from_nanos(recv_ns.saturating_sub(first).min(u64::MAX as u128) as u64);
            return true;
        }
        self.blocks += seq_max.saturating_sub(seq_first) + 1;
        self.end_seq = Some(self.end_seq.map_or(seq_max, |e| e.max(seq_max)));
        false
    }

    /// `first_seq - requested`: 0 if the server honoured the header, > 0 if
    /// it started later (requested is older than its backlog).
    pub fn first_minus_requested(&self) -> Option<i128> {
        Some(self.first_seq? as i128 - self.requested? as i128)
    }

    /// One-line summary for connections.tsv (`key=value`, no tabs).
    pub fn detail(&self) -> String {
        fn d<T: ToString>(v: Option<T>) -> String {
            v.map_or_else(|| "-".to_string(), |x| x.to_string())
        }
        format!(
            "requested={} last_seq_before={} first_seq={} first_minus_requested={} first_lag_ms={} backlog_blocks={} backlog_end_seq={} live_seq={} live_lag_ms={} live_after_ms={} stale_frames={} complete={}",
            d(self.requested),
            d(self.last_seq_before),
            d(self.first_seq),
            d(self.first_minus_requested()),
            d(self.first_lag_ms),
            self.blocks,
            d(self.end_seq),
            d(self.live_seq),
            d(self.live_lag_ms),
            self.live_after.as_millis(),
            self.stale_frames,
            self.done,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u128 = 1_000_000;
    const SEC: u128 = 1_000_000_000;

    #[test]
    fn requested_from_last_seq() {
        assert_eq!(requested_seq(Some(76_661_958), true), (Some(76_661_959), ResumeMode::Header));
        assert_eq!(requested_seq(None, true), (None, ResumeMode::NoData));
        assert_eq!(requested_seq(Some(5), false), (None, ResumeMode::Disabled));
        assert_eq!(requested_seq(None, false), (None, ResumeMode::Disabled));
        assert_eq!(requested_seq(Some(u64::MAX), true), (None, ResumeMode::NoData));
        assert_eq!(ResumeMode::Header.as_str(), "header");
    }

    /// Timing modelled on session B of the 009 live check (2026-10-01,
    /// data/feed-test-009, seqs 77169713..): the backlog starts 63.6 s
    /// behind, has short pauses (98.8 ms after 6 blocks) and lags fall
    /// as timestamps advance; the first frame with lag < 2 s
    /// ends it.
    #[test]
    fn backlog_ends_at_first_frame_lagging_under_2s() {
        let mut b = Backlog::new(Some(77_169_135), Some(77_169_134));
        let t0 = 1_790_837_152 * SEC + 630 * MS; // recv of the first frame
        let ts0 = 1_790_837_089u64; // its header.timestamp: lag 63.63 s
        let mut recv = t0;
        for i in 0..630u64 {
            // a 99 ms pause after 6 blocks must not end the backlog
            recv += if i == 6 { 99 * MS } else { MS };
            let ts = ts0 + i / 11;
            assert!(!b.observe(recv, 77_169_713 + i, 77_169_713 + i, Some(ts), false), "i={i}");
        }
        // Delayed message with an old timestamp: still backlog.
        recv += MS;
        assert!(!b.observe(recv, 77_170_343, 77_170_343, None, false));
        // Live: lag 1.4 s.
        let live_recv = 1_790_837_153 * SEC + 420 * MS;
        assert!(b.observe(live_recv, 77_170_344, 77_170_344, Some(1_790_837_152), false));
        assert!(!b.observe(live_recv + 113 * MS, 77_170_345, 77_170_345, Some(1_790_837_152), false));
        assert!(b.done);
        assert_eq!(b.first_seq, Some(77_169_713));
        assert_eq!(b.first_lag_ms, Some(63_631));
        assert_eq!(b.blocks, 631);
        assert_eq!(b.end_seq, Some(77_170_343));
        assert_eq!(b.live_seq, Some(77_170_344));
        assert_eq!(b.live_lag_ms, Some(1_420));
        assert_eq!(b.first_minus_requested(), Some(578));
        let d = b.detail();
        assert!(d.starts_with("requested=77169135 last_seq_before=77169134 first_seq=77169713 first_minus_requested=578 first_lag_ms=63631 backlog_blocks=631 backlog_end_seq=77170343 live_seq=77170344 live_lag_ms=1420"), "{d}");
        assert!(d.ends_with("stale_frames=0 complete=true"), "{d}");
        assert!(!d.contains('\t'));
    }

    /// Real data: the first 641 sequenced frames of session B of the 009
    /// live check (2026-10-01 06:45:52Z, requested 77169135, last_seq_before
    /// 77169134, data/feed-test-009). The server started at 77169713 (older
    /// requests are outside its backlog); the first kind-3 frame lagging
    /// < 2 s is 77170334 (lag 1.42 s), so the backlog is 621 blocks. The old
    /// "first pause > 30 ms" rule would have stopped after 6 blocks (98.8 ms
    /// pause at 77169719).
    #[test]
    fn real_session_b_backlog() {
        let fixture = include_str!("../tests/fixtures/009-session-b-head.tsv");
        let mut b = Backlog::new(Some(77_169_135), Some(77_169_134));
        let mut ended_at = None;
        let mut first_pause_over_30ms = None;
        let mut prev_ns: Option<u128> = None;
        for (i, line) in fixture.lines().filter(|l| !l.starts_with('#')).enumerate() {
            let c: Vec<&str> = line.split('\t').collect();
            let ns: u128 = c[0].parse().unwrap();
            let (sf, sm): (u64, u64) = (c[1].parse().unwrap(), c[2].parse().unwrap());
            let ts = c[3].parse::<u64>().ok();
            if prev_ns.is_some_and(|p| ns - p > 30 * MS) && first_pause_over_30ms.is_none() {
                first_pause_over_30ms = Some(sf);
            }
            prev_ns = Some(ns);
            if b.observe(ns, sf, sm, ts, false) {
                ended_at = Some(i);
            }
        }
        assert_eq!(first_pause_over_30ms, Some(77_169_719));
        assert_eq!(ended_at, Some(621));
        assert_eq!(b.first_seq, Some(77_169_713));
        assert_eq!(b.first_minus_requested(), Some(578));
        assert_eq!(b.first_lag_ms, Some(63_631));
        assert_eq!(b.blocks, 621);
        assert_eq!(b.end_seq, Some(77_170_333));
        assert_eq!(b.live_seq, Some(77_170_334));
        assert_eq!(b.live_lag_ms, Some(1_421));
        assert_eq!(b.live_after.as_millis(), 790);
    }

    #[test]
    fn header_honoured_and_stale_frames() {
        let mut b = Backlog::new(Some(105), Some(104));
        let now = 1_790_837_000 * SEC;
        let old = Some(1_790_836_990); // lag 10 s
        b.observe(now, 103, 103, old, true); // replayed, stale
        b.observe(now + MS, 105, 105, old, false);
        b.observe(now + 2 * MS, 106, 107, old, false); // two-message envelope
        assert!(b.observe(now + 3 * MS, 108, 108, Some(1_790_836_999), false));
        assert_eq!(b.stale_frames, 1);
        assert_eq!(b.first_seq, Some(103));
        assert_eq!(b.blocks, 4);
        assert_eq!(b.live_after, Duration::from_millis(3));
        // Stale frames after the backlog are still counted.
        b.observe(now + SEC, 50, 50, None, true);
        assert_eq!(b.stale_frames, 2);
        assert_eq!(b.first_minus_requested(), Some(-2));
    }

    /// Without the header the stream starts at the tip: the first frame is
    /// live (lag 0.71 s in session A of the 009 live check), backlog 0.
    #[test]
    fn no_header_first_frame_is_live() {
        let mut b = Backlog::new(None, None);
        let recv = 1_790_836_432 * SEC + 710 * MS;
        assert!(b.observe(recv, 77_163_198, 77_163_198, Some(1_790_836_432), false));
        let d = b.detail();
        assert!(d.starts_with("requested=- last_seq_before=- first_seq=77163198 first_minus_requested=- first_lag_ms=710 backlog_blocks=0 backlog_end_seq=- live_seq=77163198 live_lag_ms=710 live_after_ms=0"), "{d}");
    }

    #[test]
    fn unfinished_backlog() {
        let mut b = Backlog::new(Some(7), Some(6));
        b.observe(100 * SEC, 7, 7, Some(10), false);
        b.observe(100 * SEC + MS, 8, 8, None, false);
        assert!(!b.done);
        let d = b.detail();
        assert!(d.contains("backlog_blocks=2 backlog_end_seq=8 live_seq=- live_lag_ms=- live_after_ms=0"), "{d}");
        assert!(d.ends_with("complete=false"), "{d}");
    }
}
