//! Reconnect pause policy (task 002, item 4).
//!
//! Pure logic: the caller supplies the outcome of a connection attempt and a
//! random number in [0, 1); this module decides how long to wait. No I/O here
//! so the whole ladder is unit-tested without touching the feed.
//!
//! Rules:
//! - Normal close by the server (or idle stream) after a healthy session:
//!   1..5 s with jitter.
//! - HTTP 429: honour `Retry-After` if present, else 5 -> 10 -> 20 -> 40 -> 60
//!   min (cap 60 min).
//! - HTTP 403 / refused upgrade: same ladder, starting at 15 min.
//! - A session that stayed connected for >= 10 min resets the ladder.
//! - Frames without blocks for `--block-idle-timeout-secs` (task 012 item 3):
//!   always the transient ladder below, so a persistent stall reconnects at
//!   most ~11 times an hour instead of every ~35 s.
//! - Not specified by the task, added defensively: network/TLS errors, 5xx and
//!   sessions closed within 30 s of connecting use a gentle transient ladder
//!   5 s -> 10 s -> ... -> 5 min, so a flapping server does not turn into a
//!   burst of reconnects that would earn a 429.

use std::time::Duration;

/// Ban/limit ladder ceiling.
pub const LADDER_CAP: Duration = Duration::from_secs(60 * 60);
/// Base pause for 429 without `Retry-After`.
pub const BASE_429: Duration = Duration::from_secs(5 * 60);
/// Base pause for 403 / refused upgrade.
pub const BASE_403: Duration = Duration::from_secs(15 * 60);
/// A session at least this long resets both ladders.
pub const RESET_AFTER: Duration = Duration::from_secs(10 * 60);
/// Sessions shorter than this that end in a "normal" close are treated as
/// flapping and go to the transient ladder.
pub const SHORT_SESSION: Duration = Duration::from_secs(30);
/// Transient ladder: base and ceiling.
pub const TRANSIENT_BASE: Duration = Duration::from_secs(5);
pub const TRANSIENT_CAP: Duration = Duration::from_secs(5 * 60);
/// Upper sanity bound on a server-supplied `Retry-After`.
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(6 * 60 * 60);

/// Why a connection attempt or session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndKind {
    /// Server closed the stream (close frame, EOF, reset) after the upgrade.
    ServerClosed,
    /// No frame for `idle_timeout`; we closed the connection.
    Idle,
    /// Frames (pings, confirmations) but no block (seq > 0) for
    /// `--block-idle-timeout-secs`; we closed the connection (task 012 item 3).
    BlockIdle,
    /// HTTP 429 on the upgrade request.
    RateLimited,
    /// HTTP 403 or any other refused upgrade (4xx, missing Upgrade header, redirect).
    Forbidden,
    /// 5xx on the upgrade request.
    HttpError,
    /// DNS / TCP / TLS / handshake timeout.
    NetError,
}

impl EndKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EndKind::ServerClosed => "server_closed",
            EndKind::Idle => "idle_timeout",
            EndKind::BlockIdle => "block_idle",
            EndKind::RateLimited => "http_429",
            EndKind::Forbidden => "forbidden",
            EndKind::HttpError => "http_error",
            EndKind::NetError => "net_error",
        }
    }
}

/// Which rule produced the pause (logged to connections.tsv).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    NormalJitter,
    RetryAfter,
    Ladder429,
    Ladder403,
    Transient,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Rule::NormalJitter => "jitter_1_5s",
            Rule::RetryAfter => "retry_after",
            Rule::Ladder429 => "ladder_429",
            Rule::Ladder403 => "ladder_403",
            Rule::Transient => "transient",
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Ladder {
    /// Consecutive 429/403 outcomes since the last reset.
    pub strikes: u32,
    /// Consecutive transient failures since the last healthy session.
    pub transient: u32,
}

fn doubled(base: Duration, n: u32, cap: Duration) -> Duration {
    // 2^n overflows quickly; anything past 2^16 is way above every cap.
    let factor = 1u32 << n.min(16);
    base.saturating_mul(factor).min(cap)
}

