//! Reading the recorder's `gaps.tsv` and the enricher's `filled.tsv`.
//!
//! Formats, range arithmetic and the reading policy live in
//! [`hood_core::ranges`]; this module only adds file IO and error context.
//! Policy (task 012 item 5, the same for both files since task 019): an
//! unterminated last line is ignored and returned so the caller logs a WARN,
//! a broken line that ends with `\n` is an error.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Context, Result};
use hood_core::ranges::{parse_ranges_file, Range};

/// Ranges of a state file plus the ignored unterminated last line, if any.
pub type RangesRead = (Vec<Range>, Option<String>);

fn parse_text(text: &str, path: &Path, what: &str) -> Result<RangesRead> {
    let p = parse_ranges_file(text).with_context(|| format!("{what} file {}", path.display()))?;
    Ok((p.ranges, p.unterminated.map(str::to_owned)))
}

/// The recorder's `gaps.tsv` for `--gaps`. A missing file is an error.
pub fn read_gaps_file(path: &Path) -> Result<RangesRead> {
    let text = fs::read_to_string(path).with_context(|| format!("read gaps file {}", path.display()))?;
    parse_text(&text, path, "gaps")
}

/// `filled.tsv` of the blocks out-dir. A missing file means nothing is
/// filled yet.
pub fn read_filled_file(path: &Path) -> Result<RangesRead> {
    match fs::read_to_string(path) {
        Ok(text) => parse_text(&text, path, "filled"),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok((Vec::new(), None)),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("enricher-ranges-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn files_follow_one_policy() {
        let d = scratch("policy");
        let r = |from, to| Range { from, to };
        let filled = d.join("filled.tsv");
        assert_eq!(read_filled_file(&filled).unwrap(), (vec![], None));
        assert!(read_gaps_file(&d.join("gaps.tsv")).is_err(), "missing gaps file is an error");

        fs::write(&filled, "1\t2\tblocks-1-2.jsonl.zst\t1790000000\n3\t4\tblocks-3").unwrap();
        assert_eq!(read_filled_file(&filled).unwrap(), (vec![r(1, 2)], Some("3\t4\tblocks-3".to_owned())));
        fs::write(&filled, "1\t2\tblocks-1-2.jsonl.zst\t1790000000\n3\n").unwrap();
        let e = read_filled_file(&filled).unwrap_err();
        assert!(format!("{e:#}").contains("filled file"), "{e:#}");
        assert!(format!("{e:#}").contains("line 2"), "{e:#}");

        let gaps = d.join("gaps.tsv");
        fs::write(&gaps, "5\t9\t1\n77200000\t").unwrap();
        assert_eq!(read_gaps_file(&gaps).unwrap(), (vec![r(5, 9)], Some("77200000\t".to_owned())));
        fs::write(&gaps, "5\tx\n").unwrap();
        assert!(read_gaps_file(&gaps).is_err());
        fs::remove_dir_all(&d).ok();
    }
}
