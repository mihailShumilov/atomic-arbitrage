//! Atomic output files and the per-out-dir lock.
//!
//! A file is written as `<final>.partial`, then zstd is finished, the file is
//! `fsync`ed, renamed to its final name and the directory is `fsync`ed. A file
//! with a final name is therefore always complete. If the process dies before
//! `commit`, only a `*.partial` file can remain; it is removed on drop (normal
//! error / Ctrl-C) or by [`OutDirLock::acquire`] on the next start (kill -9).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

pub const PARTIAL_SUFFIX: &str = ".partial";
const LOCK_NAME: &str = ".enricher.lock";

pub fn partial_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_owned();
    s.push(PARTIAL_SUFFIX);
    PathBuf::from(s)
}

/// zstd-compressed file that only receives its final name on [`commit`](Self::commit).
pub struct AtomicZstdFile {
    final_path: PathBuf,
    partial_path: PathBuf,
    enc: Option<zstd::Encoder<'static, BufWriter<File>>>,
}

impl AtomicZstdFile {
    pub fn create(final_path: &Path, level: i32) -> Result<Self> {
        let partial_path = partial_path(final_path);
        let f = File::create(&partial_path).with_context(|| format!("create {}", partial_path.display()))?;
        let enc = zstd::Encoder::new(BufWriter::new(f), level)?;
        Ok(Self { final_path: final_path.to_owned(), partial_path, enc: Some(enc) })
    }

    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    pub fn partial_path(&self) -> &Path {
        &self.partial_path
    }

    /// Finish zstd, fsync the data, rename to the final name, fsync the directory.
    pub fn commit(mut self) -> Result<PathBuf> {
        let enc = self.enc.take().expect("encoder present until commit");
        let buf = enc.finish().context("finish zstd")?;
        let f = buf.into_inner().map_err(|e| e.into_error()).context("flush")?;
        f.sync_all().context("fsync data")?;
        drop(f);
        fs::rename(&self.partial_path, &self.final_path)
            .with_context(|| format!("rename {} -> {}", self.partial_path.display(), self.final_path.display()))?;
        if let Some(dir) = self.final_path.parent() {
            fsync_dir(dir)?;
        }
        Ok(self.final_path.clone())
    }
}

impl Write for AtomicZstdFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.enc.as_mut().expect("encoder present until commit").write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.enc.as_mut().expect("encoder present until commit").flush()
    }
}

impl Drop for AtomicZstdFile {
    fn drop(&mut self) {
        // Not committed: the data is incomplete, never give it a final name.
        if self.enc.take().is_some() {
            let _ = fs::remove_file(&self.partial_path);
        }
    }
}

pub fn fsync_dir(dir: &Path) -> Result<()> {
    let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
    File::open(dir).and_then(|d| d.sync_all()).with_context(|| format!("fsync dir {}", dir.display()))
}

/// Exclusive lock on an output directory. Held for the whole run; the OS
/// releases it when the process exits, including kill -9, so there are no
/// stale locks. Because only the lock holder writes into the directory, any
/// `*.partial` found after acquiring it is a leftover of a dead run.
pub struct OutDirLock {
    _file: File,
    pub removed_partials: Vec<PathBuf>,
}

impl OutDirLock {
    pub fn acquire(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join(LOCK_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                bail!("another enricher is already writing into {} (lock {})", dir.display(), path.display())
            }
            Err(fs::TryLockError::Error(e)) => return Err(e).context(format!("lock {}", path.display())),
        }
        let removed_partials = remove_partials(dir)?;
        Ok(Self { _file: file, removed_partials })
    }
}

fn remove_partials(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_file() && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(PARTIAL_SUFFIX)) {
            fs::remove_file(&p).with_context(|| format!("remove stale {}", p.display()))?;
            removed.push(p);
        }
    }
    removed.sort();
    Ok(removed)
}

/// Append one line to a small state file and fsync it.
pub fn append_line_synced(path: &Path, line: &str) -> Result<()> {
    let mut f =
        OpenOptions::new().create(true).append(true).open(path).with_context(|| format!("open {}", path.display()))?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("enricher-atomic-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn interrupted_write_never_has_final_name() {
        let d = scratch("interrupt");
        let fin = d.join("blocks-1-10.jsonl.zst");
        {
            let mut f = AtomicZstdFile::create(&fin, 3).unwrap();
            f.write_all(b"{\"number\":1}\n").unwrap();
            assert!(f.partial_path().exists());
            assert!(!fin.exists(), "final name must not exist while writing");
            // Dropped without commit == error / Ctrl-C mid-range.
        }
        assert!(!fin.exists());
        assert!(!partial_path(&fin).exists(), "partial removed on drop");
    }

    #[test]
    fn commit_renames_and_is_readable() {
        let d = scratch("commit");
        let fin = d.join("blocks-1-2.jsonl.zst");
        let mut f = AtomicZstdFile::create(&fin, 3).unwrap();
        f.write_all(b"a\nb\n").unwrap();
        f.commit().unwrap();
        assert!(fin.exists());
        assert!(!partial_path(&fin).exists());
        let mut s = String::new();
        zstd::Decoder::new(File::open(&fin).unwrap()).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "a\nb\n");
    }

    #[test]
    fn lock_removes_leftover_partials_and_is_exclusive() {
        let d = scratch("lock");
        // Simulates kill -9: partial left behind, no final file.
        let leftover = d.join("blocks-5-6.jsonl.zst.partial");
        fs::write(&leftover, b"torn").unwrap();
        let keep = d.join("blocks-1-2.jsonl.zst");
        fs::write(&keep, b"x").unwrap();
        let lock = OutDirLock::acquire(&d).unwrap();
        assert_eq!(lock.removed_partials, vec![leftover.clone()]);
        assert!(!leftover.exists());
        assert!(keep.exists());
        let second = OutDirLock::acquire(&d);
        assert!(second.is_err(), "second lock on the same dir must fail");
        drop(lock);
        OutDirLock::acquire(&d).unwrap();
    }
}
