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

/// Copies the non-directory entry `at` names, of the type `stat` gives it: a
/// symlink, a regular file, or a special file. Returns `false` only when
/// cancelled.
pub(super) fn copy_entry(
    context: &mut CopyContext<'_>,
    at: &At<'_>,
    paths: &Paths,
    stat: &Stat,
) -> bool {
    // The type comes from an `lstat`, so a symlink (even one pointing at a
    // directory) is recreated as a link rather than followed.
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

/// Recreates the symlink `at` names, pointing at the same (possibly relative,
/// possibly dangling) target. The target is never followed, so no bytes are
/// transferred and no permissions are applied.
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

/// Copies a file chunk-by-chunk, sending debounced progress updates through
/// the context's task. The destination is never more readable than the source
/// while it is written (see `create_file_at`), and gets its final mode through
/// the handle once the copy stops, however it stops. Failures are recorded in
/// the context's errors; returns `false` only when cancelled.
///
/// The mode and owner applied are those of the file opened.
pub(super) fn copy_file(context: &mut CopyContext<'_>, at: &At<'_>, paths: &Paths) -> bool {
    let is_move = context.is_move;
    let failed = |error: &dyn std::fmt::Display| {
        format!(
            "Failed to {} {}: {error}",
            verb(is_move),
            transfer_object(paths)
        )
    };
    // Opened before the destination is created, so a source that cannot be
    // read leaves nothing behind.
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
            // Like interrupted `cp`: leave the partially written destination
            // file in place rather than removing it.
            break false;
        }

        match old_file.read(context.buffer) {
            Ok(0) => {
                // Before `new_file` is dropped: writing is what moves the
                // modification time, so it has to be restored once the last
                // byte is written.
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

/// Recreates a special file (FIFO, socket, or device node) as a fresh node
/// with the source's permission bits, like `cp -R` does. No bytes are
/// transferred: reading a FIFO would block until a writer appears. FIFOs and
/// sockets need no privileges; device nodes require root, so as a normal user
/// they record a "not permitted" error here, exactly as `cp` reports.
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
    // Everything comes from the one `lstat` the type came from. Device nodes
    // need the source's device numbers; the rest take zero.
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
        // The source's group first, as a moved file or directory gets it, so the
        // group bits restored below are granted to the group they were granted
        // to before. Best effort, the same way.
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

/// Gives a node a move created the source's permission bits, which the umask
/// trimmed at creation and `mv` keeps. By name, since a FIFO cannot be opened
/// without blocking, and without following a link swapped in at the name since
/// (`set_mode_at`). A filesystem that cannot
/// set a mode that way leaves the node as created, with a warning.
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
        // The entry at the name is not what failed; `replace_entry` says it
        // was left.
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

/// Creates the node `at` names in the destination directory. `_path` is where
/// that is, which only macOS needs.
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

/// Creates the node `at` names, at `path`: macOS has no `mknodat`. A parent
/// swapped for a symlink since it was opened could place the node outside the
/// tree; an empty FIFO or socket there carries nothing, and a device node
/// needs root.
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

/// The node type `mknod` takes for `file_type`, and the device numbers it
/// takes with it: a device node the source's, anything else zero.
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

/// The file type bits of `mode`.
pub(super) fn type_bits(mode: u32) -> u32 {
    mode & 0o170_000
}

/// Opens a regular file to copy from. `O_NOFOLLOW` refuses a symlink swapped
/// in since the entry was listed, and `O_NONBLOCK` keeps a FIFO swapped in
/// from blocking the open (and with it the worker every operation shares);
/// anything that is not a regular file is then refused before a byte is read,
/// so a device such as `/dev/zero` cannot be read without end either.
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
    // `O_NONBLOCK` was only for the open. None of the other flags `F_SETFL`
    // changes was asked for, so clearing them all clears only it.
    fcntl(&fd, FcntlArg::F_SETFL(OFlag::empty()))?;
    Ok((File::from(fd), stat))
}

/// Creates the destination of a file copy. `O_EXCL` fails atomically if the
/// name is taken, closing the same window as `rename_no_replace`: without it a
/// file that appeared since `validate_paths` ran would be truncated, and
/// `O_NOFOLLOW` keeps a symlink planted at the name from being written through.
///
/// A copy is created with the source's permission bits, which the umask trims
/// as it does for `cp`: the file is never readable by more users than the
/// source, even part way through. A move is created with the source's owner
/// bits only and given its full mode at the end, since `mv` keeps the mode
/// whatever the umask.
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

/// Settles an entry's destination name (`paths.new`), taken since the queue
/// saw it free: when the task started for the top-level entry, and at any time
/// inside a directory this copy created. It is never replaced (`Conflicts`):
/// a standing "skip all" skips it, which also makes a move keep its source,
/// and otherwise it is recorded like any other entry that could not be
/// written.
pub(super) fn resolve_raced(context: &mut CopyContext<'_>, paths: &Paths) {
    // The staging directory was created empty for the one entry, so a name
    // taken in it is no race another paste or program could win fairly: it
    // is refused, never skipped, and what holds it is never landed.
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
