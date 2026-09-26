use std::{
    fs::{self, File, Metadata},
    io::{Read, Write},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Instant,
};

use log::warn;
use nix::sys::stat::SFlag;

use super::{CopyContext, finish};
use crate::file_system::{conflicts::verb, path_info::compact, tasks::sys::mode_bits};

/// Recreates the symlink `old` with the same target, which is never followed.
pub(super) fn copy_symlink(context: &mut CopyContext<'_>, old: &Path, new: &Path) {
    let target = match fs::read_link(old) {
        Ok(target) => target,
        Err(error) => {
            context
                .errors
                .push(format!("Failed to read symlink {}: {error}", compact(old)));
            return;
        }
    };
    if let Err(error) = std::os::unix::fs::symlink(target, new) {
        context.errors.push(format!(
            "Failed to create symlink {}: {error}",
            compact(context.shown(new))
        ));
    }
}

/// Copies a regular file, sending debounced progress. Returns `false` only when cancelled.
pub(super) fn copy_file(
    context: &mut CopyContext<'_>,
    metadata: &Metadata,
    old: &Path,
    new: &Path,
) -> bool {
    let failed = |context: &CopyContext<'_>, error: &std::io::Error| {
        format!(
            "Failed to {} {}: {error}",
            verb(context.is_move),
            context.transfer_object(old)
        )
    };
    // Opened before the destination is created, so an unreadable source leaves nothing behind.
    let mut old_file = match File::open(old) {
        Ok(file) => file,
        Err(error) => {
            let message = failed(context, &error);
            context.errors.push(message);
            return true;
        }
    };
    // A move is owner only until its full mode is applied at the end.
    let creation = if context.is_move {
        0o600
    } else {
        metadata.mode() & 0o777
    };
    // `create_new` fails on a taken name, and does not write through a symlink there.
    let mut new_file = match File::options()
        .write(true)
        .create_new(true)
        .mode(creation)
        .open(new)
    {
        Ok(file) => file,
        Err(error) => {
            let message = failed(context, &error);
            context.errors.push(message);
            return true;
        }
    };

    let not_cancelled = loop {
        if context.active.is_cancelled() {
            // Like interrupted `cp`: the partial destination stays.
            break false;
        }
        match old_file.read(context.buffer) {
            Ok(0) => break true,
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
                    context.errors.push(format!(
                        "Failed to write {}: {error}",
                        compact(context.shown(new))
                    ));
                    break true;
                }
            },
            Err(error) => {
                context
                    .errors
                    .push(format!("Failed to read {}: {error}", compact(old)));
                break true;
            }
        }
    };
    finish(context, metadata, new, &new_file);
    not_cancelled
}

/// Recreates a FIFO, socket, or device node with the source's permission bits, like `cp -R`. No
/// bytes are read: a FIFO would block. A device node needs root, otherwise "not permitted" is
/// recorded.
pub(super) fn copy_special(
    context: &mut CopyContext<'_>,
    metadata: &Metadata,
    old: &Path,
    new: &Path,
) {
    let file_type = metadata.file_type();
    let (kind, device) = if file_type.is_fifo() {
        (SFlag::S_IFIFO, 0)
    } else if file_type.is_socket() {
        (SFlag::S_IFSOCK, 0)
    } else if file_type.is_block_device() {
        (SFlag::S_IFBLK, metadata.rdev())
    } else if file_type.is_char_device() {
        (SFlag::S_IFCHR, metadata.rdev())
    } else {
        context.errors.push(format!(
            "Cannot {} {}: unsupported file type",
            verb(context.is_move),
            compact(old)
        ));
        return;
    };
    let mode = metadata.mode();
    // `dev_t` is u64 on Linux but i32 on macOS.
    #[allow(clippy::useless_conversion, clippy::unnecessary_fallible_conversions)]
    let device = device.try_into().unwrap_or_default();
    if let Err(error) = nix::sys::stat::mknod(new, kind, mode_bits(mode & 0o777), device) {
        context.errors.push(format!(
            "Failed to create special file {}: {error}",
            compact(context.shown(new))
        ));
        return;
    }
    if context.is_move
        && let Err(error) = fs::set_permissions(new, fs::Permissions::from_mode(mode & 0o7777))
    {
        warn!(
            "Failed to set the mode of {}: {error}",
            compact(context.shown(new))
        );
    }
}
