//! Helpers shared by the unit tests across the crate.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::file_system::path_info::PathInfo;

/// Writes `contents` to `path` as an executable script, from a child process:
/// a write fd open here could leak into a concurrent fork and cause ETXTBSY.
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

/// Set on the process `run_alone` starts.
const RUN_ALONE: &str = "FILECTRL_TEST_RUN_ALONE";

/// Runs the ignored test `name` alone in its own process, through `sh` after
/// `prelude` (ending in `;`) with `envs` set, and asserts that it ran and
/// passed. For tests that change process-wide state.
pub(crate) fn run_alone(name: &str, prelude: &str, envs: &[(&str, &OsStr)]) {
    let output = std::process::Command::new("sh")
        .args(["-c", &format!(r#"{prelude} exec "$0" "$@""#)])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            name,
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(RUN_ALONE, "1")
        .envs(envs.iter().copied())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}{stderr}");
    assert!(stdout.contains(" 1 passed;"), "{stdout}");
    assert!(stderr.contains(RAN_ALONE), "the test did not run: {stderr}");
}

const RAN_ALONE: &str = "running alone";

/// Whether this is the process `run_alone` started.
pub(crate) fn alone() -> bool {
    let alone = std::env::var_os(RUN_ALONE).is_some();
    if alone {
        eprintln!("{RAN_ALONE}");
    }
    alone
}

/// A unique, empty, owner-only temp directory, removed when dropped.
pub(crate) struct TempDir {
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl TempDir {
    /// Creates the directory. `label` only identifies it while debugging.
    pub(crate) fn new(label: &str) -> Self {
        let dir = tempfile::Builder::new()
            .prefix(&format!("filectrl_{label}_"))
            .permissions(std::os::unix::fs::PermissionsExt::from_mode(0o700))
            .tempdir()
            .unwrap();
        Self {
            path: dir.path().to_path_buf(),
            _dir: dir,
        }
    }

    /// Reserves a unique path inside a private directory without creating it.
    pub(crate) fn reserved(label: &str) -> Self {
        let Self { path, _dir: dir } = Self::new(label);
        Self {
            path: path.join("reserved"),
            _dir: dir,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn join(&self, name: impl AsRef<Path>) -> PathBuf {
        self.path.join(name)
    }

    pub(crate) fn directory(&self) -> PathInfo {
        PathInfo::try_from(self.path.as_path()).unwrap()
    }

    pub(crate) fn file(&self, name: &str, size: usize) -> PathInfo {
        let path = self.join(name);
        std::fs::write(&path, vec![b'x'; size]).unwrap();
        PathInfo::try_from(&path).unwrap()
    }

    pub(crate) fn subdirectory(&self, name: &str) -> PathInfo {
        let path = self.join(name);
        std::fs::create_dir_all(&path).unwrap();
        PathInfo::try_from(&path).unwrap()
    }

    /// Creates a one-byte file `name` inside `dir`, creating `dir` as needed.
    pub(crate) fn nested(&self, dir: &str, name: &str) -> PathInfo {
        let dir = self.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        PathInfo::try_from(&path).unwrap()
    }
}
