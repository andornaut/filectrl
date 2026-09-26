use super::super::sys::{
    AtFlags, Errno, FileType, Mode, OFlag, Stat, fstat, mode_bits, openat, readlinkat, set_mode_at,
    stat_mode, symlinkat,
};
use super::attributes::{apply_final_mode, apply_times};
use super::replace::staging_changed;
use super::{At, CopyContext, Paths, Source, transfer_object};
use crate::file_system::conflicts::{raced_in_copy_refusal, raced_refusal, verb};
use crate::file_system::path_info::compact;
use log::warn;
use nix::fcntl::{FcntlArg, fcntl};
use nix::unistd::Gid;
use std::ffi::CStr;
use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsFd;
use std::path::Path;
use std::time::Instant;

/// Copies the non-directory entry `at` names. Returns `false` only when cancelled.
pub(super) fn copy_entry(
    context: &mut CopyContext<'_>,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> bool {
    // The type comes from `lstat`, so a symlink is recreated, never followed.
    match FileType::of(stat) {
        FileType::Symlink => {
            copy_symlink(context, at, paths);
            true
        }
        FileType::RegularFile => copy_file(context, at, paths),
        _ => {
            copy_special(context, at, paths, stat);
            true
        }
    }
}

/// Recreates the symlink `at` names with the same target, which is never followed.
pub(super) fn copy_symlink(context: &mut CopyContext<'_>, at: &At<'_>, paths: &Paths) {
    let target = match readlinkat(at.src, at.src_name) {
        Ok(read) => read,
        Err(error) => {
            context.errors.push(format!(
                "Failed to read symlink {}: {error}",
                compact(&paths.old)
            ));
            return;
        }
    };
    match symlinkat(target.as_os_str(), at.dst, at.dst_name) {
        Ok(()) => context.mark_written(at, paths),
        Err(Errno::EEXIST) => resolve_raced(context, paths),
        Err(error) => {
            let own = || format!("Failed to create symlink {}: {error}", compact(&paths.new));
            context
                .errors
                .push(context.written_failure(paths, own, &error));
        }
    }
}

/// Copies a regular file, sending debounced progress. The destination is never more readable than
/// the source while written (`create_file_at`) and gets its final mode through the handle however
/// the copy stops. Returns `false` only when cancelled.
pub(super) fn copy_file(context: &mut CopyContext<'_>, at: &At<'_>, paths: &Paths) -> bool {
    let is_move = context.is_move;
    let failed = |error: &dyn std::fmt::Display| {
        format!(
            "Failed to {} {}: {error}",
            verb(is_move),
            transfer_object(paths)
        )
    };
    // Opened before the destination is created, so an unreadable source leaves nothing behind.
    let opened = match context.source.take() {
        Some(opened) => Ok(opened),
        None => open_source_file(at.src, at.src_name),
    };
    let (mut old_file, stat) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            context.errors.push(failed(&error));
            return true;
        }
    };
    let source = &Source::read(context.is_move, &paths.old, &old_file, &stat);
    let mut new_file = match create_file_at(at.dst, at.dst_name, source.mode, context.is_move) {
        Ok(file) => {
            context.mark_written(at, paths);
            file
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            resolve_raced(context, paths);
            return true;
        }
        Err(error) => {
            context.errors.push(failed(&error));
            return true;
        }
    };

    let not_cancelled = loop {
        if context.active.is_cancelled() {
            // Like interrupted `cp`: the partial destination stays.
            break false;
        }

        match old_file.read(context.buffer) {
            Ok(0) => {
                // Before `new_file` is dropped: the last write moves the
                // modification time.
                if context.is_move {
                    apply_times(&paths.new, source, &new_file);
                }
                break true;
            }
            Ok(bytes) => match new_file.write_all(&context.buffer[..bytes]) {
                Ok(()) => {
                    context.active.increment(bytes as u64);
                    if context
                        .progress
                        .should_trigger(Instant::now(), bytes as u64)
                    {
                        context.active.send_progress();
                    }
                }
                Err(error) => {
                    let own = || format!("Failed to write {}: {error}", compact(&paths.new));
                    context
                        .errors
                        .push(context.written_failure(paths, own, &error));
                    break true;
                }
            },
            Err(error) => {
                context
                    .errors
                    .push(format!("Failed to read {}: {error}", compact(&paths.old)));
                break true;
            }
        }
    };
    apply_final_mode(context, paths, &new_file, source);
    not_cancelled
}

/// Recreates a FIFO, socket, or device node with the source's permission bits, like `cp -R`. No
/// bytes are read: a FIFO would block. A device node needs root, otherwise "not permitted" is
/// recorded.
pub(super) fn copy_special(context: &mut CopyContext<'_>, at: &At<'_>, paths: &Paths, stat: &Stat) {
    let file_type = FileType::of(stat);
    if !matches!(
        file_type,
        FileType::Fifo | FileType::Socket | FileType::BlockDevice | FileType::CharacterDevice
    ) {
        context.errors.push(format!(
            "Cannot {} {}: unsupported file type",
            verb(context.is_move),
            compact(&paths.old)
        ));
        return;
    }
    let path = context.node_path(paths);
    match make_node(at, &path, file_type, stat_mode(stat), stat.st_rdev) {
        Ok(()) => context.mark_written(at, paths),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            resolve_raced(context, paths);
            return;
        }
        Err(error) => {
            let own = || {
                format!(
                    "Failed to create special file {}: {error}",
                    compact(&paths.new)
                )
            };
            context
                .errors
                .push(context.written_failure(paths, own, &error));
            return;
        }
    }
    if context.is_move {
        // The source's group first, so the group bits restored below keep their meaning.
        // Best effort.
        let _ = nix::unistd::fchownat(
            at.dst,
            at.dst_name,
            None,
            Some(Gid::from_raw(stat.st_gid)),
            AtFlags::AT_SYMLINK_NOFOLLOW,
        );
        restore_node_mode(context, at, paths, stat_mode(stat));
    }
}

