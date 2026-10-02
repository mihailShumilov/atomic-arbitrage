//! Inclusive L2 block ranges and the two state files that list them.
//!
//! - `gaps.tsv` (recorder, next to the raw feed): `from \t to \t recv_ns`,
//!   `recv_ns` of the first line after the hole ([`GapRow`]).
//! - `filled.tsv` (enricher, in the blocks out-dir):
//!   `from \t to \t file_name \t filled_unix_s` ([`FilledRow`]).
//!
//! The formats are described in `references/data-model.md`; the `Display`
//! impls here are the only writers. Reading policy (task 012, item 5, one for
//! both files, see [`parse_ranges_file`]): an unterminated last line (no
//! `\n`, may be being appended right now) is cut off and handed back to the
//! caller to log a WARN; a broken line that does end with `\n` is an error.
//! Pure: no IO in this module.

use std::fmt;

/// Inclusive range of L2 block numbers, `from <= to`.
///
/// Fields are public for pattern matching and literals in tests; code that
/// builds a range from external data must use [`Range::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Range {
    /// First block, inclusive.
    pub from: u64,
    /// Last block, inclusive.
    pub to: u64,
}

/// A contiguous range of L2 blocks missing from the recorded feed.
pub type Gap = Range;

impl Range {
    /// Checked constructor.
    ///
    /// # Errors
    /// [`RangeError`] if `to < from`.
    pub fn new(from: u64, to: u64) -> Result<Self, RangeError> {
        if to < from {
            return Err(RangeError { from, to });
        }
        Ok(Self { from, to })
    }

    /// Number of blocks in the range (never zero for a valid range;
    /// saturates at `u64::MAX` for `0..=u64::MAX`).
    pub fn blocks(&self) -> u64 {
        (self.to - self.from).saturating_add(1)
    }
}

impl fmt::Display for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..={}", self.from, self.to)
    }
}

/// `to < from` in [`Range::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RangeError {
    /// Requested first block.
    pub from: u64,
    /// Requested last block.
    pub to: u64,
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad range {}..={}", self.from, self.to)
    }
}

impl std::error::Error for RangeError {}

/// Given the last recorded sequence number and the first one of a new
/// envelope (or the next one inside an envelope), the hole between them.
/// A duplicate or an older number is not a hole.
pub fn detect_gap(last_seen: Option<u64>, first_new: u64) -> Option<Gap> {
    let next = last_seen?.checked_add(1)?;
    (first_new > next).then(|| Range { from: next, to: first_new - 1 })
}

/// Sort and merge overlapping or adjacent ranges.
pub fn merge(mut v: Vec<Range>) -> Vec<Range> {
    v.sort_unstable();
    let mut out: Vec<Range> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.from <= last.to.saturating_add(1) => last.to = last.to.max(r.to),
            _ => out.push(r),
        }
    }
    out
}

/// `want` minus `have`, sorted and merged. Inputs need not be sorted or
/// disjoint; they are borrowed (task 025: callers used to clone them).
pub fn subtract(want: &[Range], have: &[Range]) -> Vec<Range> {
    let have = merge(have.to_vec());
    let mut out = Vec::new();
    for w in merge(want.to_vec()) {
        let mut cur = w.from;
        let mut done = false;
        for h in have.iter().filter(|h| h.to >= w.from && h.from <= w.to) {
            if h.from > cur {
                out.push(Range { from: cur, to: h.from - 1 });
            }
            match h.to.checked_add(1) {
                Some(n) => cur = cur.max(n),
                // `have` runs to u64::MAX: nothing of `w` is left after it.
                None => {
                    done = true;
                    break;
                }
            }
        }
        if !done && cur <= w.to {
            out.push(Range { from: cur, to: w.to });
        }
    }
    out
}

/// Split ranges into pieces of at most `chunk` blocks.
///
/// # Panics
/// If `chunk` is zero (callers validate their `--chunk` flag first).
pub fn chunk(v: &[Range], chunk: u64) -> Vec<Range> {
    assert!(chunk > 0, "chunk must be > 0");
    let mut out = Vec::new();
    for r in v {
        let mut s = r.from;
        while s <= r.to {
            let e = s.saturating_add(chunk - 1).min(r.to);
            out.push(Range { from: s, to: e });
            if e == u64::MAX {
                break;
            }
            s = e + 1;
        }
    }
    out
}

/// Split off an unterminated last line (no trailing `\n`). Returns the
/// complete part and the cut line, if any. A whitespace-only tail is not a
/// line and is not reported.
fn split_unterminated(text: &str) -> (&str, Option<&str>) {
    if text.is_empty() || text.ends_with('\n') {
        return (text, None);
    }
    let cut = text.rfind('\n').map_or(0, |i| i + 1);
    let tail = &text[cut..];
    (&text[..cut], (!tail.trim().is_empty()).then_some(tail))
}

