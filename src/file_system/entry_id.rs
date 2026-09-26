//! Entry identity apart from the name: device and inode, and for a paste, the
//! entry as it was seen (`Seen`).

use std::{
    ffi::{CStr, CString},
    os::fd::AsFd,
    path::Path,
};

use nix::{
    libc,
    sys::stat::{FileStat, fstat, lstat},
};

/// When `stat`'s entry last changed, with nanoseconds.
// The field types vary by target, and on some they already are these.
#[allow(clippy::useless_conversion)]
pub(super) fn changed(stat: &FileStat) -> (i64, i64) {
    (i64::from(stat.st_ctime), i64::from(stat.st_ctime_nsec))
}

/// The device and inode of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct EntryId {
    dev: libc::dev_t,
    ino: libc::ino_t,
}

impl EntryId {
    /// The entry an open handle refers to.
    pub(super) fn of(file: impl AsFd) -> std::io::Result<Self> {
        Ok(Self::of_stat(&fstat(file)?))
    }

    pub(super) fn of_stat(stat: &FileStat) -> Self {
        Self {
            dev: stat.st_dev,
            ino: stat.st_ino,
        }
    }

    /// The entry `path` names, without following a symlink. `None` when unreadable.
    pub(super) fn of_path(path: &Path) -> Option<Self> {
        lstat(path).ok().map(|stat| Self::of_stat(&stat))
    }

    /// The entry `name` names in the open directory `dir`, without following a symlink.
    pub(super) fn at(dir: impl AsFd, name: &CStr) -> std::io::Result<Self> {
        use nix::{fcntl::AtFlags, sys::stat::fstatat};
        Ok(Self::of_stat(&fstatat(
            dir,
            name,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?))
    }
}

/// An entry as the paste saw it: identity, type, birth time, modification time
/// and size.
///
/// ext4 and xfs reuse a freed inode number at once, so identity alone could take
/// a replacement for the entry seen; the replacement's later birth time (or,
/// where none is recorded, change time) tells them apart, to the resolution of
/// the filesystem's clock. The birth time is preferred because replacing
/// another link to the same file moves the change time but not the birth time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seen {
    id: EntryId,
    kind: u32,
    born: (i64, i64),
    /// `born` is a recorded birth time, not a change time standing in for one.
    recorded: bool,
    modified: (i64, i64),
    size: u64,
}

impl Seen {
    // `S_IFDIR` is already a `u32` on Linux.
    #[allow(clippy::useless_conversion)]
    pub(super) fn is_directory(&self) -> bool {
        self.kind == u32::from(libc::S_IFDIR)
    }