impl Ladder {
    /// Decide the pause after a connection ended.
    ///
    /// `session` is how long the WebSocket stayed upgraded (zero if the
    /// upgrade never succeeded). `rand01` must be in [0, 1).
    pub fn next_pause(
        &mut self,
        kind: EndKind,
        retry_after: Option<Duration>,
        session: Duration,
        rand01: f64,
    ) -> (Duration, Rule) {
        let rand01 = rand01.clamp(0.0, 0.999_999);
        if session >= RESET_AFTER {
            self.strikes = 0;
            self.transient = 0;
        }
        match kind {
            EndKind::RateLimited => {
                let out = match retry_after {
                    Some(ra) => (ra.clamp(Duration::from_secs(1), RETRY_AFTER_MAX), Rule::RetryAfter),
                    None => (doubled(BASE_429, self.strikes, LADDER_CAP), Rule::Ladder429),
                };
                self.strikes = self.strikes.saturating_add(1);
                out
            }
            EndKind::Forbidden => {
                let ladder = doubled(BASE_403, self.strikes, LADDER_CAP);
                self.strikes = self.strikes.saturating_add(1);
                match retry_after {
                    // Never shorter than the ladder, but respect a longer hint.
                    Some(ra) if ra.min(RETRY_AFTER_MAX) > ladder => (ra.min(RETRY_AFTER_MAX), Rule::RetryAfter),
                    _ => (ladder, Rule::Ladder403),
                }
            }
            EndKind::ServerClosed | EndKind::Idle if session >= SHORT_SESSION => {
                self.transient = 0;
                // 1..5 s uniformly.
                let ms = 1000 + (rand01 * 4000.0) as u64;
                (Duration::from_millis(ms), Rule::NormalJitter)
            }
            // A stalled stream (frames without blocks) always takes the
            // escalating transient ladder, also after a >= 30 s session: if
            // the stall persists across reconnects, 1..5 s pauses would mean
            // ~100 connects an hour. A session >= RESET_AFTER (blocks flowed
            // for 10 min before the stall) still starts it from the bottom.
            EndKind::ServerClosed | EndKind::Idle | EndKind::BlockIdle | EndKind::HttpError | EndKind::NetError => {
                let base = doubled(TRANSIENT_BASE, self.transient, TRANSIENT_CAP);
                self.transient = self.transient.saturating_add(1);
                // +-20 % jitter, still capped.
                let f = 0.8 + 0.4 * rand01;
                let p = Duration::from_secs_f64(base.as_secs_f64() * f).min(TRANSIENT_CAP);
                (p, Rule::Transient)
            }
        }
    }
}

/// Why the process waits before its first connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupWaitReason {
    /// A pause chosen by the previous process (e.g. `Retry-After` of a ban).
    PendingPause,
    /// Less than `--min-connect-interval-secs` since the end of the previous
    /// session (task 012 item 1; before that: since the last `connected`).
    MinConnectInterval,
}

impl StartupWaitReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StartupWaitReason::PendingPause => "pending_pause",
            StartupWaitReason::MinConnectInterval => "min_connect_interval",
        }
    }
}

/// Where the end of the previous session was taken from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndSource {
    /// Last row of connections.tsv after the last `connected`.
    LogRow,
    /// mtime of the newest hourly data file (newer than every log row after
    /// `connected`, or no such row: kill -9).
    DataMtime,
    /// Only the `connected` row itself (no later row, no newer data).
    Connected,
}

impl SessionEndSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionEndSource::LogRow => "log_row",
            SessionEndSource::DataMtime => "data_mtime",
            SessionEndSource::Connected => "connected",
        }
    }
}

/// End of the previous session (task 012, item 1), unix ns.
///
/// - `connected_ns` / `last_row_ns`: the last `connected` row of
///   connections.tsv and the newest row of any type after it
///   ([`crate::net::LogSession`]). No `connected` -> None (no wait).
/// - `data_mtime_ns`: mtime of the newest hourly data file, read before
///   crash recovery.
///
/// The task's rule is "last row after `connected`, else (kill -9) the data
/// mtime". The later of the two is used in both cases: since task 009 a
/// `backlog` row follows `connected` about 1 s into every session, so after
/// kill -9 in the middle of a long session the last row would be that early
/// `backlog` row, while the data file was written until (at most
/// `--frame-secs` before) the kill. Taking the later time never makes the
/// wait shorter than the task's rule.
pub fn session_end_ns(
    connected_ns: Option<u128>,
    last_row_ns: Option<u128>,
    data_mtime_ns: Option<u128>,
) -> Option<(u128, SessionEndSource)> {
    // No `connected` in the log: no session of this out-dir to space from
    // (fresh dir, copied data); as before task 012, no interval wait.
    let connected = connected_ns?;
    let log = match last_row_ns {
        Some(r) => (r, SessionEndSource::LogRow),
        None => (connected, SessionEndSource::Connected),
    };
    match data_mtime_ns {
        Some(m) if m > log.0 => Some((m, SessionEndSource::DataMtime)),
        _ => Some(log),
    }
}

