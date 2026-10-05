//! Row types as `TabSeparatedWithNames` batches, validated line by line before anything is sent.
//!
//! ClickHouse turns an empty TSV field into `''`/`0` and `\N` into the column default without an
//! error (review 029, З1: an empty `block_number` became block 0). So every line a row writes is
//! checked here against its table description ([`TsvTable`]): the field count equals
//! `COLUMNS.len()`, an empty field is only allowed in a `MAY_BE_EMPTY` column, `\N` only in a
//! `NULLABLE` column, no field contains a backslash (nothing is escaped), and `block_number`
//! is a decimal number > 0.

use std::io::{self, Write};
use std::marker::PhantomData;

use anyhow::{bail, ensure, Context, Result};

/// One ClickHouse table and how a row of it is written.
pub trait TsvTable {
    /// `database.table`.
    const TABLE: &'static str;
    /// Column order of [`Self::write_tsv`] (= the header line and the INSERT column list).
    const COLUMNS: &'static [&'static str];
    /// Columns where `''` is a valid value (`String DEFAULT ''` meaning "absent").
    const MAY_BE_EMPTY: &'static [&'static str];
    /// Columns where `\N` (NULL) is a valid value.
    const NULLABLE: &'static [&'static str];

    /// One line in [`Self::COLUMNS`] order, ending with `\n`.
    ///
    /// # Errors
    /// I/O errors of `w`; a value the table cannot hold.
    fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()>;
}

/// TSV body of one INSERT: header + validated lines.
#[derive(Debug)]
pub struct Batch<T> {
    body: Vec<u8>,
    rows: usize,
    line: Vec<u8>,
    _t: PhantomData<T>,
}

impl<T: TsvTable> Default for Batch<T> {
    fn default() -> Self {
        let mut body = Vec::new();
        // Infallible: writing to a Vec.
        let _ = writeln!(body, "{}", T::COLUMNS.join("\t"));
        Self { body, rows: 0, line: Vec::new(), _t: PhantomData }
    }
}

impl<T: TsvTable> Batch<T> {
    /// Appends one row after checking its line.
    ///
    /// # Errors
    /// The row cannot be written or its line breaks a rule of the module docs.
    pub fn push(&mut self, row: &T) -> Result<()> {
        self.line.clear();
        row.write_tsv(&mut self.line).with_context(|| format!("{} row {}", T::TABLE, self.rows + 1))?;
        check_line::<T>(&self.line).with_context(|| format!("{} row {}", T::TABLE, self.rows + 1))?;
        self.body.extend_from_slice(&self.line);
        self.rows += 1;
        Ok(())
    }

    /// Number of rows (header not counted).
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// The body to send (header included).
    pub fn body(&self) -> &[u8] {
        &self.body
    }
}

/// Checks one written line (with its `\n`) against `T`.
///
/// # Errors
/// The first broken rule, naming the column.
pub(crate) fn check_line<T: TsvTable>(line: &[u8]) -> Result<()> {
    let Some(body) = line.strip_suffix(b"\n") else { bail!("line does not end with \\n") };
    let text = std::str::from_utf8(body).context("line is not UTF-8")?;
    ensure!(!text.contains(['\n', '\r']), "line break inside a line");
    let fields: Vec<&str> = text.split('\t').collect();
    ensure!(fields.len() == T::COLUMNS.len(), "{} fields for {} columns", fields.len(), T::COLUMNS.len());
    for (col, v) in T::COLUMNS.iter().zip(&fields) {
        match *v {
            "" => ensure!(T::MAY_BE_EMPTY.contains(col), "empty value in column {col}"),
            "\\N" => ensure!(T::NULLABLE.contains(col), "NULL in non-Nullable column {col}"),
            v => ensure!(!v.contains('\\'), "backslash in column {col}: {v:?}"),
        }
        if *col == "block_number" {
            let n: u64 = v.parse().with_context(|| format!("block_number {v:?}"))?;
            ensure!(n > 0, "block_number 0");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Row(&'static str);

    impl TsvTable for Row {
        const TABLE: &'static str = "t";
        const COLUMNS: &'static [&'static str] = &["block_number", "a", "b"];
        const MAY_BE_EMPTY: &'static [&'static str] = &["a"];
        const NULLABLE: &'static [&'static str] = &["b"];
        fn write_tsv(&self, w: &mut Vec<u8>) -> io::Result<()> {
            w.extend_from_slice(self.0.as_bytes());
            Ok(())
        }
    }

    #[test]
    fn lines_are_validated() {
        let mut b = Batch::<Row>::default();
        b.push(&Row("7\t\tx\n")).unwrap();
        b.push(&Row("7\ta\t\\N\n")).unwrap();
        for bad in [
            "\tx\ty\n",     // empty block_number (review 029, З1)
            "0\tx\ty\n",    // block 0
            "x\tx\ty\n",    // not a number
            "7\tx\t\n",     // empty in a non-empty column
            "7\t\\N\ty\n",  // NULL in a non-Nullable column
            "7\tx\n",       // too few fields
            "7\tx\ty\tz\n", // too many
            "7\tx\\t\ty\n", // escape sequence
            "7\tx\ty",      // no newline
        ] {
            assert!(b.push(&Row(bad)).is_err(), "{bad:?}");
        }
        assert_eq!(b.rows(), 2);
        assert_eq!(b.body(), b"block_number\ta\tb\n7\t\tx\n7\ta\t\\N\n");
    }
}
