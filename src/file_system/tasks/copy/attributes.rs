use super::super::sys::{FileType, Stat, fstat, stat_mode};
use super::{CopyContext, Paths, Source};
use crate::file_system::path_info::compact;
use log::warn;
use nix::sys::time::TimeSpec;
use nix::unistd::{Gid, fchown};
use rustix::fs::XattrFlags;
use std::ffi::{CStr, CString, OsStr};
use std::fs;
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Applies the mode a copied entry ends with, through its handle.
///
/// A copy keeps the permission bits the umask left at creation, and never the
/// source's setuid, setgid or sticky bits, like `cp` without `-p`: a file
/// copied into a directory someone else can reach must not become a setuid
/// program of the user who copied it. A directory keeps the setgid bit it
/// inherited from its parent. The creation mode had owner access added so the entry
/// could be written, so the source's owner bits are put back.
///
/// A move keeps the full mode, like `mv`, except setuid and setgid when the
/// copy is not owned by the source's user and group: `mv` clears them when it
/// cannot carry the ownership over, or a program would run as whoever moved it.
/// It first gives the copy the source's group, as `mv` does, so the group bits
/// it keeps are granted to the group they were granted to before rather than
/// to whichever group the destination assigned. Best effort: a user can only
/// give a file a group they belong to, and the comparison below then clears
/// setgid for a group that did not carry over.
///
/// A move also gets the source's extended attributes, like `mv`. They include
/// the POSIX ACLs on Linux, without which the named users and groups an ACL
/// grants would be dropped and the mask, which is what the source's group bits
/// hold, granted to the owning group instead. Before the mode: an ACL sets the
/// mode's permission bits from its own entries, and the mode then sets the
/// mask from the group bits, which are the source's mask, so the order changes
/// nothing but closes the window in which the owning group has the mask's
/// access. Best effort, since a destination that cannot hold an attribute (no
/// ACLs on FAT, a namespace only root may write) is no reason to fail the move.
pub(super) fn apply_final_mode(
    context: &CopyContext<'_>,
    paths: &Paths,
    file: &File,
    source: &Source,
) {
    if context.is_move {
        let _ = fchown(file, None, Some(Gid::from_raw(source.gid)));
        for (name, value) in &source.attributes {
            if let Err(error) = rustix::fs::fsetxattr(file, name, value, XattrFlags::empty()) {
                warn!(
                    "Failed to copy the extended attribute {} to {}: {error}",
                    crate::visible_os(OsStr::from_bytes(name.to_bytes())),
                    compact(&paths.new)
                );
            }
        }
    }
    let Ok(created) = fstat(file) else {
        return;
    };
    let created_mode = stat_mode(&created) & 0o777;
    let mode = if context.is_move {
        let special = if created.st_uid == source.uid && created.st_gid == source.gid {
            0o7000
        } else {
            0o1000
        };
        source.mode & (0o777 | special)
    } else {
        // A directory created in a setgid directory inherits the setgid bit,
        // which `cp -R` keeps so what is later created in it takes the shared
        // group.
        let inherited = if FileType::of(&created) == FileType::Directory {
            stat_mode(&created) & 0o2000
        } else {
            0
        };
        (created_mode & 0o077) | (source.mode & created_mode & 0o700) | inherited
    };
    // A copy usually already has the mode it was created with.
    if stat_mode(&created) & 0o7777 == mode {
        return;
    }
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode)) {
        warn!("Failed to set the mode of {}: {error}", compact(&paths.new));
    }
}

/// Gives the open `target`, the copy at `path`, `source`'s access and
/// modification times. Best effort: a filesystem that cannot record them is
/// not a reason to fail the operation.
pub(super) fn apply_times(path: &Path, source: &Source, target: &File) {
    let Some(times) = &source.times else {
        return;
    };
    if let Err(error) = nix::sys::stat::futimens(target, &times.access, &times.modification) {
        warn!("Failed to set the times of {}: {error}", compact(path));
    }
}

/// How many times a size-then-read is retried when the value grew in between.
pub(super) const SIZE_ATTEMPTS: usize = 3;

