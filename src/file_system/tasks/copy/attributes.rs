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
/// A copy keeps the permission bits the umask left, without setuid, setgid or sticky, like `cp`
/// without `-p`; a directory keeps an inherited setgid bit. The owner bits added at creation are
/// put back to the source's.
///
/// A move keeps the full mode and extended attributes (POSIX ACLs included), like `mv`, clearing
/// setuid and setgid when the owner or group did not carry over. The source's group and the
/// attributes are applied first, best effort: the ACL is set before the mode so the owning group
/// never holds the mask's access.
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
        // A directory inherits setgid from a setgid parent; `cp -R` keeps it.
        let inherited = if FileType::of(&created) == FileType::Directory {
            stat_mode(&created) & 0o2000
        } else {
            0
        };
        (created_mode & 0o077) | (source.mode & created_mode & 0o700) | inherited
    };
    if stat_mode(&created) & 0o7777 == mode {
        return;
    }
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode)) {
        warn!("Failed to set the mode of {}: {error}", compact(&paths.new));
    }
}

/// Gives the open `target` `source`'s access and modification times. Best effort.
pub(super) fn apply_times(path: &Path, source: &Source, target: &File) {
    let Some(times) = &source.times else {
        return;
    };
    if let Err(error) = nix::sys::stat::futimens(target, &times.access, &times.modification) {
        warn!("Failed to set the times of {}: {error}", compact(path));
    }
}

pub(super) const SIZE_ATTEMPTS: usize = 3;

/// Reads a variable-length value by asking `read` for its size (an empty buffer), then reading it.
/// `ERANGE` means it grew in between, so it is measured again, up to `SIZE_ATTEMPTS` times.
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

/// The POSIX ACLs, asked for by name when the attribute list cannot be read; `true` marks the
/// directory-only one.
#[cfg(target_os = "linux")]
pub(super) const ACL_ATTRIBUTES: [(&CStr, bool); 2] = [
    (c"system.posix_acl_access", false),
    (c"system.posix_acl_default", true),
];

#[cfg(not(target_os = "linux"))]
pub(super) const ACL_ATTRIBUTES: [(&CStr, bool); 0] = [];

pub(super) fn acl_names(is_directory: bool) -> Vec<CString> {
    ACL_ATTRIBUTES
        .into_iter()
        .filter(|&(_, directory_only)| is_directory || !directory_only)
        .map(|(name, _)| name.to_owned())
        .collect()
}

/// Every extended attribute of `file` with its value. Best effort: an unreadable one is skipped.
/// When the list cannot be read the ACLs are read by name; an unsupported listing (FAT, some FUSE)
/// is not warned about.
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

/// The value of the extended attribute `name` on `file`, `None` when absent or unreadable.
pub(super) fn read_attribute(file: &File, name: &CStr) -> Option<Vec<u8>> {
    read_sized(|buffer| rustix::fs::fgetxattr(file, name, buffer)).ok()
}

pub(super) struct Times {
    pub(super) access: TimeSpec,
    pub(super) modification: TimeSpec,
}

/// `stat`'s access and modification times; `None` only where one does not fit `TimeSpec`.
// The field types vary by target, and on some they already are `TimeSpec`'s.
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
