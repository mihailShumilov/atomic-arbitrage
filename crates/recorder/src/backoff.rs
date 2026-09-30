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
    /// No frame for `idle_timeout`; we dropped the connection.
    Idle,
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
                    Some(ra) => (
                        ra.clamp(Duration::from_secs(1), RETRY_AFTER_MAX),
                        Rule::RetryAfter,
                    ),
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
                    Some(ra) if ra.min(RETRY_AFTER_MAX) > ladder => {
                        (ra.min(RETRY_AFTER_MAX), Rule::RetryAfter)
                    }
                    _ => (ladder, Rule::Ladder403),
                }
            }
            EndKind::ServerClosed | EndKind::Idle if session >= SHORT_SESSION => {
                self.transient = 0;
                // 1..5 s uniformly.
                let ms = 1000 + (rand01 * 4000.0) as u64;
                (Duration::from_millis(ms), Rule::NormalJitter)
            }
            EndKind::ServerClosed | EndKind::Idle | EndKind::HttpError | EndKind::NetError => {
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

/// Parse an HTTP `Retry-After` value: delta-seconds or an HTTP-date.
/// `now_unix` is used to turn a date into a delay.
pub fn parse_retry_after(v: &str, now_unix: i64) -> Option<Duration> {
    let v = v.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = chrono::DateTime::parse_from_rfc2822(v).ok()?.timestamp();
    Some(Duration::from_secs(
        when.saturating_sub(now_unix).max(0) as u64
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S0: Duration = Duration::ZERO;

    fn minutes(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    #[test]
    fn ladder_429_without_retry_after() {
        let mut l = Ladder::default();
        let got: Vec<u64> = (0..7)
            .map(|_| {
                l.next_pause(EndKind::RateLimited, None, S0, 0.5)
                    .0
                    .as_secs()
                    / 60
            })
            .collect();
        assert_eq!(got, vec![5, 10, 20, 40, 60, 60, 60]);
    }

    #[test]
    fn ladder_403_starts_at_15() {
        let mut l = Ladder::default();
        let got: Vec<u64> = (0..5)
            .map(|_| l.next_pause(EndKind::Forbidden, None, S0, 0.5).0.as_secs() / 60)
            .collect();
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
        let (p, _) = l.next_pause(
            EndKind::RateLimited,
            Some(Duration::from_secs(10_000_000)),
            S0,
            0.1,
        );
        assert_eq!(p, RETRY_AFTER_MAX);
    }

    #[test]
    fn mixed_429_and_403_share_strikes() {
        let mut l = Ladder::default();
        assert_eq!(
            l.next_pause(EndKind::RateLimited, None, S0, 0.0).0,
            minutes(5)
        );
        assert_eq!(
            l.next_pause(EndKind::Forbidden, None, S0, 0.0).0,
            minutes(30)
        );
        assert_eq!(
            l.next_pause(EndKind::RateLimited, None, S0, 0.0).0,
            minutes(20)
        );
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
        assert_eq!(
            l.next_pause(EndKind::RateLimited, None, S0, 0.0).0,
            minutes(40)
        );
        // 10 min session resets.
        l.next_pause(EndKind::ServerClosed, None, minutes(10), 0.0);
        assert_eq!(l.strikes, 0);
        assert_eq!(
            l.next_pause(EndKind::RateLimited, None, S0, 0.0).0,
            minutes(5)
        );
    }

    #[test]
    fn normal_close_is_1_to_5_seconds() {
        let mut l = Ladder::default();
        for i in 0..100 {
            let r = i as f64 / 100.0;
            let (p, rule) = l.next_pause(EndKind::ServerClosed, None, minutes(1), r);
            assert_eq!(rule, Rule::NormalJitter);
            assert!(
                p >= Duration::from_secs(1) && p < Duration::from_secs(5),
                "{p:?}"
            );
        }
        let (p, _) = l.next_pause(EndKind::Idle, None, minutes(1), 0.999);
        assert!(p < Duration::from_secs(5));
    }

    #[test]
    fn flapping_and_net_errors_use_transient_ladder() {
        let mut l = Ladder::default();
        let got: Vec<u64> = (0..9)
            .map(|_| l.next_pause(EndKind::NetError, None, S0, 0.5).0.as_secs())
            .collect();
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

    #[test]
    fn retry_after_parsing() {
        assert_eq!(
            parse_retry_after(" 120 ", 0),
            Some(Duration::from_secs(120))
        );
        // Sun, 06 Nov 1994 08:49:37 GMT = 784111777
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 - 30),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            parse_retry_after("Sun, 06 Nov 1994 08:49:37 GMT", 784_111_777 + 30),
            Some(Duration::ZERO)
        );
        assert_eq!(parse_retry_after("soon", 0), None);
    }
}
