//! HTTP helpers shared by the recorder (feed upgrade) and the enricher (RPC).
//!
//! Only parsing lives here. The cap on an honoured delay is a policy of each
//! caller (recorder: 6 h, enricher: `--max-retry-after` 600 s by default).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, NaiveDateTime};

/// Parse an HTTP `Retry-After` value into a delay measured from `now`.
///
/// Accepted forms (RFC 9110, section 10.2.3; the date forms of section 5.6.7):
/// - delta-seconds: `120`; decimal seconds (`1.5`) are accepted too, some
///   providers send them. Digits only: no sign, exponent, `inf` or `nan`.
///   Too many integer digits saturate to `u64::MAX` seconds (callers cap it);
///   fraction digits beyond nanoseconds are ignored;
/// - IMF-fixdate: `Sun, 06 Nov 1994 08:49:37 GMT`;
/// - obsolete RFC 850: `Sunday, 06-Nov-94 08:49:37 GMT`;
/// - obsolete asctime: `Sun Nov  6 08:49:37 1994`.
///
/// A date in the past (or equal to `now`) gives [`Duration::ZERO`].
/// Anything else is `None` and the caller falls back to its own backoff.
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Some(d) = parse_delta_seconds(v) {
        return Some(d);
    }
    let when = parse_http_date(v)?;
    Some(when.duration_since(now).unwrap_or(Duration::ZERO))
}

/// `1*DIGIT [ "." 1*DIGIT ]`, parsed exactly (no float rounding).
fn parse_delta_seconds(v: &str) -> Option<Duration> {
    let (int, frac) = match v.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (v, None),
    };
    let all_digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int) || frac.is_some_and(|f| !all_digits(f)) {
        return None;
    }
    // Only digits are left, so a parse error can only be an overflow.
    let secs = int.parse::<u64>().unwrap_or(u64::MAX);
    let nanos = frac.map_or(0, |f| {
        let mut n = 0u32;
        for i in 0..9 {
            n = n * 10 + f.as_bytes().get(i).map_or(0, |b| u32::from(b - b'0'));
        }
        n
    });
    Some(Duration::new(secs, nanos))
}

/// The three HTTP-date forms; all are UTC by definition.
fn parse_http_date(v: &str) -> Option<SystemTime> {
    let ts = if let Ok(dt) = DateTime::parse_from_rfc2822(v) {
        dt.timestamp()
    } else {
        let naive = NaiveDateTime::parse_from_str(v, "%A, %d-%b-%y %H:%M:%S GMT")
            .or_else(|_| NaiveDateTime::parse_from_str(v, "%a %b %e %H:%M:%S %Y"))
            .ok()?;
        naive.and_utc().timestamp()
    };
    // Before 1970: certainly in the past, the result is a zero delay.
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(ts).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sun, 06 Nov 1994 08:49:37 GMT (the RFC 9110 example).
    const RFC_EXAMPLE: u64 = 784_111_777;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn integer_and_decimal_seconds() {
        let now = at(0);
        assert_eq!(parse_retry_after("2", now), Some(Duration::from_secs(2)));
        assert_eq!(parse_retry_after(" 120 ", now), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("0", now), Some(Duration::ZERO));
        // Fractional seconds: the recorder ignored these before task 019.
        assert_eq!(parse_retry_after(" 0.5 ", now), Some(Duration::from_millis(500)));
        assert_eq!(parse_retry_after("1.5", now), Some(Duration::from_millis(1500)));
        assert_eq!(parse_retry_after("1.000000001", now), Some(Duration::new(1, 1)));
        assert_eq!(parse_retry_after("1.0000000019", now), Some(Duration::new(1, 1)));
        // Saturates instead of failing; the caller caps it.
        assert_eq!(parse_retry_after("99999999999999999999999", now), Some(Duration::from_secs(u64::MAX)));
    }

    #[test]
    fn rejects_non_delta_numbers() {
        let now = at(0);
        for v in ["", "  ", "-1", "+1", "1e3", "inf", "NaN", ".5", "5.", "1.2.3", "1 2", "soon", "0x10"] {
            assert_eq!(parse_retry_after(v, now), None, "{v:?}");
        }
    }

    #[test]
    fn imf_fixdate() {
        let v = "Sun, 06 Nov 1994 08:49:37 GMT";
        assert_eq!(parse_retry_after(v, at(RFC_EXAMPLE - 30)), Some(Duration::from_secs(30)));
        assert_eq!(parse_retry_after(v, at(RFC_EXAMPLE)), Some(Duration::ZERO));
        assert_eq!(parse_retry_after(v, at(RFC_EXAMPLE + 30)), Some(Duration::ZERO));
        // Sub-second `now` is not rounded to whole seconds.
        let now = at(RFC_EXAMPLE - 10) + Duration::from_millis(250);
        assert_eq!(parse_retry_after(v, now), Some(Duration::from_millis(9750)));
        // A date in 2026 (the enricher ignored dates before task 019).
        let v = "Fri, 02 Oct 2026 12:10:00 GMT";
        assert_eq!(parse_retry_after(v, at(1_790_942_400)), Some(Duration::from_secs(600)));
    }

    #[test]
    fn obsolete_date_forms() {
        let now = at(RFC_EXAMPLE - 60);
        assert_eq!(parse_retry_after("Sunday, 06-Nov-94 08:49:37 GMT", now), Some(Duration::from_secs(60)));
        assert_eq!(parse_retry_after("Sun Nov  6 08:49:37 1994", now), Some(Duration::from_secs(60)));
        assert_eq!(parse_retry_after("Sun, 31 Feb 1994 08:49:37 GMT", now), None);
    }
}
