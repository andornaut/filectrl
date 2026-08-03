//! Helpers shared by the unit tests across the crate.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// A unique temp directory that is removed when it is dropped.
///
/// Starting from a clean directory matters as much as uniqueness: several
/// tests assert that an operation refuses an existing destination, so a
/// directory left behind by an earlier run would make them fail until it was
/// deleted by hand. `Drop` cannot be the only guard, because a run killed by a
/// signal never runs it and the path is only unique per process id, which the
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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
