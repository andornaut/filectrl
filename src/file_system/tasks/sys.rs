//! The file system calls the tasks make, through nix, and the few helpers that
//! turn its raw `stat` fields into what the walks compare. Every call is
//! relative to an open directory and never follows a link unless it says so.

pub(super) use nix::{
    dir::Dir,
    errno::Errno,
    fcntl::{AT_FDCWD as CWD, AtFlags, OFlag, openat, readlinkat},
    sys::stat::{FileStat as Stat, Mode, fstat, fstatat, mkdirat},
    unistd::{UnlinkatFlags, symlinkat},
};
use nix::{dir::Type, libc};

/// What an entry is, from its `stat` mode or its directory entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FileType {
    RegularFile,
    Directory,
    Symlink,
    Fifo,
    Socket,
    CharacterDevice,
    BlockDevice,
    /// A directory entry the filesystem did not type, or a mode with no
    /// recognized type bits.
    Unknown,
}

impl FileType {
    /// The type the file type bits of `mode` hold.
    // The constants are u32 on Linux but u16 on macOS.
    #[allow(clippy::useless_conversion)]
    pub(super) fn from_mode(mode: u32) -> Self {
        match mode & u32::from(libc::S_IFMT) {
            m if m == u32::from(libc::S_IFREG) => Self::RegularFile,
            m if m == u32::from(libc::S_IFDIR) => Self::Directory,
            m if m == u32::from(libc::S_IFLNK) => Self::Symlink,
            m if m == u32::from(libc::S_IFIFO) => Self::Fifo,
            m if m == u32::from(libc::S_IFSOCK) => Self::Socket,
            m if m == u32::from(libc::S_IFCHR) => Self::CharacterDevice,
            m if m == u32::from(libc::S_IFBLK) => Self::BlockDevice,
            _ => Self::Unknown,
        }
    }

    pub(super) fn of(stat: &Stat) -> Self {
        Self::from_mode(stat_mode(stat))
    }

    /// The type a directory entry reports, `Unknown` where it reports none.
    pub(super) fn of_entry(file_type: Option<Type>) -> Self {
        match file_type {
            Some(Type::File) => Self::RegularFile,
            Some(Type::Directory) => Self::Directory,
            Some(Type::Symlink) => Self::Symlink,
            Some(Type::Fifo) => Self::Fifo,
            Some(Type::Socket) => Self::Socket,
            Some(Type::CharacterDevice) => Self::CharacterDevice,
            Some(Type::BlockDevice) => Self::BlockDevice,
            None => Self::Unknown,
        }
    }
}

/// `st_mode` as a u32: it is u32 on Linux but u16 on macOS.
#[allow(clippy::useless_conversion)]
pub(super) fn stat_mode(stat: &Stat) -> u32 {
    u32::from(stat.st_mode)
}

/// The permission bits of `mode`, setuid, setgid and sticky included, as a
/// `Mode`.
pub(super) fn mode_bits(mode: u32) -> Mode {
    // `mode_t` is u32 on Linux but u16 on macOS; the permission bits fit.
    #[allow(clippy::cast_possible_truncation)]
    let raw = (mode & 0o7777) as libc::mode_t;
    Mode::from_bits_truncate(raw)
}

/// Sets the mode of `name` in the open directory `dir` (the working
/// directory through `CWD`) without following a symlink at the name.
/// `EOPNOTSUPP` is returned as it is, never retried with a change that
/// follows: it is what Linux answers for a symlink at the name, as well as a
/// system that cannot change a mode without following (glibc before 2.32),
/// and a link swapped in again before a retry would have its target changed.
/// macOS sets the link's own mode instead, which leaves the target alone too.
pub(in crate::file_system) fn set_mode_at(
    dir: impl std::os::fd::AsFd,
    name: &(impl ?Sized + nix::NixPath),
    mode: u32,
) -> std::io::Result<()> {
    use nix::sys::stat::{FchmodatFlags, fchmodat};
    Ok(fchmodat(
        dir,
        name,
        mode_bits(mode),
        FchmodatFlags::NoFollowSymlink,
    )?)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case(0o100_644 => FileType::RegularFile ; "a regular file")]
    #[test_case(0o040_755 => FileType::Directory ; "a directory")]
    #[test_case(0o120_777 => FileType::Symlink ; "a symlink")]
    #[test_case(0o010_644 => FileType::Fifo ; "a fifo")]
    #[test_case(0o140_755 => FileType::Socket ; "a socket")]
    #[test_case(0o020_620 => FileType::CharacterDevice ; "a character device")]
    #[test_case(0o060_660 => FileType::BlockDevice ; "a block device")]
    #[test_case(0o000_644 => FileType::Unknown ; "no type bits")]
    fn a_mode_names_its_type(mode: u32) -> FileType {
        FileType::from_mode(mode)
    }

    #[test]
    fn the_permission_bits_keep_the_special_bits_and_drop_the_type() {
        assert_eq!(Mode::from_bits_truncate(0o4755), mode_bits(0o104_755));
    }
}
