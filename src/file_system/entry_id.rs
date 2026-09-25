//! What an entry is, apart from what it is named: its device and inode, and
//! for a paste, the entry as it was seen (`Seen`).

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

/// The device and inode of an entry, to tell whether a name still holds the
/// entry it held, or whether two names hold one entry.
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

    /// The entry `path` names, without following a symlink there. `None` when
    /// it cannot be read.
    pub(super) fn of_path(path: &Path) -> Option<Self> {
        lstat(path).ok().map(|stat| Self::of_stat(&stat))
    }

    /// The entry `name` names in the open directory `dir`, without following a
    /// symlink there.
    pub(super) fn at(dir: impl AsFd, name: &CStr) -> std::io::Result<Self> {
        use nix::{fcntl::AtFlags, sys::stat::fstatat};
        Ok(Self::of_stat(&fstatat(
            dir,
            name,
            AtFlags::AT_SYMLINK_NOFOLLOW,
        )?))
    }
}

/// An entry as the paste saw it: which entry it is, its type, when it was
/// created, when it was last written, and how large it was.
///
/// A number a removal frees can be given to the next entry created at once
/// (ext4 and xfs reuse an inode number immediately), so the identity alone
/// could take a replacement for the entry seen. What tells them apart is when
/// each was created (`born`): a replacement is created after the queue looked
/// at the name, so it carries a later birth time, or where the filesystem
/// records none a later change time, wherever the filesystem's clock resolves
/// the interval between that look and the replacement's creation. Linux 6.13
/// and later give ext4, xfs, btrfs and tmpfs timestamps fine enough for any
/// such interval; elsewhere a timestamp moves in ticks of the kernel's coarse
/// clock (1 to 10 ms, or whole seconds on ext4 with 128-byte inodes), and a
/// replacement of the same size created within the same tick, given the same
/// inode number and modification time, is taken for the entry seen.
///
/// The birth time is preferred because it moves only when an entry is
/// created: another link to the same file being replaced, which changes its
/// change time, does not make it another entry. An entry written since it was
/// seen counts as another one too, by its modification time and size.
///
/// Attributes a network filesystem caches (NFS, CIFS, sshfs) can still show
/// an entry replaced from another host as the one seen; that is the same
/// check-then-replace window another program can race, and is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seen {
    id: EntryId,
    /// The file type bits of its mode.
    kind: u32,
    born: (i64, i64),
    /// `born` is a birth time the filesystem recorded, not a change time
    /// standing in for one.
    recorded: bool,
    modified: (i64, i64),
    size: u64,
}

impl Seen {
    /// Whether the entry seen is a directory.
    // `S_IFDIR` is already a `u32` on Linux.
    #[allow(clippy::useless_conversion)]
    pub(super) fn is_directory(&self) -> bool {
        self.kind == u32::from(libc::S_IFDIR)
    }

    /// The entry `path` names, without following a symlink there, or `None`
    /// when the name is free.
    pub(super) fn of_path(path: &Path) -> std::io::Result<Option<Self>> {
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes())?;
        Self::at(rustix::fs::CWD, &path)
    }

    /// The entry `name` names in the open directory `dir`, without following
    /// a symlink there, or `None` when the name is free.
    pub(super) fn at(dir: impl AsFd, name: &CStr) -> std::io::Result<Option<Self>> {
        Self::look(dir, name)
    }

    /// `at`, through `statx` on Linux, for the birth time `fstatat` does not
    /// give. A kernel without `statx`, or a sandbox that refuses it (EPERM,
    /// EACCES), gets `fstatat`, and one that has refused it for good (ENOSYS,
    /// EPERM) is not asked again.
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
        // No automount, as `fstatat` never triggers one: a look is not a use.
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

    /// Which entry it is.
    pub(super) fn id(&self) -> EntryId {
        self.id
    }

    /// When it was created, where the filesystem recorded that: never a change
    /// time standing in for one, which a write or a mode change moves.
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

/// The `birth` time the filesystem recorded, when it gave one (`has_birth`)
/// and it is not zero, which is none: macOS reports that on a filesystem
/// without one (NFS, most FUSE), and a Linux filesystem can report it for an
/// entry it never recorded one for. `Seen` compares the change time, which
/// only moves forward, where there is none.
fn recorded_birth(has_birth: bool, birth: (i64, i64)) -> Option<(i64, i64)> {
    (has_birth && birth != (0, 0)).then_some(birth)
}

/// The entry `path` names now, as the paste queue would see it.
#[cfg(test)]
pub(super) fn seen(path: &Path) -> Option<Seen> {
    Seen::of_path(path).expect("the name can be looked at")
}

/// Whether the filesystem holding `path` records when an entry was created,
/// which `Seen` then compares in place of the change time.
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

    /// Writing to an entry makes it another as far as a paste is concerned:
    /// the paste agreed to replace what it saw, not what it became. Written
    /// with as many bytes as before, on the same inode, so only its times can
    /// tell.
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

    /// An entry created at the name after it was seen is another, even when
    /// it has the same size and the filesystem gives it the freed inode
    /// number back, as ext4 does.
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

    /// An entry created in place of the one seen is another even when it has
    /// the same inode number, size and modification time: when it was
    /// created is what tells them apart. Needs a filesystem that gives a
    /// freed inode number back (ext4 and xfs do; tmpfs and APFS do not, and
    /// it is skipped there), and a process to itself, so no other test takes
    /// the number first (the test below).
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

    /// An entry cut short since it was seen, with its modification time put
    /// back, is not the one seen either: its size tells.
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

    /// Replacing another link to the same file changes the file's change
    /// time, not when it was created, so the entry seen under this link is
    /// still the one seen. Where the filesystem records no birth time, the
    /// change time stands in, and this cannot hold; the test says so.
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

    /// A mode change moves only the change time, which the birth time stands
    /// in for, so the entry is still the one seen where one is recorded.
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

    /// A birth time the filesystem did not report in the mask is not used, and
    /// the device is part of what an entry is, read from a real `statx` with
    /// the fields in question changed.
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

    /// A birth time counts only when one is reported and it is not zero.
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

    /// An entry created later is another, whatever else it shares with the
    /// one seen; so is one on another device with the same inode number.
    #[test]
    fn an_entry_is_told_apart_by_when_it_was_created_and_its_device() {
        let seen = entry(1, 10, (100, 0));

        assert_eq!(seen, entry(1, 10, (100, 0)));
        assert_ne!(seen, entry(1, 10, (100, 1)));
        assert_ne!(seen, entry(2, 10, (100, 0)));
    }
}
