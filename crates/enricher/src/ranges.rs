//! Inclusive block ranges: reading the recorder's `gaps.tsv`, the enricher's
//! `filled.tsv`, and computing what is still missing.
//!
//! `gaps.tsv` (written by the recorder): `from \t to \t recv_unix_ns`.
//! `filled.tsv` (written here, in the blocks out-dir):
//!   `from \t to \t file_name \t filled_unix_s`, one line per final file,
//!   appended only after the file has been fsynced and renamed, so the state
//!   never runs ahead of the data.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Range {
    pub from: u64,
    pub to: u64,
}

impl Range {
    pub fn new(from: u64, to: u64) -> Result<Self> {
        if to < from {
            bail!("bad range {from}..={to}");
        }
        Ok(Self { from, to })
    }
    /// Number of blocks in the range (never zero).
    pub fn blocks(&self) -> u64 {
        self.to - self.from + 1
    }
}

/// Parse the first two tab-separated columns of each line as an inclusive
/// range. Blank lines and lines starting with `#` are skipped. A line that
/// cannot be parsed is an error, not silently ignored.
pub fn parse_ranges_tsv(text: &str, what: &str) -> Result<Vec<Range>> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut cols = line.split('\t');
        let parse = |c: Option<&str>| -> Result<u64> {
            c.context("missing column")?.trim().parse::<u64>().context("not a number")
        };
        let r = (|| Range::new(parse(cols.next())?, parse(cols.next())?))()
            .with_context(|| format!("{what} line {}: {line:?}", i + 1))?;
        out.push(r);
    }
    Ok(out)
}

pub fn read_ranges_file(path: &Path, what: &str) -> Result<Vec<Range>> {
    match fs::read_to_string(path) {
        Ok(t) => parse_ranges_tsv(&t, what).with_context(|| path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

/// Split off an unterminated last line (no trailing `\n`): the recorder may
/// be appending it right now (task 012 item 5). Returns the complete part
/// and the cut line, if any. Whitespace-only tails are not reported.
pub fn split_unterminated(text: &str) -> (&str, Option<&str>) {
    if text.is_empty() || text.ends_with('\n') {
        return (text, None);
    }
    let cut = text.rfind('\n').map_or(0, |i| i + 1);
    let tail = &text[cut..];
    (&text[..cut], (!tail.trim().is_empty()).then_some(tail))
}

/// The recorder's `gaps.tsv` for `--gaps`. Like [`read_ranges_file`], but
/// an unterminated last line is ignored and returned (the caller logs a
/// WARN) instead of failing the run: it is either being written right now
/// or will be completed later, and the next run picks it up. A broken line
/// that does end with `\n` is still an error. A missing file is an error.
pub fn read_gaps_file(path: &Path) -> Result<(Vec<Range>, Option<String>)> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let (complete, cut) = split_unterminated(&text);
    let ranges = parse_ranges_tsv(complete, "gaps").with_context(|| path.display().to_string())?;
    Ok((ranges, cut.map(str::to_owned)))
}

/// Sort and merge overlapping or adjacent ranges.
pub fn merge(mut v: Vec<Range>) -> Vec<Range> {
    v.sort();
    let mut out: Vec<Range> = Vec::with_capacity(v.len());
    for r in v {
        match out.last_mut() {
            Some(last) if r.from <= last.to.saturating_add(1) => last.to = last.to.max(r.to),
            _ => out.push(r),
        }
    }
    out
}

/// `want` minus `have`, merged.
pub fn subtract(want: Vec<Range>, have: Vec<Range>) -> Vec<Range> {
    let have = merge(have);
    let mut out = Vec::new();
    for w in merge(want) {
        let mut cur = w.from;
        for h in have.iter().filter(|h| h.to >= w.from && h.from <= w.to) {
            if h.from > cur {
                out.push(Range { from: cur, to: h.from - 1 });
            }
            cur = cur.max(h.to.saturating_add(1));
        }
        if cur <= w.to {
            out.push(Range { from: cur, to: w.to });
        }
    }
    out
}

/// Split ranges into pieces of at most `chunk` blocks.
pub fn chunk(v: &[Range], chunk: u64) -> Vec<Range> {
    assert!(chunk > 0);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn r(from: u64, to: u64) -> Range {
        Range { from, to }
    }

    #[test]
    fn parses_recorder_gaps_format() {
        let t = "100\t199\t1790000000000000000\n\n# comment\n300\t300\t1790000000000000001\n";
        assert_eq!(parse_ranges_tsv(t, "gaps").unwrap(), vec![r(100, 199), r(300, 300)]);
        assert!(parse_ranges_tsv("5\tx\n", "gaps").is_err());
        assert!(parse_ranges_tsv("9\t5\n", "gaps").is_err());
    }

    /// Task 012 item 5: the cases from the 011 review (`77200000`,
    /// `77200000\t`, a full range without recv_ns, a cut recv_ns) are all
    /// ignored while unterminated; once `\n` is there the line counts.
    #[test]
    fn unterminated_last_gaps_line_is_cut_off() {
        let head = "100\t199\t1790000000000000000\n";
        for tail in ["77200000", "77200000\t", "77200000\t77200099", "77200000\t77200099\t17908"] {
            let text = format!("{head}{tail}");
            let (complete, cut) = split_unterminated(&text);
            assert_eq!(complete, head);
            assert_eq!(cut, Some(tail));
            assert_eq!(parse_ranges_tsv(complete, "gaps").unwrap(), vec![r(100, 199)]);
        }
        assert_eq!(split_unterminated(head), (head, None));
        assert_eq!(split_unterminated(""), ("", None));
        assert_eq!(split_unterminated("5\t9"), ("", Some("5\t9")));
        // Trailing spaces without a newline are not a line.
        assert_eq!(split_unterminated("5\t9\t1\n  "), ("5\t9\t1\n", None));
        // A terminated broken line is still an error.
        assert!(parse_ranges_tsv(split_unterminated("5\tx\n").0, "gaps").is_err());
    }

    #[test]
    fn subtract_and_merge() {
        assert_eq!(merge(vec![r(5, 9), r(1, 3), r(4, 4), r(20, 30)]), vec![r(1, 9), r(20, 30)]);
        let want = vec![r(100, 199), r(300, 310)];
        let have = vec![r(90, 120), r(150, 160), r(300, 310)];
        assert_eq!(subtract(want, have), vec![r(121, 149), r(161, 199)]);
        assert_eq!(subtract(vec![r(1, 10)], vec![]), vec![r(1, 10)]);
        assert_eq!(subtract(vec![r(1, 10)], vec![r(0, 100)]), vec![]);
    }

    #[test]
    fn chunks() {
        assert_eq!(chunk(&[r(1, 10)], 4), vec![r(1, 4), r(5, 8), r(9, 10)]);
        assert_eq!(chunk(&[r(7, 7)], 1000), vec![r(7, 7)]);
    }
}