/// A variable-length value read by asking `read` for its size (an empty
/// buffer) and then reading it. `ERANGE` means it grew in between, so it is
/// measured again, a bounded number of times. A value measured empty is
/// empty: an empty buffer would only measure it again.
pub(super) fn read_sized(
    read: impl Fn(&mut [u8]) -> rustix::io::Result<usize>,
) -> rustix::io::Result<Vec<u8>> {
    for _ in 0..SIZE_ATTEMPTS {
        let size = read(&mut [])?;
        if size == 0 {
            return Ok(Vec::new());
        }
        let mut buffer = vec![0; size];
        match read(&mut buffer) {
            Ok(len) => {
                buffer.truncate(len);
                return Ok(buffer);
            }
            Err(rustix::io::Errno::RANGE) => {}
            Err(error) => return Err(error),
        }
    }
    Err(rustix::io::Errno::RANGE)
}

/// The POSIX ACLs, which carry permissions, so they are still asked for by
/// name when the full list cannot be read; `true` marks the one only a
/// directory has.
#[cfg(target_os = "linux")]
pub(super) const ACL_ATTRIBUTES: [(&CStr, bool); 2] = [
    (c"system.posix_acl_access", false),
    (c"system.posix_acl_default", true),
];

#[cfg(not(target_os = "linux"))]
pub(super) const ACL_ATTRIBUTES: [(&CStr, bool); 0] = [];

/// The ACL attribute names an entry of this kind can have.
pub(super) fn acl_names(is_directory: bool) -> Vec<CString> {
    ACL_ATTRIBUTES
        .into_iter()
        .filter(|&(_, directory_only)| is_directory || !directory_only)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Every extended attribute of `file` with its value. Best effort: one that
/// cannot be read is not copied. When the list itself cannot be read, the ACLs
/// are still read by name where the platform has them. Listing reported as
/// unsupported is not warned about: it is what a filesystem without extended
/// attributes answers (FAT), where the reads by name then fail too, and also
/// what one that serves reading an attribute but not listing them does (some
/// FUSE filesystems). `path` names `file` in a warning.
pub(super) fn read_attributes(
    path: &Path,
    is_directory: bool,
    file: &File,
) -> Vec<(CString, Vec<u8>)> {
    let listed = read_sized(|buffer| rustix::fs::flistxattr(file, buffer));
    attribute_names(path, is_directory, listed)
        .into_iter()
        .filter_map(|name| {
            let value = read_attribute(file, &name)?;
            Some((name, value))
        })
        .collect()
}

/// The attribute names to read, from what listing those of `path` returned.
pub(super) fn attribute_names(
    path: &Path,
    is_directory: bool,
    listed: rustix::io::Result<Vec<u8>>,
) -> Vec<CString> {
    match listed {
        Ok(list) => list
            .split(|&byte| byte == 0)
            .filter_map(|name| CString::new(name).ok().filter(|name| !name.is_empty()))
            .collect(),
        Err(error) => {
            let names = acl_names(is_directory);
            if error != rustix::io::Errno::NOTSUP {
                let copied = if names.is_empty() {
                    "none are copied"
                } else {
                    "only its ACLs are copied"
                };
                warn!(
                    "Failed to list the extended attributes of {}: {error}; {copied}",
                    compact(path)
                );
            }
            names
        }
    }
}

/// The value of the extended attribute `name` on `file`, or `None` when it has
/// none or it cannot be read.
pub(super) fn read_attribute(file: &File, name: &CStr) -> Option<Vec<u8>> {
    read_sized(|buffer| rustix::fs::fgetxattr(file, name, buffer)).ok()
}

/// An entry's access and modification times, as `futimens` takes them.
pub(super) struct Times {
    pub(super) access: TimeSpec,
    pub(super) modification: TimeSpec,
}

/// `stat`'s access and modification times, in the form `futimens` takes.
/// `None` only where one does not fit it, which no real file reaches.
// The field types vary by target, and on some they already are the ones
// `TimeSpec` has.
#[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
pub(super) fn times_of(stat: &Stat) -> Option<Times> {
    Some(Times {
        access: TimeSpec::new(
            stat.st_atime.try_into().ok()?,
            stat.st_atime_nsec.try_into().ok()?,
        ),
        modification: TimeSpec::new(
            stat.st_mtime.try_into().ok()?,
            stat.st_mtime_nsec.try_into().ok()?,
        ),
    })
}
