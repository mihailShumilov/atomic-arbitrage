//! Temporary test directory that is removed on drop (task 026 item 4: tests
//! used to leave `enricher-*` directories in TMPDIR). Kept on a failing test
//! so the files can be inspected; the path is printed. Used by the unit
//! tests (`#[cfg(test)] mod testdir`, private) and shared with the
//! integration tests via `#[path]` in `tests/common/mod.rs`.

use std::ops::Deref;
use std::path::{Path, PathBuf};

pub struct TestDir(PathBuf);

impl TestDir {
    /// Fresh empty `$TMPDIR/enricher-<name>-<pid>`.
    pub fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("enricher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if std::thread::panicking() {
            eprintln!("test failed, keeping {}", self.0.display());
        } else {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}
