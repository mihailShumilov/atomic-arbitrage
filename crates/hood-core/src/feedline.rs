//! One line of the raw feed on disk: `recv_ns \t seq_first \t seq_last \t JSON`
//! (`references/data-model.md`, "Слои", item 1). Shared by the recorder's
//! start-up recovery (`recorder::rawline`) and the ClickHouse loader
//! (`feed_recv_ns`), so both split the columns the same way. Pure: no IO.

use std::str::FromStr;

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

impl RawLine<'_> {
    /// `seq_first = seq_last = 0`: a line without blocks (ping, confirmed
    /// sequence number, non-JSON text frame, ...).
    pub fn is_unsequenced(&self) -> bool {
        self.seq_first == 0 && self.seq_last == 0
    }
}

fn num<T: FromStr>(col: Option<&[u8]>) -> Option<T> {
    std::str::from_utf8(col?).ok()?.parse().ok()
}

/// Parse one line (without `\n`). None if one of the three number columns
/// is missing or not a number (a seq column that does not fit `u64` is
/// rejected, not truncated). The JSON column is split off at the third tab
/// only, so a tab inside it (possible before task 009) stays in it.
pub fn parse_raw_line(line: &[u8]) -> Option<RawLine<'_>> {
    let mut cols = line.splitn(4, |&b| b == b'\t');
    let recv_ns = num(cols.next())?;
    let seq_first = num(cols.next())?;
    let seq_last = num(cols.next())?;
    Some(RawLine { recv_ns, seq_first, seq_last, json: cols.next().unwrap_or_default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_columns_strictly() {
        let l = parse_raw_line(b"17\t5\t5\t{\"a\":\"x\ty\"}").unwrap();
        assert_eq!((l.recv_ns, l.seq_first, l.seq_last), (17, 5, 5));
        assert_eq!(l.json, b"{\"a\":\"x\ty\"}"); // the JSON column keeps any tab
        assert!(!l.is_unsequenced());
        let u = parse_raw_line(b"17\t0\t0").unwrap();
        assert_eq!(u.json, b"");
        assert!(u.is_unsequenced());
        assert_eq!(parse_raw_line(b"17\t5"), None);
        assert_eq!(parse_raw_line(b"x\t5\t5\t{}"), None);
        assert_eq!(parse_raw_line(b"17\t-1\t5\t{}"), None);
        // 2^64 does not fit: rejected (before 021: parsed as u128, cast to u64).
        assert_eq!(parse_raw_line(b"17\t18446744073709551616\t5\t{}"), None);
        assert_eq!(parse_raw_line(b""), None);
    }
}