/// What is wrong with a line of a ranges file (task 025, remark Р1 of the
/// 019 review). `Display` gives the texts used before 025 (they appear in
/// WARN lines and in the `detail` of the recorder's `gaps_line_skipped`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineFault {
    /// Fewer columns than needed; the name of the first missing one
    /// (`from` or `to`).
    MissingColumn(&'static str),
    /// The named column is not a decimal `u64`.
    NotANumber(&'static str),
    /// `to < from`.
    BadRange(RangeError),
}

impl fmt::Display for LineFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColumn(name) => write!(f, "missing column {name}"),
            Self::NotANumber(name) => write!(f, "column {name} is not a number"),
            Self::BadRange(e) => e.fmt(f),
        }
    }
}

/// A line of a ranges file that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    /// 1-based line number in the parsed text.
    pub line_no: usize,
    /// The line as it is in the file.
    pub line: String,
    /// What is wrong with it.
    pub reason: LineFault,
}

impl fmt::Display for LineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {} in {:?}", self.line_no, self.reason, self.line)
    }
}

impl std::error::Error for LineError {}

/// Parse one line: the first two tab-separated columns as an inclusive
/// range, further columns are not looked at. `Ok(None)` for a blank line or
/// a `#` comment.
fn parse_range_line(line: &str) -> Result<Option<Range>, LineFault> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let mut cols = line.split('\t');
    let mut num = |name: &'static str| -> Result<u64, LineFault> {
        let c = cols.next().ok_or(LineFault::MissingColumn(name))?;
        c.trim().parse::<u64>().map_err(|_| LineFault::NotANumber(name))
    };
    let (from, to) = (num("from")?, num("to")?);
    Range::new(from, to).map(Some).map_err(LineFault::BadRange)
}

/// Ranges read from the contents of `gaps.tsv` or `filled.tsv`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedRanges<'a> {
    /// Ranges of all complete lines that parsed, in file order.
    pub ranges: Vec<Range>,
    /// Unterminated last line that was ignored (the caller logs a WARN).
    pub unterminated: Option<&'a str>,
    /// Complete (`\n`-terminated) lines that did not parse. Always empty in
    /// the result of [`parse_ranges_file`].
    pub broken: Vec<LineError>,
}

/// Lenient read of `gaps.tsv` / `filled.tsv`: like [`parse_ranges_file`],
/// but broken terminated lines are collected in [`ParsedRanges::broken`]
/// instead of failing. Used where a lost line is harmless: the recorder's
/// start-up (`gaps.tsv`, must not crash-loop over its own state file) and the
/// enricher's `filled.tsv` (a lost row only means the range is re-downloaded).
pub fn parse_ranges_file_lenient(text: &str) -> ParsedRanges<'_> {
    let (complete, unterminated) = split_unterminated(text);
    let mut ranges = Vec::new();
    let mut broken = Vec::new();
    for (i, line) in complete.lines().enumerate() {
        match parse_range_line(line) {
            Ok(Some(r)) => ranges.push(r),
            Ok(None) => {}
            Err(reason) => broken.push(LineError { line_no: i + 1, line: line.to_owned(), reason }),
        }
    }
    ParsedRanges { ranges, unterminated, broken }
}

/// Strict reading policy (the enricher's `gaps.tsv`): an unterminated
/// last line is ignored and returned (it is either being appended right now
/// or is a torn write; the next run sees it once it has its `\n`), a broken
/// line that ends with `\n` is an error.
///
/// # Errors
/// The first broken `\n`-terminated line, as a [`LineError`].
pub fn parse_ranges_file(text: &str) -> Result<ParsedRanges<'_>, LineError> {
    let mut p = parse_ranges_file_lenient(text);
    if p.broken.is_empty() {
        Ok(p)
    } else {
        Err(p.broken.swap_remove(0))
    }
}

/// One `gaps.tsv` row: `from \t to \t recv_ns` (`Display` gives the line
/// without `\n`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapRow {
    /// Missing blocks.
    pub range: Range,
    /// Receive time (unix ns) of the first line after the hole.
    pub recv_ns: u128,
}

impl fmt::Display for GapRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\t{}\t{}", self.range.from, self.range.to, self.recv_ns)
    }
}

/// One `filled.tsv` row: `from \t to \t file_name \t filled_unix_s`
/// (`Display` gives the line without `\n`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilledRow<'a> {
    /// Blocks in the file.
    pub range: Range,
    /// File name inside the blocks out-dir.
    pub file_name: &'a str,
    /// When the file was committed, unix seconds.
    pub filled_unix_s: u64,
}