/// How long to wait before the first connection after a (re)start (task 008
/// item 2, task 012 item 1). `pending_until_ns` is the end of the last chosen
/// reconnect pause, `session_end_ns` the end of the previous session (see
/// [`session_end_ns`]). Waits `max(pause remainder, min_interval - (now -
/// end))`; None if neither is still running.
pub fn startup_wait(
    now_ns: u128,
    pending_until_ns: Option<u128>,
    session_end_ns: Option<u128>,
    min_interval: Duration,
) -> Option<(Duration, StartupWaitReason)> {
    let left = |until: u128| Duration::from_nanos(until.saturating_sub(now_ns).min(u64::MAX as u128) as u64);
    let pending = pending_until_ns.map(left).unwrap_or_default();
    let interval = session_end_ns.map(|t| left(t.saturating_add(min_interval.as_nanos()))).unwrap_or_default();
    match (pending.is_zero(), interval.is_zero()) {
        (true, true) => None,
        _ if pending >= interval => Some((pending, StartupWaitReason::PendingPause)),
        _ => Some((interval, StartupWaitReason::MinConnectInterval)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S0: Duration = Duration::ZERO;

    fn minutes(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    const SEC: u128 = 1_000_000_000;

    /// Acceptance for item 2: `connected` 30 s ago with the default 120 s
    /// interval -> wait ~90 s.
    #[test]
    fn startup_wait_after_recent_connect() {
        let now = 1_790_800_000 * SEC;
        let min = Duration::from_secs(120);
        let (w, r) = startup_wait(now, None, Some(now - 30 * SEC), min).unwrap();
        assert_eq!(w, Duration::from_secs(90));
        assert_eq!(r, StartupWaitReason::MinConnectInterval);
        assert_eq!(r.as_str(), "min_connect_interval");
        // Long ago, or never connected: no wait.
        assert_eq!(startup_wait(now, None, Some(now - 121 * SEC), min), None);
        assert_eq!(startup_wait(now, None, Some(now - 120 * SEC), min), None);
        assert_eq!(startup_wait(now, None, None, min), None);
        // Interval disabled.
        assert_eq!(startup_wait(now, None, Some(now - SEC), Duration::ZERO), None);
        // Clock went backwards (connected "in the future"): wait at most the interval.
        let (w, _) = startup_wait(now, None, Some(now + 5 * SEC), min).unwrap();
        assert_eq!(w, Duration::from_secs(125));
    }

    #[test]
    fn startup_wait_takes_the_longer_of_pause_and_interval() {
        let now = 1_790_800_000 * SEC;
        let min = Duration::from_secs(120);
        // Ban pause (3600 s from 10 s ago) beats the interval.
        let (w, r) = startup_wait(now, Some(now + 3590 * SEC), Some(now - 11 * SEC), min).unwrap();
        assert_eq!((w, r), (Duration::from_secs(3590), StartupWaitReason::PendingPause));
        // Short pause already over, interval still running.
        let (w, r) = startup_wait(now, Some(now - SEC), Some(now - 100 * SEC), min).unwrap();
        assert_eq!((w, r), (Duration::from_secs(20), StartupWaitReason::MinConnectInterval));
        // Pause still running, no connect in the log.
        let (w, r) = startup_wait(now, Some(now + 3 * SEC), None, min).unwrap();
        assert_eq!((w, r), (Duration::from_secs(3), StartupWaitReason::PendingPause));
    }

    /// Task 012 item 1, case 1: the previous session ended cleanly (rows
    /// after `connected`). A 10-minute session that ended 30 s ago -> wait
    /// ~90 s, although the last `connected` is long past (before 012: no wait).
    #[test]
    fn interval_counts_from_last_row_after_connected() {
        let now = 1_790_800_000 * SEC;
        let min = Duration::from_secs(120);
        let log = crate::net::parse_last_session(&format!(
            "# ts_utc\tts_unix_ns\tevent\treason\thttp_status\tretry_after\tpause_s\tsession_s\tenvelopes\tstrikes\tdetail\n\
             x\t{}\tconnected\t-\t101\t-\t-\t-\t-\t0\tws://a requested=- mode=no_data\n\
             x\t{}\tbacklog\tdone\t101\t-\t-\t0.500\t5\t-\trequested=-\n\
             x\t{}\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-\n\
             x\t{}\tclient_close\tserver_replied\t101\t-\t-\t600.000\t6000\t-\tsent close 1000\n",
            now - 630 * SEC,
            now - 629 * SEC,
            now - 31 * SEC,
            now - 30 * SEC
        ))
        .unwrap();
        assert_eq!(log.connected_ns, now - 630 * SEC);
        assert_eq!(log.last_row_ns, Some(now - 30 * SEC));
        // Data mtime at the final commit, a few ms before the client_close row.
        let (end, src) =
            session_end_ns(Some(log.connected_ns), log.last_row_ns, Some(now - 30 * SEC - 5_000_000)).unwrap();
        assert_eq!((end, src), (now - 30 * SEC, SessionEndSource::LogRow));
        let (w, r) = startup_wait(now, None, Some(end), min).unwrap();
        assert_eq!((w, r), (Duration::from_secs(90), StartupWaitReason::MinConnectInterval));
        // Ended more than the interval ago: no wait.
        let (end, _) = session_end_ns(Some(now - 900 * SEC), Some(now - 121 * SEC), None).unwrap();
        assert_eq!(startup_wait(now, None, Some(end), min), None);
    }

    /// Case 2: kill -9 -> nothing after `connected` in the log; the end is
    /// the mtime of the newest hourly file. Also: only an early `backlog` row
    /// after `connected` (kill -9 mid-session since task 009) -> data mtime.
    #[test]
    fn interval_after_kill9_counts_from_data_mtime() {
        let now = 1_790_800_000 * SEC;
        let min = Duration::from_secs(120);
        let log = crate::net::parse_last_session(&format!(
            "x\t{}\tdisconnected\tserver_closed\t101\t-\t2.000\t50.000\t9\t0\trule=jitter_1_5s\n\
             x\t{}\tconnected\t-\t101\t-\t-\t-\t-\t0\tws://a\n",
            now - 700 * SEC,
            now - 600 * SEC
        ))
        .unwrap();
        assert_eq!(log.last_row_ns, None);
        let (end, src) = session_end_ns(Some(log.connected_ns), log.last_row_ns, Some(now - 40 * SEC)).unwrap();
        assert_eq!((end, src), (now - 40 * SEC, SessionEndSource::DataMtime));
        let (w, _) = startup_wait(now, None, Some(end), min).unwrap();
        assert_eq!(w, Duration::from_secs(80));
        // Early backlog row, data written much later.
        let (end, src) = session_end_ns(Some(now - 600 * SEC), Some(now - 599 * SEC), Some(now - 10 * SEC)).unwrap();
        assert_eq!((end, src), (now - 10 * SEC, SessionEndSource::DataMtime));
        // No data at all (connected, killed before the first commit): the
        // connected row is the best we have.
        let (end, src) = session_end_ns(Some(now - 20 * SEC), None, None).unwrap();
        assert_eq!((end, src), (now - 20 * SEC, SessionEndSource::Connected));
        // No `connected` in the log (fresh dir, copied data): no end, even
        // with recent data.
        assert_eq!(session_end_ns(None, None, Some(now - 5 * SEC)), None);
        assert_eq!(session_end_ns(None, None, None), None);
    }

    /// Case 3: a pending pause and the interval from the end of the session
    /// -> the longer remainder wins, both ways round.
    #[test]
    fn pending_pause_and_end_interval_take_the_longer() {
        let now = 1_790_800_000 * SEC;
        let min = Duration::from_secs(120);
        // disconnected 403 with a 900 s pause 100 s ago, then SIGTERM 99 s ago.
        let log = crate::net::parse_last_session(&format!(
            "x\t{}\tconnected\t-\t101\t-\t-\t-\t-\t0\tws://a\n\
             x\t{}\tdisconnected\tforbidden\t403\t-\t900.000\t0.000\t0\t1\tupgrade\n\
             x\t{}\tshutdown\tSIGTERM\t-\t-\t-\t-\t-\t-\t-\n",
            now - 400 * SEC,
            now - 100 * SEC,
            now - 99 * SEC
        ))
        .unwrap();
        let pause_row =
            format!("x\t{}\tdisconnected\tforbidden\t403\t-\t900.000\t0.000\t0\t1\tupgrade", now - 100 * SEC);
        let pending = crate::net::parse_pause_row(&pause_row).unwrap();
        let (end, _) = session_end_ns(Some(log.connected_ns), log.last_row_ns, None).unwrap();
        assert_eq!(end, now - 99 * SEC);
        let (w, r) = startup_wait(now, Some(pending.not_before_ns), Some(end), min).unwrap();
        assert_eq!((w, r), (Duration::from_secs(800), StartupWaitReason::PendingPause));
        // Short pause (3 s, from 1 s ago) vs. a session that ended 1 s ago:
        // the interval (119 s) wins.
        let (w, r) = startup_wait(now, Some(now + 2 * SEC), Some(now - SEC), min).unwrap();
        assert_eq!((w, r), (Duration::from_secs(119), StartupWaitReason::MinConnectInterval));
    }

    #[test]
    fn ladder_429_without_retry_after() {
        let mut l = Ladder::default();
        let got: Vec<u64> =
            (0..7).map(|_| l.next_pause(EndKind::RateLimited, None, S0, 0.5).0.as_secs() / 60).collect();
        assert_eq!(got, vec![5, 10, 20, 40, 60, 60, 60]);
    }

    #[test]
    fn ladder_403_starts_at_15() {
        let mut l = Ladder::default();
        let got: Vec<u64> = (0..5).map(|_| l.next_pause(EndKind::Forbidden, None, S0, 0.5).0.as_secs() / 60).collect();
        assert_eq!(got, vec![15, 30, 60, 60, 60]);
    }

    #[test]
    fn retry_after_is_honoured_for_429() {
        let mut l = Ladder::default();
        let (p, r) = l.next_pause(EndKind::RateLimited, Some(Duration::from_secs(90)), S0, 0.1);
        assert_eq!((p, r), (Duration::from_secs(90), Rule::RetryAfter));
        // It still counts as a strike: the next 429 without header escalates.
        let (p, _) = l.next_pause(EndKind::RateLimited, None, S0, 0.1);
        assert_eq!(p, minutes(10));
        // Absurd values are capped.
        let (p, _) = l.next_pause(EndKind::RateLimited, Some(Duration::from_secs(10_000_000)), S0, 0.1);
        assert_eq!(p, RETRY_AFTER_MAX);
    }

    /// Task 019: the shared parser (hood_core::http) accepts fractional
    /// seconds; before 019 "90.5" fell back to the 429 ladder (5 min).
    #[test]
    fn fractional_and_date_retry_after_reach_the_ladder() {
        use hood_core::http::parse_retry_after;
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = UNIX_EPOCH + Duration::from_secs(784_111_777 - 120);
        let mut l = Ladder::default();
        let ra = parse_retry_after("90.5", now);
        let (p, r) = l.next_pause(EndKind::RateLimited, ra, S0, 0.1);
        assert_eq!((p, r), (Duration::from_millis(90_500), Rule::RetryAfter));
        let ra = parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now);
        let (p, r) = Ladder::default().next_pause(EndKind::RateLimited, ra, S0, 0.1);
        assert_eq!((p, r), (Duration::from_secs(120), Rule::RetryAfter));
        // Sub-second values are still floored at 1 s by the ladder.
        let ra = parse_retry_after("0.2", SystemTime::now());
        let (p, _) = Ladder::default().next_pause(EndKind::RateLimited, ra, S0, 0.1);
        assert_eq!(p, Duration::from_secs(1));
    }

    #[test]
    fn mixed_429_and_403_share_strikes() {
        let mut l = Ladder::default();
        assert_eq!(l.next_pause(EndKind::RateLimited, None, S0, 0.0).0, minutes(5));
        assert_eq!(l.next_pause(EndKind::Forbidden, None, S0, 0.0).0, minutes(30));
        assert_eq!(l.next_pause(EndKind::RateLimited, None, S0, 0.0).0, minutes(20));
    }

    #[test]
    fn long_session_resets_ladder() {
        let mut l = Ladder::default();
        for _ in 0..3 {
            l.next_pause(EndKind::RateLimited, None, S0, 0.0);
        }
        // 9 min session: no reset, normal close gives jitter but keeps strikes.
        let (p, r) = l.next_pause(EndKind::ServerClosed, None, minutes(9), 0.0);
        assert_eq!((p, r), (Duration::from_secs(1), Rule::NormalJitter));
        assert_eq!(l.next_pause(EndKind::RateLimited, None, S0, 0.0).0, minutes(40));
        // 10 min session resets.
        l.next_pause(EndKind::ServerClosed, None, minutes(10), 0.0);
        assert_eq!(l.strikes, 0);
        assert_eq!(l.next_pause(EndKind::RateLimited, None, S0, 0.0).0, minutes(5));
    }

    #[test]
    fn normal_close_is_1_to_5_seconds() {
        let mut l = Ladder::default();
        for i in 0..100 {
            let r = i as f64 / 100.0;
            let (p, rule) = l.next_pause(EndKind::ServerClosed, None, minutes(1), r);
            assert_eq!(rule, Rule::NormalJitter);
            assert!(p >= Duration::from_secs(1) && p < Duration::from_secs(5), "{p:?}");
        }
        let (p, _) = l.next_pause(EndKind::Idle, None, minutes(1), 0.999);
        assert!(p < Duration::from_secs(5));
    }

    #[test]
    fn flapping_and_net_errors_use_transient_ladder() {
        let mut l = Ladder::default();
        let got: Vec<u64> = (0..9).map(|_| l.next_pause(EndKind::NetError, None, S0, 0.5).0.as_secs()).collect();
        assert_eq!(got, vec![5, 10, 20, 40, 80, 160, 300, 300, 300]);
        // A healthy session clears it.
        l.next_pause(EndKind::ServerClosed, None, minutes(2), 0.5);
        assert_eq!(l.transient, 0);
        // Close within 30 s of connecting is flapping, not a normal close.
        let (p, r) = l.next_pause(EndKind::ServerClosed, None, Duration::from_secs(3), 0.5);
        assert_eq!((p.as_secs(), r), (5, Rule::Transient));
        // Jitter bounds of the transient ladder.
        let mut l = Ladder::default();
        let (lo, _) = l.next_pause(EndKind::HttpError, None, S0, 0.0);
        let mut l = Ladder::default();
        let (hi, _) = l.next_pause(EndKind::HttpError, None, S0, 0.999);
        assert_eq!(lo, Duration::from_secs(4));
        assert!(hi < Duration::from_secs(6));
    }

    /// Task 012 item 3: block_idle escalates even after 30 s sessions; a
    /// healthy normal close clears it; a 10-minute session resets it.
    #[test]
    fn block_idle_uses_transient_ladder() {
        let mut l = Ladder::default();
        let s = Duration::from_secs(31);
        let got: Vec<(u64, Rule)> = (0..8)
            .map(|_| {
                let (p, r) = l.next_pause(EndKind::BlockIdle, None, s, 0.5);
                (p.as_secs(), r)
            })
            .collect();
        let secs: Vec<u64> = got.iter().map(|g| g.0).collect();
        assert_eq!(secs, vec![5, 10, 20, 40, 80, 160, 300, 300]);
        assert!(got.iter().all(|g| g.1 == Rule::Transient));
        assert_eq!(EndKind::BlockIdle.as_str(), "block_idle");
        // Blocks flowed for 10 min before the stall: start from the bottom.
        let (p, _) = l.next_pause(EndKind::BlockIdle, None, minutes(10), 0.5);
        assert_eq!(p.as_secs(), 5);
        // Healthy normal close clears the ladder.
        l.next_pause(EndKind::ServerClosed, None, minutes(1), 0.5);
        assert_eq!(l.transient, 0);
    }
}