    /// The entry `path` names, without following a symlink, or `None` when free.
    pub(super) fn of_path(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes())?;
        Self::at(rustix::fs::CWD, &path)
    }

    /// The entry `name` names in `dir`, without following a symlink, or `None` when free.
    pub(super) fn at(dir: impl AsFd, name: &CStr) -> std::io::Result<Option<Self>> {
        Self::look(dir, name)
    }

    /// `at` through `statx` on Linux, for the birth time. Falls back to `fstatat` on
    /// ENOSYS, EPERM or EACCES, and stops asking after ENOSYS or EPERM.
    #[cfg(target_os = "linux")]
    fn look(dir: impl AsFd, name: &CStr) -> std::io::Result<Option<Self>> {
        use std::sync::atomic::{AtomicBool, Ordering};

        use rustix::{
            fs::{AtFlags, StatxFlags, statx},
            io::Errno,
        };
        static REFUSED: AtomicBool = AtomicBool::new(false);
        if REFUSED.load(Ordering::Relaxed) {
            return Self::by_stat(dir, name);
        }
        let mask = StatxFlags::TYPE
            | StatxFlags::INO
            | StatxFlags::MTIME
            | StatxFlags::CTIME
            | StatxFlags::SIZE
            | StatxFlags::BTIME;
        // No automount, like `fstatat`.
        let flags = AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT;
        match statx(&dir, name, flags, mask) {
            Ok(stat) => Ok(Some(Self::of_statx(&stat))),
            Err(Errno::NOENT) => Ok(None),
            Err(errno @ (Errno::NOSYS | Errno::PERM | Errno::ACCESS)) => {
                if errno != Errno::ACCESS {
                    REFUSED.store(true, Ordering::Relaxed);
                }
                Self::by_stat(dir, name)
            }
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn look(dir: impl AsFd, name: &CStr) -> std::io::Result<Option<Self>> {
        Self::by_stat(dir, name)
    }

    pub(super) fn id(&self) -> EntryId {
        self.id
    }

    /// The recorded birth time, never a change time standing in for one.
    pub(super) fn birth(&self) -> Option<(i64, i64)> {
        self.recorded.then_some(self.born)
    }

    fn by_stat(dir: impl AsFd, name: &CStr) -> std::io::Result<Option<Self>> {
        use nix::{errno::Errno, fcntl::AtFlags, sys::stat::fstatat};
        match fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => Ok(Some(Self::of_stat(&stat))),
            Err(Errno::ENOENT) => Ok(None),
            Err(errno) => Err(errno.into()),
        }
    }

    // The field types vary by target, and on some they already are these.
    #[allow(clippy::useless_conversion)]
    fn of_stat(stat: &FileStat) -> Self {
        #[cfg(target_os = "macos")]
        let (born, recorded) = match recorded_birth(
            true,
            (
                i64::from(stat.st_birthtime),
                i64::from(stat.st_birthtime_nsec),
            ),
        ) {
            Some(birth) => (birth, true),
            None => (changed(stat), false),
        };
        #[cfg(not(target_os = "macos"))]
        let (born, recorded) = (changed(stat), false);
        Self {
            id: EntryId::of_stat(stat),
            kind: u32::from(stat.st_mode) & u32::from(libc::S_IFMT),
            born,
            recorded,
            modified: (i64::from(stat.st_mtime), i64::from(stat.st_mtime_nsec)),
            size: u64::try_from(stat.st_size).unwrap_or(0),
        }
    }

    #[cfg(target_os = "linux")]
    // `S_IFMT` is already a `u32` on Linux.
    #[allow(clippy::useless_conversion)]
    fn of_statx(stat: &rustix::fs::Statx) -> Self {
        use rustix::fs::StatxFlags;
        let time = |t: rustix::fs::StatxTimestamp| (t.tv_sec, i64::from(t.tv_nsec));
        let birth = recorded_birth(
            StatxFlags::from_bits_retain(stat.stx_mask).contains(StatxFlags::BTIME),
            time(stat.stx_btime),
        );
        Self {
            id: EntryId {
                dev: rustix::fs::makedev(stat.stx_dev_major, stat.stx_dev_minor),
                ino: stat.stx_ino,
            },
            kind: u32::from(stat.stx_mode) & u32::from(libc::S_IFMT),
            born: birth.unwrap_or_else(|| time(stat.stx_ctime)),
            recorded: birth.is_some(),
            modified: time(stat.stx_mtime),
            size: stat.stx_size,
        }
    }
}

/// `birth` if the filesystem reported one and it is not zero (macOS reports
/// zero where there is none, and Linux can for an entry it never recorded).
fn recorded_birth(has_birth: bool, birth: (i64, i64)) -> Option<(i64, i64)> {
    (has_birth && birth != (0, 0)).then_some(birth)
}

#[cfg(test)]
pub(super) fn seen(path: &Path) -> Option<Seen> {
    Seen::of_path(path).expect("the name can be looked at")
}