impl fmt::Display for FilledRow<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\t{}\t{}\t{}", self.range.from, self.range.to, self.file_name, self.filled_unix_s)
    }
}

/// Concatenated lines (each with `\n`) for one append to a state file.
pub fn to_lines<T: fmt::Display>(rows: &[T]) -> String {
    rows.iter().map(|r| format!("{r}\n")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(from: u64, to: u64) -> Range {
        Range { from, to }
    }

    #[test]
    fn range_new_and_blocks() {
        assert_eq!(Range::new(5, 9), Ok(r(5, 9)));
        assert_eq!(Range::new(7, 7).unwrap().blocks(), 1);
        assert_eq!(Range::new(9, 5), Err(RangeError { from: 9, to: 5 }));
        assert_eq!(r(0, u64::MAX).blocks(), u64::MAX);
        assert_eq!(r(5, 9).to_string(), "5..=9");
    }

    #[test]
    fn gap_detection() {
        assert_eq!(detect_gap(None, 10), None);
        assert_eq!(detect_gap(Some(9), 10), None);
        assert_eq!(detect_gap(Some(10), 10), None); // duplicate, not a gap
        assert_eq!(detect_gap(Some(11), 10), None); // older, not a gap
        assert_eq!(detect_gap(Some(5), 10), Some(r(6, 9)));
        assert_eq!(detect_gap(Some(u64::MAX), 3), None); // no overflow
        assert_eq!(detect_gap(Some(u64::MAX - 2), u64::MAX), Some(r(u64::MAX - 1, u64::MAX - 1)));
    }

    #[test]
    fn subtract_and_merge() {
        assert_eq!(merge(vec![r(5, 9), r(1, 3), r(4, 4), r(20, 30)]), vec![r(1, 9), r(20, 30)]);
        assert_eq!(merge(vec![r(1, u64::MAX), r(5, 6)]), vec![r(1, u64::MAX)]);
        let want = vec![r(100, 199), r(300, 310)];
        let have = vec![r(90, 120), r(150, 160), r(300, 310)];
        assert_eq!(subtract(&want, &have), vec![r(121, 149), r(161, 199)]);
        assert_eq!(subtract(&[r(1, 10)], &[]), vec![r(1, 10)]);
        assert_eq!(subtract(&[r(1, 10)], &[r(0, 100)]), vec![]);
        assert_eq!(subtract(&[], &[r(0, 100)]), vec![]);
        // The recorder's former `uncovered` cases (writer.rs, before task 019).
        let unc = |listed: Vec<Range>| subtract(&[r(10, 20)], &listed);
        assert_eq!(unc(vec![]), vec![r(10, 20)]);
        assert_eq!(unc(vec![r(10, 20)]), vec![]);
        assert_eq!(unc(vec![r(5, 30)]), vec![]);
        assert_eq!(unc(vec![r(12, 14)]), vec![r(10, 11), r(15, 20)]);
        assert_eq!(unc(vec![r(18, 25), r(1, 10), r(13, 13)]), vec![r(11, 12), r(14, 17)]);
        assert_eq!(unc(vec![r(21, 30), r(1, 9)]), vec![r(10, 20)]);
        assert_eq!(unc(vec![r(0, u64::MAX)]), vec![]);
        assert_eq!(
            subtract(&[r(u64::MAX - 5, u64::MAX)], &[r(u64::MAX - 3, u64::MAX)]),
            vec![r(u64::MAX - 5, u64::MAX - 4)]
        );
        // Unsorted, overlapping input on both sides.
        assert_eq!(
            subtract(&[r(50, 60), r(1, 30), r(20, 40)], &[r(35, 52), r(5, 5)]),
            vec![r(1, 4), r(6, 34), r(53, 60)]
        );
    }

    #[test]
    fn chunks() {
        assert_eq!(chunk(&[r(1, 10)], 4), vec![r(1, 4), r(5, 8), r(9, 10)]);
        assert_eq!(chunk(&[r(7, 7)], 1000), vec![r(7, 7)]);
        assert_eq!(chunk(&[r(u64::MAX - 1, u64::MAX)], u64::MAX), vec![r(u64::MAX - 1, u64::MAX)]);
    }

    #[test]
    fn parses_recorder_gaps_format() {
        let t = "100\t199\t1790000000000000000\n\n# comment\n300\t300\t1790000000000000001\n";
        assert_eq!(parse_ranges_file(t).unwrap().ranges, vec![r(100, 199), r(300, 300)]);
        let e = parse_ranges_file("1\t2\t3\n5\tx\n").unwrap_err();
        assert_eq!((e.line_no, e.line.as_str()), (2, "5\tx"));
        assert_eq!(e.reason, LineFault::NotANumber("to"));
        assert_eq!(e.to_string(), "line 2: column to is not a number in \"5\\tx\"");
        let e = parse_ranges_file("9\t5\n").unwrap_err();
        assert_eq!(e.reason, LineFault::BadRange(RangeError { from: 9, to: 5 }));
        assert_eq!(parse_ranges_file("9\n").unwrap_err().reason, LineFault::MissingColumn("to"));
        assert_eq!(parse_ranges_file("x\t5\n").unwrap_err().reason, LineFault::NotANumber("from"));
    }

    /// Task 025: the typed reason prints exactly the texts of the former
    /// `String` reason (WARN lines, `gaps_line_skipped` detail).
    #[test]
    fn line_fault_texts_are_unchanged() {
        assert_eq!(LineFault::MissingColumn("to").to_string(), "missing column to");
        assert_eq!(LineFault::NotANumber("from").to_string(), "column from is not a number");
        assert_eq!(LineFault::BadRange(RangeError { from: 9, to: 5 }).to_string(), "bad range 9..=5");
    }

    #[test]
    fn parses_enricher_filled_format() {
        let t = "713002\t713201\tblocks-713002-713201.jsonl.zst\t1790000000\n";
        assert_eq!(
            parse_ranges_file(t).unwrap(),
            ParsedRanges { ranges: vec![r(713002, 713201)], unterminated: None, broken: vec![] }
        );
    }

    /// Task 012 item 5: the cases from the 011 review (`77200000`,
    /// `77200000\t`, a full range without recv_ns, a cut recv_ns) are all
    /// ignored while unterminated; once `\n` is there the line counts.
    #[test]
    fn unterminated_last_line_is_cut_off_broken_terminated_line_is_an_error() {
        let head = "100\t199\t1790000000000000000\n";
        for tail in ["77200000", "77200000\t", "77200000\t77200099", "77200000\t77200099\t17908"] {
            let text = format!("{head}{tail}");
            let p = parse_ranges_file(&text).unwrap();
            assert_eq!(p.ranges, vec![r(100, 199)]);
            assert_eq!(p.unterminated, Some(tail));
        }
        assert_eq!(split_unterminated(head), (head, None));
        assert_eq!(split_unterminated(""), ("", None));
        assert_eq!(split_unterminated("5\t9"), ("", Some("5\t9")));
        // Trailing spaces without a newline are not a line.
        assert_eq!(split_unterminated("5\t9\t1\n  "), ("5\t9\t1\n", None));
        // A terminated broken line is an error, wherever it is.
        assert!(parse_ranges_file("5\tx\n").is_err());
        assert_eq!(parse_ranges_file(&format!("5\tx\n{head}")).unwrap_err().line_no, 1);
        assert_eq!(parse_ranges_file("").unwrap().ranges, vec![]);
    }

    /// Lenient twin for the recorder: same ranges and unterminated tail,
    /// broken terminated lines are collected instead of failing.
    #[test]
    fn lenient_collects_broken_lines() {
        let t = "51\t99\t2\nbroken\n# c\n\n102\t104\t4\n9\t5\n200\t2";
        let p = parse_ranges_file_lenient(t);
        assert_eq!(p.ranges, vec![r(51, 99), r(102, 104)]);
        assert_eq!(p.unterminated, Some("200\t2"));
        assert_eq!(p.broken.iter().map(|e| e.line_no).collect::<Vec<_>>(), vec![2, 6]);
        assert_eq!(parse_ranges_file(t).unwrap_err(), p.broken[0]);
        // Without broken lines both readers agree.
        let ok = "1\t2\t3\n4\t5";
        assert_eq!(parse_ranges_file(ok).unwrap(), parse_ranges_file_lenient(ok));
    }

    /// Formats are byte-for-byte those of the recorder and enricher before
    /// task 019 (`format!("{}\t{}\t{}")` / `format!("{}\t{}\t{name}\t{now}")`).
    #[test]
    fn row_formats_are_unchanged() {
        let g = GapRow { range: r(101, 104), recv_ns: 1_790_769_600_000_000_001 };
        assert_eq!(g.to_string(), "101\t104\t1790769600000000001");
        let f = FilledRow { range: r(1, 2), file_name: "blocks-1-2.jsonl.zst", filled_unix_s: 1_790_000_000 };
        assert_eq!(f.to_string(), "1\t2\tblocks-1-2.jsonl.zst\t1790000000");
        let g2 = GapRow { range: r(5, 5), recv_ns: 7 };
        assert_eq!(to_lines(&[g, g2]), "101\t104\t1790769600000000001\n5\t5\t7\n");
        assert_eq!(to_lines::<GapRow>(&[]), "");
        // Round trip through the reader.
        assert_eq!(parse_ranges_file(&to_lines(&[g, g2])).unwrap().ranges, vec![r(101, 104), r(5, 5)]);
    }
}
