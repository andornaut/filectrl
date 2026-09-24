//! What an entry is, apart from what it is named: its device and inode.

use std::{os::fd::AsFd, path::Path};

use nix::{
    libc,
    sys::stat::{FileStat, fstat, lstat},
};

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
}