/// Whether the filesystem holding `path` records birth times.
#[cfg(test)]
pub(super) fn records_birth_time(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{AtFlags, StatxFlags, statx};
        statx(
            rustix::fs::CWD,
            path,
            AtFlags::SYMLINK_NOFOLLOW,
            StatxFlags::BTIME,
        )
        .is_ok_and(|stat| {
            recorded_birth(
                StatxFlags::from_bits_retain(stat.stx_mask).contains(StatxFlags::BTIME),
                (stat.stx_btime.tv_sec, i64::from(stat.stx_btime.tv_nsec)),
            )
            .is_some()
        })
    }
    #[cfg(target_os = "macos")]
    {
        #[allow(clippy::useless_conversion)]
        lstat(path).is_ok_and(|stat| {
            recorded_birth(
                true,
                (
                    i64::from(stat.st_birthtime),
                    i64::from(stat.st_birthtime_nsec),
                ),
            )
            .is_some()
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TempDir, tick};

    fn seen(path: &Path) -> Seen {
        super::seen(path).expect("the entry exists")
    }

    /// The same size on the same inode, so only its times tell.
    #[test]
    fn an_entry_written_since_it_was_seen_is_not_the_one_seen() {
        let fx = TempDir::new("seen_written");
        let path = fx.join("entry");
        std::fs::write(&path, b"seen").unwrap();
        let before = seen(&path);
        assert_eq!(before, seen(&path));
        tick();

        std::fs::write(&path, b"SEEN").unwrap();

        let now = seen(&path);
        assert_eq!((before.id, before.size), (now.id, now.size));
        assert_ne!(before, now);
    }

    #[test]
    fn an_entry_created_in_place_of_the_one_seen_is_another() {
        let fx = TempDir::new("seen_recreated");
        let path = fx.join("entry");
        std::fs::write(&path, b"seen").unwrap();
        let before = seen(&path);
        tick();

        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"seen").unwrap();

        assert_ne!(before, seen(&path));
    }

    /// Needs a filesystem that reuses a freed inode number (ext4, xfs; skipped on
    /// tmpfs and APFS), in a process of its own so no other test takes the number.
    #[test]
    #[ignore = "run in a process of its own by the test below"]
    fn a_reused_inode_with_the_same_times_is_another_entry() {
        use std::os::unix::fs::MetadataExt;
        if !crate::test_support::alone() {
            return;
        }
        let fx = TempDir::new("seen_reused");
        let path = fx.join("entry");
        std::fs::write(&path, b"seen").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let before = seen(&path);
        tick();

        for _ in 0..100 {
            std::fs::remove_file(&path).unwrap();
            std::fs::write(&path, b"seen").unwrap();
            if std::fs::metadata(&path).unwrap().ino() == before.id.ino {
                std::fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .unwrap();
                let now = seen(&path);
                assert_eq!(
                    (before.id, before.modified, before.size),
                    (now.id, now.modified, now.size)
                );
                assert_ne!(before, now);
                return;
            }
        }
        eprintln!("skipped: the filesystem never gave the inode number back");
    }

    #[test]
    fn a_reused_inode_is_told_apart_in_a_process_of_its_own() {
        crate::test_support::run_alone(
            "file_system::entry_id::tests::a_reused_inode_with_the_same_times_is_another_entry",
            "",
            &[],
        );
    }

    #[test]
    fn an_entry_cut_short_since_it_was_seen_is_not_the_one_seen() {
        let fx = TempDir::new("seen_truncated");
        let path = fx.join("entry");
        std::fs::write(&path, b"seen").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let before = seen(&path);

        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_len(1).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();

        let now = seen(&path);
        assert_eq!((before.id, before.modified), (now.id, now.modified));
        assert_ne!(before, now);
    }

    /// Replacing another link moves the change time, not the birth time. Skipped
    /// where no birth time is recorded.
    #[test]
    fn another_link_replaced_leaves_the_entry_the_one_seen() {
        let fx = TempDir::new("seen_other_link");
        let (a, b) = (fx.join("a"), fx.join("b"));
        std::fs::write(&a, b"seen").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        let before = seen(&b);
        tick();

        std::fs::write(fx.join("new"), b"new").unwrap();
        std::fs::rename(fx.join("new"), &a).unwrap();

        if records_birth_time(&b) {
            assert_eq!(before, seen(&b));
        } else {
            eprintln!("skipped: the filesystem records no birth time");
        }
    }

    #[test]
    fn a_mode_change_leaves_the_entry_the_one_seen() {
        use std::os::unix::fs::PermissionsExt;
        let fx = TempDir::new("seen_chmod");
        let path = fx.join("entry");
        std::fs::write(&path, b"seen").unwrap();
        let before = seen(&path);
        tick();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        if records_birth_time(&path) {
            assert_eq!(before, seen(&path));
        } else {
            eprintln!("skipped: the filesystem records no birth time");
        }
    }

    #[test]
    fn a_free_name_is_seen_as_nothing() {
        let fx = TempDir::new("seen_free");

        assert_eq!(None, Seen::of_path(&fx.join("absent")).unwrap());
    }

    #[test]
    // `S_IFLNK` is already a `u32` on Linux.
    #[allow(clippy::useless_conversion)]
    fn a_symlink_is_seen_as_itself() {
        let fx = TempDir::new("seen_link");
        std::fs::write(fx.join("target"), b"target").unwrap();
        std::os::unix::fs::symlink("target", fx.join("link")).unwrap();

        let link = seen(&fx.join("link"));

        assert_ne!(seen(&fx.join("target")).id, link.id);
        assert_eq!(u32::from(libc::S_IFLNK), link.kind);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_statx_birth_time_counts_only_when_reported() {
        use rustix::fs::{AtFlags, StatxFlags, statx};
        let fx = TempDir::new("seen_statx");
        let path = fx.join("entry");
        std::fs::write(&path, b"seen").unwrap();
        let mut stat = statx(
            rustix::fs::CWD,
            &path,
            AtFlags::SYMLINK_NOFOLLOW,
            StatxFlags::BASIC_STATS | StatxFlags::BTIME,
        )
        .unwrap();
        stat.stx_btime.tv_sec = stat.stx_ctime.tv_sec + 1000;
        let changed = (stat.stx_ctime.tv_sec, i64::from(stat.stx_ctime.tv_nsec));
        let birth = (stat.stx_btime.tv_sec, i64::from(stat.stx_btime.tv_nsec));

        stat.stx_mask = (StatxFlags::BASIC_STATS | StatxFlags::BTIME).bits();
        assert_eq!(birth, Seen::of_statx(&stat).born);
        assert_eq!(Some(birth), Seen::of_statx(&stat).birth());
        stat.stx_mask = StatxFlags::BASIC_STATS.bits();
        let seen = Seen::of_statx(&stat);
        assert_eq!(changed, seen.born);
        assert_eq!(None, seen.birth());
        stat.stx_dev_major += 1;
        assert_ne!(seen, Seen::of_statx(&stat));
    }

    #[test]
    fn a_birth_time_is_recorded_only_when_reported_and_not_zero() {
        assert_eq!(Some((5, 6)), recorded_birth(true, (5, 6)));
        assert_eq!(None, recorded_birth(false, (5, 6)));
        assert_eq!(None, recorded_birth(true, (0, 0)));
        assert_eq!(Some((0, 1)), recorded_birth(true, (0, 1)));
    }

    // `S_IFREG` is already a `u32` on Linux.
    #[allow(clippy::useless_conversion)]
    fn entry(dev: u64, ino: u64, born: (i64, i64)) -> Seen {
        Seen {
            id: EntryId {
                dev: dev.try_into().unwrap(),
                ino: ino.try_into().unwrap(),
            },
            kind: u32::from(libc::S_IFREG),
            born,
            recorded: true,
            modified: (1, 2),
            size: 3,
        }
    }

    #[test]
    fn an_entry_is_told_apart_by_when_it_was_created_and_its_device() {
        let seen = entry(1, 10, (100, 0));

        assert_eq!(seen, entry(1, 10, (100, 0)));
        assert_ne!(seen, entry(1, 10, (100, 1)));
        assert_ne!(seen, entry(2, 10, (100, 0)));
    }
}