/// Gives a node a move created the source's permission bits, by name (a FIFO cannot be opened
/// without blocking) and without following a symlink (`set_mode_at`).
pub(super) fn restore_node_mode(
    context: &mut CopyContext<'_>,
    at: &At<'_>,
    paths: &Paths,
    source_mode: u32,
) {
    let Err(error) = set_mode_at(at.dst, at.dst_name, source_mode & 0o777) else {
        return;
    };
    if error.raw_os_error() == Some(nix::libc::EOPNOTSUPP) {
        warn!("Failed to set the mode of {}: {error}", compact(&paths.new));
    } else if context.staging.is_some() {
        context.errors.push(format!(
            "Failed to set the mode of the replacement for {}: {error}",
            compact(&paths.new)
        ));
    } else {
        context.errors.push(format!(
            "Failed to chmod {} to {:o}: {error}",
            compact(&paths.new),
            source_mode & 0o777
        ));
    }
}

/// Creates the node `at` names. `_path` is only used on macOS.
#[cfg(not(target_os = "macos"))]
pub(super) fn make_node(
    at: &At<'_>,
    _path: &Path,
    file_type: FileType,
    source_mode: u32,
    device: nix::libc::dev_t,
) -> std::io::Result<()> {
    let (kind, device) = node_kind(file_type, device);
    Ok(nix::sys::stat::mknodat(
        at.dst,
        at.dst_name,
        kind,
        mode_bits(source_mode & 0o777),
        device,
    )?)
}

/// Creates the node at `path`: macOS has no `mknodat`.
#[cfg(target_os = "macos")]
pub(super) fn make_node(
    _at: &At<'_>,
    path: &Path,
    file_type: FileType,
    source_mode: u32,
    device: nix::libc::dev_t,
) -> std::io::Result<()> {
    let (kind, device) = node_kind(file_type, device);
    Ok(nix::sys::stat::mknod(
        path,
        kind,
        mode_bits(source_mode & 0o777),
        device,
    )?)
}

/// The `mknod` node type for `file_type`, and the source's device numbers for a device node (else
/// zero).
pub(super) fn node_kind(
    file_type: FileType,
    device: nix::libc::dev_t,
) -> (nix::sys::stat::SFlag, nix::libc::dev_t) {
    use nix::sys::stat::SFlag;

    match file_type {
        FileType::BlockDevice => (SFlag::S_IFBLK, device),
        FileType::CharacterDevice => (SFlag::S_IFCHR, device),
        FileType::Socket => (SFlag::S_IFSOCK, 0),
        _ => (SFlag::S_IFIFO, 0),
    }
}

pub(super) fn type_bits(mode: u32) -> u32 {
    mode & 0o170_000
}

/// Opens a regular file to copy from. `O_NOFOLLOW` refuses a swapped-in symlink and `O_NONBLOCK` a
/// FIFO; anything not a regular file is refused before a byte is read.
pub(super) fn open_source_file(dir: impl AsFd, name: &CStr) -> std::io::Result<(File, Stat)> {
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC;
    let fd = openat(dir, name, flags, Mode::empty())?;
    let stat = fstat(&fd)?;
    if FileType::of(&stat) != FileType::RegularFile {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "it is no longer a regular file",
        ));
    }
    // Clears `O_NONBLOCK`, the only `F_SETFL` flag set.
    fcntl(&fd, FcntlArg::F_SETFL(OFlag::empty()))?;
    Ok((File::from(fd), stat))
}

/// Creates the destination of a file copy with `O_EXCL | O_NOFOLLOW`, so a name taken since
/// validation fails and a planted symlink is not written through. A copy gets the source's
/// permission bits (trimmed by the umask); a move gets only the owner bits until its full mode is
/// applied at the end.
pub(super) fn create_file_at(
    dir: impl AsFd,
    name: &CStr,
    source_mode: u32,
    is_move: bool,
) -> std::io::Result<File> {
    let creation = if is_move {
        (source_mode & 0o700) | 0o600
    } else {
        source_mode & 0o777
    };
    let flags =
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    Ok(File::from(openat(dir, name, flags, mode_bits(creation))?))
}

/// Settles a destination name taken since the queue saw it free. It is never replaced: a standing
/// "skip all" skips it (a move then keeps its source), otherwise it is recorded as an error.
pub(super) fn resolve_raced(context: &mut CopyContext<'_>, paths: &Paths) {
    // The staging directory was created empty, so a name taken in it is refused, never skipped
    // or landed.
    if paths.depth == 0 && context.staging.is_some() {
        context.errors.push(staging_changed(context.is_move, paths));
    } else if context.conflicts.skips_raced() {
        context.skipped += 1;
    } else if paths.depth == 0 {
        context
            .errors
            .push(raced_refusal(context.is_move, &paths.old, &paths.new));
    } else {
        context.errors.push(raced_in_copy_refusal(
            context.is_move,
            &paths.old,
            &paths.new,
        ));
    }
}
