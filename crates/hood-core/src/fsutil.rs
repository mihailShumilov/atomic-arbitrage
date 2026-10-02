//! Durable file operations shared by the recorder and the enricher.
//!
//! One policy for both binaries: every fsync error, including the fsync of
//! the parent directory after a create or rename, is returned to the caller.
//! A failed fsync means the data may not survive a crash; pretending
//! otherwise would let `last_seq.txt`, `gaps.tsv` or `filled.tsv` claim data
//! that is not on disk. Errors keep their [`io::ErrorKind`] and name the step
//! and the path. Only std; what to do on an error is the caller's decision.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn with_path(e: io::Error, step: &str, path: &Path) -> io::Error {
    io::Error::new(e.kind(), format!("{step} {}: {e}", path.display()))
}

/// Parent directory of `path`; `.` for a bare file name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// fsync a directory so that a create or rename inside it is durable.
/// An empty path means the current directory.
pub fn fsync_dir(dir: &Path) -> io::Result<()> {
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    File::open(dir).and_then(|d| d.sync_all()).map_err(|e| with_path(e, "fsync dir", dir))
}

/// Rename `from` to `to`, then fsync the directory of `to`. `from` must
/// already be fsynced by the caller.
pub fn rename_durable(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to).map_err(|e| with_path(e, &format!("rename {} ->", from.display()), to))?;
    fsync_dir(parent_dir(to))
}

/// Temporary name used by [`write_atomic`]: `.<name>.tmp` next to `path`
/// (the recorder's `last_seq.txt` used this name before task 019 too).
fn atomic_tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("state");
    parent_dir(path).join(format!(".{name}.tmp"))
}

/// Atomically replace `path` with `contents`: write a temporary file, fsync
/// it, rename it over `path`, fsync the directory. A reader sees either the
/// old or the new contents, never a mix.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let tmp = atomic_tmp_path(path);
    {
        let mut f = File::create(&tmp).map_err(|e| with_path(e, "create", &tmp))?;
        f.write_all(contents).map_err(|e| with_path(e, "write", &tmp))?;
        f.sync_all().map_err(|e| with_path(e, "fsync", &tmp))?;
    }
    rename_durable(&tmp, path)
}

/// Append `data` to `path` (created if missing) with one `write_all`, fsync
/// the file, then fsync its directory (the append may have created it).
/// `data` should end with `\n`: readers ignore an unterminated last line.
///
/// If the file is not empty and does not end with `\n` (a torn write of a
/// previous run: every writer of these state files is the only one, so it is
/// never a line in progress), a `\n` is written first, in the same
/// `write_all`. Otherwise the new line would be glued to the fragment
/// (finding F1 of the 019 data audit). The fragment becomes a terminated
/// line of its own; readers then see it as broken or as a valid row.
pub fn append_synced(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|e| with_path(e, "open for append", path))?;
    let needs_newline =
        !data.is_empty() && !ends_with_newline(&mut f).map_err(|e| with_path(e, "read end of", path))?;
    let buf;
    let out = if needs_newline {
        buf = [b"\n".as_slice(), data].concat();
        buf.as_slice()
    } else {
        data
    };
    f.write_all(out).map_err(|e| with_path(e, "append to", path))?;
    f.sync_all().map_err(|e| with_path(e, "fsync", path))?;
    fsync_dir(parent_dir(path))
}

/// True for an empty file or one whose last byte is `\n`. Moves the read
/// position only; in append mode writes still go to the end.
fn ends_with_newline(f: &mut File) -> io::Result<bool> {
    if f.metadata()?.len() == 0 {
        return Ok(true);
    }
    f.seek(SeekFrom::End(-1))?;
    let mut last = [0u8; 1];
    f.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

/// [`append_synced`] for one line; `\n` is added, line and newline go out
/// in a single write (no line without `\n` if the process dies in between).
pub fn append_line_synced(path: &Path, line: &str) -> io::Result<()> {
    append_synced(path, format!("{line}\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hood-core-fsutil-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_tmp() {
        let d = scratch("atomic");
        let p = d.join("last_seq.txt");
        write_atomic(&p, b"10").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "10");
        write_atomic(&p, b"12").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "12");
        assert_eq!(atomic_tmp_path(&p), d.join(".last_seq.txt.tmp"));
        assert!(!atomic_tmp_path(&p).exists());
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn append_creates_and_appends_bytes_exactly() {
        let d = scratch("append");
        let p = d.join("gaps.tsv");
        append_synced(&p, b"1\t2\t3\n").unwrap();
        append_line_synced(&p, "4\t5\t6").unwrap();
        append_synced(&p, b"").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"1\t2\t3\n4\t5\t6\n");
        fs::remove_dir_all(&d).ok();
    }

    /// Finding F1 of the 019 data audit: appending after a torn last line
    /// (no `\n`) must not glue the new line to it.
    #[test]
    fn append_after_unterminated_line_starts_a_new_line() {
        let d = scratch("torn");
        let p = d.join("gaps.tsv");
        fs::write(&p, "1\t2\t3\n77169135\t77169712\t1790837152631000000").unwrap();
        append_synced(&p, b"77169135\t77169712\t1790837152631000000\n").unwrap();
        assert_eq!(
            fs::read_to_string(&p).unwrap(),
            "1\t2\t3\n77169135\t77169712\t1790837152631000000\n77169135\t77169712\t1790837152631000000\n"
        );
        // A terminated file gets no extra newline; an empty append changes nothing.
        let before = fs::read(&p).unwrap();
        append_synced(&p, b"").unwrap();
        assert_eq!(fs::read(&p).unwrap(), before);
        append_line_synced(&p, "5\t6\t7").unwrap();
        assert!(fs::read_to_string(&p).unwrap().ends_with("1790837152631000000\n5\t6\t7\n"));
        // Torn fragment only, via append_line_synced.
        let q = d.join("filled.tsv");
        fs::write(&q, "3\t4\tblocks-3").unwrap();
        append_line_synced(&q, "5\t6\tblocks-5-6.jsonl.zst\t1").unwrap();
        assert_eq!(fs::read_to_string(&q).unwrap(), "3\t4\tblocks-3\n5\t6\tblocks-5-6.jsonl.zst\t1\n");
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn rename_durable_and_dir_fsync() {
        let d = scratch("rename");
        let a = d.join("x.partial");
        fs::write(&a, b"x").unwrap();
        rename_durable(&a, &d.join("x")).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read(d.join("x")).unwrap(), b"x");
        fsync_dir(&d).unwrap();
        fsync_dir(Path::new("")).unwrap();
        fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn errors_are_returned_with_step_and_path() {
        let d = scratch("errors");
        let missing = d.join("no-such-dir");
        let e = fsync_dir(&missing).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().starts_with("fsync dir "), "{e}");
        assert!(e.to_string().contains("no-such-dir"), "{e}");
        let e = append_synced(&missing.join("gaps.tsv"), b"1\n").unwrap_err();
        assert!(e.to_string().starts_with("open for append "), "{e}");
        let e = write_atomic(&missing.join("last_seq.txt"), b"1").unwrap_err();
        assert!(e.to_string().starts_with("create "), "{e}");
        fs::remove_dir_all(&d).ok();
    }
}
