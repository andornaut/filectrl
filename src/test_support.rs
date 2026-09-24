//! Helpers shared by the unit tests across the crate.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::file_system::path_info::PathInfo;

/// Writes `contents` to `path` as an executable script, from a child process.
///
/// Written here, the file would be open for writing in this process for a
/// moment, and a test on another thread forking then would hand that open file
/// to its child. Running the script fails with "Text file busy" for as long as
/// any process holds it open for writing, so the test that runs it would fail
/// at random.
pub(crate) fn write_executable(path: &Path, contents: &str) {
    let status = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            r#"printf '%s' "$1" > "$2" && chmod 755 "$2""#,
            "sh",
            contents,
        ])
        .arg(path)
        .status()
        .expect("the shell should run");
    assert!(status.success(), "failed to write {}", path.display());
}

/// A unique temp directory, removed when dropped.
///
/// Starting clean matters as much as being unique: tests that assert an
/// operation refuses an existing destination would fail against a directory an
/// earlier run left behind. `Drop` cannot be the only guard, since a run killed
/// by a signal never runs it and the path is unique only per pid, which the
/// kernel reuses.
pub(crate) struct TempDir {
    path: PathBuf,
}

impl TempDir {
    /// Creates the directory, empty. `label` only makes the path easier to
    /// identify while debugging.
    pub(crate) fn new(label: &str) -> Self {
        let temp_dir = Self::reserved(label);
        std::fs::create_dir_all(&temp_dir.path).unwrap();
        temp_dir
    }

    /// Reserves a unique path without creating it, for code under test that is
    /// expected to create the directory itself.
    pub(crate) fn reserved(label: &str) -> Self {
        // A per-process counter guarantees a unique directory even when two
        // are created in the same nanosecond on parallel threads, so one
        // fixture's Drop never wipes another's directory.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("filectrl_{label}_{}_{seq}", std::process::id()));
        // Clear anything a previous run left at this path. `create_dir_all`
        // would silently adopt a stale directory along with its contents, and
        // a reserved path is expected not to exist at all.
        let _ = std::fs::remove_dir_all(&path);
        Self { path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.path.join(name)
    }

    /// The directory itself, as a listing names it.
    pub(crate) fn directory(&self) -> PathInfo {
        PathInfo::try_from(self.path.as_path()).unwrap()
    }

    /// Creates `name` as a file of `size` bytes.
    pub(crate) fn file(&self, name: &str, size: usize) -> PathInfo {
        let path = self.join(name);
        std::fs::write(&path, vec![b'x'; size]).unwrap();
        PathInfo::try_from(&path).unwrap()
    }

    /// Creates `name` as a directory.
    pub(crate) fn subdirectory(&self, name: &str) -> PathInfo {
        let path = self.join(name);
        std::fs::create_dir_all(&path).unwrap();
        PathInfo::try_from(&path).unwrap()
    }

    /// Creates a one-byte file `name` inside `dir`, creating `dir` as needed,
    /// so a search rooted here renders it with a separator in its name and a
    /// displayed name that differs from its basename.
    pub(crate) fn nested(&self, dir: &str, name: &str) -> PathInfo {
        let dir = self.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        PathInfo::try_from(&path).unwrap()
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
