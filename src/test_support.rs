//! Helpers shared by the unit tests across the crate.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::file_system::path_info::PathInfo;

/// Waits past any timestamp granularity a filesystem might round to, so what
/// happens next gets later times.
pub(crate) fn tick() {
    std::thread::sleep(std::time::Duration::from_millis(20));
}

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

/// Set on the process `run_alone` starts, so a test meant to run only there
/// does nothing when `--include-ignored` runs it among the others (`alone`).
const RUN_ALONE: &str = "FILECTRL_TEST_RUN_ALONE";

/// Runs the ignored test `name` (its full path) by itself, in a process of
/// its own started through `sh` after `prelude` (a `ulimit`, say, ending in
/// `;`), with `envs` set, and asserts that it ran and passed. For a test that
/// changes something process-wide, such as the umask, or needs a limit the
/// other tests must not share.
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

/// What `alone` prints in the process `run_alone` started, which tells it the
/// test ran rather than returned at once.
const RAN_ALONE: &str = "running alone";

/// Whether this is the process `run_alone` started. A test it runs returns at
/// once when it is not.
pub(crate) fn alone() -> bool {
    let alone = std::env::var_os(RUN_ALONE).is_some();
    if alone {
        eprintln!("{RAN_ALONE}");
    }
    alone
}

/// How many paths `TempDir::new` tries before giving up.
const TEMP_DIR_ATTEMPTS: usize = 100;

/// Creates the directory `path`, only there (never its parents, never through
/// a symlink at the path), checks that what holds the path is then a
/// directory this user owns (`AlreadyExists` for anything else), and makes it
/// owner-only through its handle, whatever the umask: fixtures inside are
/// then out of other users' reach.
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

/// A per-process counter guarantees a unique directory even when two are
/// created in the same nanosecond on parallel threads, so one fixture's Drop
/// never wipes another's directory.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// The temp directory path in `dir` for `label` and sequence number `seq`.
fn temp_path(dir: &Path, label: &str, seq: u64) -> PathBuf {
    dir.join(format!("filectrl_{label}_{}_{seq}", std::process::id()))
}

/// The first path in `dir` for `label`, numbered from `counter`, that nothing
/// holds: one something already holds (left by an earlier run, or put there
/// by another user, since the path is predictable) is passed over, never
/// removed.
fn free_path(counter: &AtomicU64, dir: &Path, label: &str) -> PathBuf {
    loop {
        let path = temp_path(dir, label, counter.fetch_add(1, Ordering::Relaxed));
        if std::fs::symlink_metadata(&path).is_err() {
            return path;
        }
    }
}

/// A unique temp directory, removed when dropped.
///
/// Starting clean matters as much as being unique: tests that assert an
/// operation refuses an existing destination would fail against a directory an
/// earlier run left behind. `Drop` cannot be the only guard, since a run killed
/// by a signal never runs it and the path is unique only per pid, which the
/// kernel reuses, so a path something already holds is passed over.
pub(crate) struct TempDir {
    path: PathBuf,
    /// The private directory a reserved path is inside, removed after it.
    _base: Option<Box<TempDir>>,
}

impl TempDir {
    /// Creates the directory, empty and this user's. `label` only makes the
    /// path easier to identify while debugging.
    ///
    /// The path is predictable, so another user could have put something at
    /// it first, a symlink to a directory of this user's say: that path is
    /// passed over for the next rather than written through
    /// (`create_owned_directory`).
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

    /// Reserves a unique path without creating it, for code under test that is
    /// expected to create the directory itself. The path is inside a private,
    /// owner-only temp directory (`new`), so nothing else can hold it or put
    /// anything at it before the code under test creates it.
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
        if std::fs::remove_dir_all(&self.path).is_err() {
            // A test that failed before giving back access it took away
            // leaves directories that cannot be listed or emptied.
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

/// Gives the owner read, write and search on the directory `name` in `dir`
/// and on every directory below it, so the tree can be removed. Everything
/// goes through directory handles and nothing follows a symlink: a name is
/// changed only while it holds a directory this user owns, the change itself
/// refuses a symlink swapped in at the name, and descending opens with
/// `O_NOFOLLOW`. The mode given is a fixed owner-only one, never one read
/// from the entry. The test directories include world-writable ones, where
/// another user could swap entries while this runs.
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

    /// A tree a failing test left without owner access is still removed.
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

    /// Giving access back to remove a locked tree never follows a symlink in
    /// it: a directory and a file outside it, both linked to from inside and
    /// both this user's, keep their modes, and so do the file and directory
    /// the directory holds.
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

    /// The access given back is owner-only whatever the directory allowed
    /// before: a mode read from the entry could have been chosen by whoever
    /// last changed it.
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

    /// A temp directory is owner-only, whatever the umask.
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

    /// What already holds the next temp directory paths (an earlier run's
    /// leftovers, or another user's tree moved there) is passed over, never
    /// removed or written into. The paths are inside a private temp
    /// directory, so the test itself removes nothing it did not make.
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

    /// A reserved path is inside a private, owner-only directory, and nothing
    /// holds it until the code under test creates it.
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

    /// A symlink put at a temp directory's path first is never created
    /// through: the path is refused, and nothing appears where it points.
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
