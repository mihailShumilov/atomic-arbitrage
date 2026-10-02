//! Reading the recorder's `gaps.tsv` and the enricher's `filled.tsv`.
//!
//! Formats, range arithmetic and the parsers live in [`hood_core::ranges`];
//! this module only adds file IO, error context and the reading policy:
//! - both files: an unterminated last line is ignored and returned, the
//!   caller logs a WARN (task 012 item 5);
//! - `gaps.tsv`: a broken line that ends with `\n` is an error (a lost line
//!   would be a lost gap);
//! - `filled.tsv` (task 020 item 6, review 019 Р9): broken lines are skipped
//!   and returned for a WARN. Losing a line is harmless: its range is just
//!   downloaded again. A strict read made one damaged line stop every
//!   `--gaps` run until someone edited the file by hand.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use hood_core::ranges::{parse_ranges_file, parse_ranges_file_lenient, LineError, ParsedRanges, Range};

/// Ranges of a state file plus what was skipped.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RangesRead {
    /// Ranges of the complete lines that parsed, in file order.
    pub ranges: Vec<Range>,
    /// The ignored unterminated last line, if any.
    pub unterminated: Option<String>,
    /// Skipped broken lines (only ever non-empty for `filled.tsv`).
    pub broken: Vec<LineError>,
}

impl From<ParsedRanges<'_>> for RangesRead {
    fn from(p: ParsedRanges<'_>) -> Self {
        Self { ranges: p.ranges, unterminated: p.unterminated.map(str::to_owned), broken: p.broken }
    }
}

/// The recorder's `gaps.tsv` for `--gaps`, strict. A missing file is an error.
pub fn read_gaps_file(path: &Path) -> Result<RangesRead> {
    let text = fs::read_to_string(path).with_context(|| format!("read gaps file {}", path.display()))?;
    let p = parse_ranges_file(&text).with_context(|| format!("gaps file {}", path.display()))?;
    Ok(p.into())
}

/// `filled.tsv` of the blocks out-dir, lenient. A missing file means
/// nothing is filled yet.
pub fn read_filled_file(path: &Path) -> Result<RangesRead> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_ranges_file_lenient(&text).into()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(RangesRead::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    #[test]
    fn gaps_strict_filled_lenient() {
        let d = TestDir::new("ranges-policy");
        let r = |from, to| Range { from, to };
        let filled = d.join("filled.tsv");
        assert_eq!(read_filled_file(&filled).unwrap(), RangesRead::default());
        assert!(read_gaps_file(&d.join("gaps.tsv")).is_err(), "missing gaps file is an error");

        fs::write(&filled, "1\t2\tblocks-1-2.jsonl.zst\t1790000000\n3\t4\tblocks-3").unwrap();
        let got = read_filled_file(&filled).unwrap();
        assert_eq!(got.ranges, vec![r(1, 2)]);
        assert_eq!(got.unterminated.as_deref(), Some("3\t4\tblocks-3"));
        assert!(got.broken.is_empty());

        // A broken terminated line of filled.tsv is skipped, not an error.
        fs::write(&filled, "1\t2\tblocks-1-2.jsonl.zst\t1790000000\n3\n5\t6\tblocks-5-6.jsonl.zst\t1\n").unwrap();
        let got = read_filled_file(&filled).unwrap();
        assert_eq!(got.ranges, vec![r(1, 2), r(5, 6)]);
        assert_eq!(got.broken.len(), 1);
        assert_eq!((got.broken[0].line_no, got.broken[0].line.as_str()), (2, "3"));

        let gaps = d.join("gaps.tsv");
        fs::write(&gaps, "5\t9\t1\n77200000\t").unwrap();
        let got = read_gaps_file(&gaps).unwrap();
        assert_eq!(got.ranges, vec![r(5, 9)]);
        assert_eq!(got.unterminated.as_deref(), Some("77200000\t"));
        // ... but in gaps.tsv it is still an error, with file and line.
        fs::write(&gaps, "5\t9\t1\n5\tx\n").unwrap();
        let e = format!("{:#}", read_gaps_file(&gaps).unwrap_err());
        assert!(e.contains("gaps file") && e.contains("line 2"), "{e}");
    }
}
