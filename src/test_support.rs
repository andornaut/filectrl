//! Helpers shared by the unit tests across the crate.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::file_system::path_info::PathInfo;

/// Waits past any filesystem timestamp granularity.
pub(crate) fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(20));
}

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

const TEMP_DIR_ATTEMPTS: usize = 100;

/// Creates the directory `path` owner-only, failing with `AlreadyExists` if
/// the path then holds anything but a directory this user owns.
fn create_owned_directory(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir(path)?;
    let dir = open_directory_at(nix::fcntl::AT_FDCWD, path)?;
    let stat = nix::sys::stat::fstat(&dir)?;
    if stat.st_uid != nix::unistd::geteuid().as_raw() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    nix::sys::stat::fchmod(&dir, nix::sys::stat::Mode::S_IRWXU)?;
    Ok(())
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_path(dir: &Path, label: &str, seq: u64) -> PathBuf {
    dir.join(format!("filectrl_{label}_{}_{seq}", std::process::id()))
}

/// The first path in `dir` for `label`, numbered from `counter`, that nothing
/// holds. A held path is passed over, never removed.
fn free_path(counter: &AtomicU64, dir: &Path, label: &str) -> PathBuf {
    loop {
        let path = temp_path(dir, label, counter.fetch_add(1, Ordering::Relaxed));
        if std::fs::symlink_metadata(&path).is_err() {
            return path;
        }
    }
}

/// A unique, empty temp directory, removed when dropped.
pub(crate) struct TempDir {
    path: PathBuf,
    _base: Option<Box<TempDir>>,
}

impl TempDir {
    /// Creates the directory. `label` only identifies it while debugging.
    pub(crate) fn new(label: &str) -> Self {
        for _ in 0..TEMP_DIR_ATTEMPTS {
            let path = free_path(&COUNTER, &std::env::temp_dir(), label);
            match create_owned_directory(&path) {
                Ok(()) => return Self { path, _base: None },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("{}: {error}", path.display()),
            }
        }
        panic!("no free path for a temp directory labelled {label}");
    }

    /// Reserves a unique path inside a private directory without creating it.
    pub(crate) fn reserved(label: &str) -> Self {
        let base = Self::new(label);
        Self {
            path: base.join("reserved"),
            _base: Some(Box::new(base)),
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

impl Drop for TempDir {
    fn drop(&mut self) {
        if std::fs::remove_dir_all(&self.path).is_err() {
            // A failed test can leave directories without owner access.
            if let Some(parent) = self.path.parent()
                && let Some(name) = self.path.file_name()
                && let Ok(parent) = open_directory_at(nix::fcntl::AT_FDCWD, parent)
            {
                grant_owner_access(&parent, name);
            }
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Opens the directory `name` in `dir` without following a symlink there.
fn open_directory_at(
    dir: impl std::os::fd::AsFd,
    name: &(impl nix::NixPath + ?Sized),
) -> nix::Result<std::os::fd::OwnedFd> {
    use nix::fcntl::{OFlag, openat};
    openat(
        dir,
        name,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
}

/// Sets `0o700` on the directory `name` in `dir` and every directory below it
/// that this user owns, so the tree can be removed. Never follows a symlink.
fn grant_owner_access(dir: impl std::os::fd::AsFd, name: &std::ffi::OsStr) {
    use nix::{
        fcntl::AtFlags,
        sys::stat::{FchmodatFlags, Mode, SFlag, fchmodat, fstatat},
    };
    let Ok(stat) = fstatat(&dir, name, AtFlags::AT_SYMLINK_NOFOLLOW) else {
        return;
    };
    let is_directory = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT == SFlag::S_IFDIR;
    if !is_directory || stat.st_uid != nix::unistd::geteuid().as_raw() {
        return;
    }
    let _ = fchmodat(&dir, name, Mode::S_IRWXU, FchmodatFlags::NoFollowSymlink);
    let Ok(child) = open_directory_at(&dir, name) else {
        return;
    };
    let Ok(clone) = child.try_clone() else {
        return;
    };
    let Ok(mut listed) = nix::dir::Dir::from_fd(clone) else {
        return;
    };
    let names: Vec<std::ffi::OsString> = listed
        .iter()
        .flatten()
        .map(|entry| {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(entry.file_name().to_bytes()).to_os_string()
        })
        .filter(|entry| entry != "." && entry != "..")
        .collect();
    for entry in names {
        grant_owner_access(&child, &entry);
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt};

    use super::TempDir;

    #[test]
    fn a_temp_dir_left_without_owner_access_is_removed() {
        let fx = TempDir::new("test_support_locked");
        let path = fx.path().to_path_buf();
        let locked = fx.join("locked");
        fs::create_dir_all(locked.join("inner")).unwrap();
        fs::write(locked.join("inner").join("file"), b"x").unwrap();
        fs::set_permissions(locked.join("inner"), fs::Permissions::from_mode(0o000)).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

        drop(fx);

        assert!(!path.exists(), "{} was left behind", path.display());
    }

    #[test]
    fn removing_a_locked_temp_dir_never_changes_what_its_links_point_at() {
        let outside = TempDir::new("test_support_outside");
        let (target_dir, target_file) = (outside.join("dir"), outside.join("file"));
        fs::create_dir(&target_dir).unwrap();
        fs::write(target_dir.join("inner"), b"x").unwrap();
        fs::create_dir(target_dir.join("below")).unwrap();
        fs::set_permissions(target_dir.join("below"), fs::Permissions::from_mode(0o500)).unwrap();
        fs::write(&target_file, b"x").unwrap();
        fs::set_permissions(target_dir.join("inner"), fs::Permissions::from_mode(0o400)).unwrap();
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o500)).unwrap();
        fs::set_permissions(&target_file, fs::Permissions::from_mode(0o400)).unwrap();
        let fx = TempDir::new("test_support_links");
        let locked = fx.join("locked");
        fs::create_dir(&locked).unwrap();
        std::os::unix::fs::symlink(&target_dir, locked.join("to_dir")).unwrap();
        std::os::unix::fs::symlink(&target_file, locked.join("to_file")).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();

        drop(fx);

        let mode = |path: &std::path::Path| {
            fs::symlink_metadata(path).unwrap().permissions().mode() & 0o7777
        };
        assert_eq!(0o500, mode(&target_dir));
        assert_eq!(0o400, mode(&target_dir.join("inner")));
        assert_eq!(0o500, mode(&target_dir.join("below")));
        assert_eq!(0o400, mode(&target_file));
        fs::set_permissions(target_dir.join("below"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&target_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn access_given_back_is_owner_only() {
        let fx = TempDir::new("test_support_grant_mode");
        let open = fx.join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o577)).unwrap();
        let parent = fs::File::open(fx.path()).unwrap();

        super::grant_owner_access(&parent, std::ffi::OsStr::new("open"));

        assert_eq!(
            0o700,
            fs::symlink_metadata(&open).unwrap().permissions().mode() & 0o7777
        );
    }

    #[test]
    fn a_temp_dir_is_owner_only() {
        let fx = TempDir::new("test_support_owner_only");

        assert_eq!(
            0o700,
            fs::symlink_metadata(fx.path())
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        );
    }

    #[test]
    fn a_path_already_held_is_passed_over_and_left_alone() {
        let base = TempDir::new("test_support_held_base");
        let label = "test_support_held";
        let counter = std::sync::atomic::AtomicU64::new(0);
        let held: Vec<_> = (0..3)
            .map(|seq| super::temp_path(base.path(), label, seq))
            .collect();
        for path in &held {
            fs::create_dir(path).unwrap();
            fs::write(path.join("keep"), b"keep").unwrap();
        }

        let free = super::free_path(&counter, base.path(), label);

        for path in &held {
            assert_eq!(b"keep".to_vec(), fs::read(path.join("keep")).unwrap());
        }
        assert_eq!(super::temp_path(base.path(), label, 3), free);
    }

    #[test]
    fn a_reserved_path_is_inside_a_private_directory() {
        let reserved = TempDir::reserved("test_support_reserved");
        let parent = reserved.path().parent().unwrap();

        assert!(fs::symlink_metadata(reserved.path()).is_err());
        assert_eq!(
            0o700,
            fs::symlink_metadata(parent).unwrap().permissions().mode() & 0o7777
        );
        let parent = parent.to_path_buf();
        drop(reserved);
        assert!(!parent.exists(), "{} was left behind", parent.display());
    }

    #[test]
    fn a_temp_dir_is_never_created_through_a_symlink_at_its_path() {
        let fx = TempDir::new("test_support_planted");
        let target = fx.join("target");
        fs::create_dir(&target).unwrap();
        let planted = fx.join("planted");
        std::os::unix::fs::symlink(&target, &planted).unwrap();

        let created = super::create_owned_directory(&planted);

        assert_eq!(
            std::io::ErrorKind::AlreadyExists,
            created.unwrap_err().kind()
        );
        assert!(
            fs::symlink_metadata(&planted)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(0, fs::read_dir(&target).unwrap().count());
    }
}
