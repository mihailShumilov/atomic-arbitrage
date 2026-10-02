//! Atomic output files and the per-out-dir lock.
//!
//! A file is written as `<final>.partial`, then zstd is finished, the file is
//! `fsync`ed, renamed to its final name and the directory is `fsync`ed
//! ([`hood_core::fsutil::rename_durable`]). A file
//! with a final name is therefore always complete. If the process dies before
//! `commit`, only a `*.partial` file can remain; it is removed on drop (normal
//! error / Ctrl-C) or by [`OutDirLock::acquire`] on the next start (kill -9).
//!
//! Every frame carries the zstd content checksum (task 026; files written
//! before it have none). A reader (`zstd -dc`, `zstd::Decoder`, Python
//! `zstandard`) then rejects a file whose decompressed bytes were changed by
//! a flipped bit on disk instead of returning them. Old files without the
//! checksum decode as before. The decompressed content is unchanged.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use hood_core::fsutil::rename_durable;

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
    /// Start writing `<final_path>.partial` at zstd `level`, with the
    /// content checksum on (see the module docs).
    pub fn create(final_path: &Path, level: i32) -> Result<Self> {
        let partial_path = partial_path(final_path);
        let f = File::create(&partial_path).with_context(|| format!("create {}", partial_path.display()))?;
        let mut enc = zstd::Encoder::new(BufWriter::new(f), level)?;
        enc.include_checksum(true).with_context(|| format!("zstd checksum for {}", partial_path.display()))?;
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
        // Rename + fsync of the directory; errors name the paths.
        rename_durable(&self.partial_path, &self.final_path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;
    use std::io::Read;

    fn scratch(name: &str) -> TestDir {
        TestDir::new(&format!("atomic-{name}"))
    }

    /// Frame header descriptor of the first frame: bit 2 = content checksum.
    fn has_checksum_flag(zst: &[u8]) -> bool {
        assert_eq!(zst[..4], [0x28, 0xb5, 0x2f, 0xfd], "zstd magic");
        zst[4] & 0b100 != 0
    }

    /// Bytes that zstd cannot compress, so they are stored as a raw block
    /// and a flipped byte in the middle of the file is a flipped byte of the
    /// content (no structural error to catch it). Deterministic (xorshift).
    fn incompressible(n: usize) -> Vec<u8> {
        let mut x: u64 = 0x2545_f491_4f6c_dd1d;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                (x >> 24) as u8
            })
            .collect()
    }

    fn flip_middle_byte(p: &Path) {
        let mut b = fs::read(p).unwrap();
        let i = b.len() / 2;
        b[i] ^= 0x01;
        fs::write(p, b).unwrap();
    }

    /// Task 026 item 1: files carry the checksum, and a flipped content byte
    /// is reported as an error instead of being decoded silently.
    #[test]
    fn corrupted_byte_is_detected_by_the_checksum() {
        let d = scratch("checksum");
        let data = incompressible(64 * 1024);
        let fin = d.join("blocks-1-2.jsonl.zst");
        let mut f = AtomicZstdFile::create(&fin, 3).unwrap();
        f.write_all(&data).unwrap();
        f.commit().unwrap();
        assert!(has_checksum_flag(&fs::read(&fin).unwrap()));
        assert_eq!(zstd::decode_all(File::open(&fin).unwrap()).unwrap(), data);

        flip_middle_byte(&fin);
        let e = zstd::decode_all(File::open(&fin).unwrap()).unwrap_err();
        assert!(e.to_string().to_lowercase().contains("checksum"), "{e}");

        // Control: the same corruption of a file without checksum (as written
        // before task 026) decodes "fine" to wrong bytes. That is what the
        // checksum is for, and it proves the flip hit the content.
        let old = d.join("old.jsonl.zst");
        fs::write(&old, zstd::encode_all(&data[..], 3).unwrap()).unwrap();
        assert!(!has_checksum_flag(&fs::read(&old).unwrap()));
        flip_middle_byte(&old);
        let got = zstd::decode_all(File::open(&old).unwrap()).unwrap();
        assert_eq!(got.len(), data.len());
        assert_ne!(got, data);
    }

    /// Files written before task 026 (no checksum) still read the same way.
    #[test]
    fn old_files_without_checksum_still_decode() {
        let d = scratch("old");
        let old = d.join("blocks-1-2.jsonl.zst");
        let mut enc = zstd::Encoder::new(File::create(&old).unwrap(), 3).unwrap();
        enc.include_checksum(false).unwrap();
        enc.write_all(b"{\"number\":1}\n").unwrap();
        enc.finish().unwrap();
        assert!(!has_checksum_flag(&fs::read(&old).unwrap()));
        let mut s = String::new();
        zstd::Decoder::new(File::open(&old).unwrap()).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "{\"number\":1}\n");
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
