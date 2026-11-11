use std::{
    ffi::OsString,
    fs::{self, File, Metadata},
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

use log::warn;

use super::{
    PROGRESS_DEBOUNCE_PERCENTAGE, PROGRESS_MIN_INTERVAL, cancel_logging,
    walk::{Entries, list_entries, scan_tree},
};
use crate::{
    command::progress::ActiveTask,
    file_system::{
        conflicts::failed_transfer,
        debounce,
        path_info::{PathInfo, compact},
    },
};

mod entry;
#[cfg(test)]
mod tests;

use self::entry::{copy_file, copy_special, copy_symlink};

/// Settings and shared state one copy carries from the task down to every entry of the tree.
struct CopyContext<'a> {
    active: &'a mut ActiveTask,
    /// Entries that could not be copied, in order. The copy continues past them, like `cp -R`.
    errors: Vec<String>,
    /// One read buffer for the whole tree.
    buffer: &'a mut [u8],
    /// The copy a move across devices makes, which keeps the source's full mode and modification
    /// time, like `mv`. A plain copy takes the umask, drops the special bits and keeps no times,
    /// like `cp` without `-p`.
    is_move: bool,
    /// One debouncer for the whole tree: a per-file debouncer sends an update for every file.
    progress: debounce::ProgressDebouncer,
    /// The top-level source and destination, for messages.
    roots: (PathBuf, PathBuf),
    /// The temporary name a replacement is written under, shown in messages as the destination.
    temporary: Option<PathBuf>,
}

impl<'a> CopyContext<'a> {
    fn new(
        is_move: bool,
        active: &'a mut ActiveTask,
        buffer: &'a mut [u8],
        total_size: u64,
    ) -> Self {
        Self {
            active,
            errors: Vec::new(),
            buffer,
            is_move,
            progress: debounce::ProgressDebouncer::new(
                PROGRESS_DEBOUNCE_PERCENTAGE,
                PROGRESS_MIN_INTERVAL,
                total_size,
            ),
            roots: (PathBuf::new(), PathBuf::new()),
            temporary: None,
        }
    }

    /// `path` as messages show it: a replacement's temporary name is shown as the destination.
    fn shown<'p>(&'p self, path: &'p Path) -> &'p Path {
        match &self.temporary {
            Some(temporary) if temporary == path => &self.roots.1,
            _ => path,
        }
    }

    /// The object of a transfer's message. Below the top level it names the entry relative to both
    /// sides, since `compact` keeps only the last components of a path.
    fn transfer_object(&self, old: &Path) -> String {
        let (old_root, new_root) = &self.roots;
        match old.strip_prefix(old_root) {
            Ok(entry) if !entry.as_os_str().is_empty() => format!(
                "{} from {} to {}",
                compact(entry),
                compact(old_root),
                compact(new_root)
            ),
            _ => format!("{} to {}", compact(old_root), compact(new_root)),
        }
    }

    fn into_outcome(self) -> CopyOutcome {
        CopyOutcome {
            errors: self.errors,
            #[cfg(test)]
            progress_threshold: self.progress.threshold(),
        }
    }
}

/// What a tree copy left behind: the errors of the entries it could not copy.
#[derive(Default)]
pub(super) struct CopyOutcome {
    pub(super) errors: Vec<String>,
    /// The tree debouncer's threshold.
    #[cfg(test)]
    pub(super) progress_threshold: u64,
}

/// The copy read buffer size, the same as coreutils `cp`. Keeps cancel latency near 10 ms under
/// writeback throttling.
const COPY_BUFFER_BYTES: usize = 128 * 1024;

/// The byte-copy stage shared by copy and cross-device move: sets the progress total for a
/// directory source, and copies the tree. `listed` is the source as the task was started for it.
///
/// `None` when the task was cancelled (and finalized); otherwise the task and what the walk left
/// behind, for the caller to finalize.
pub(super) fn copy_with_progress(
    is_move: bool,
    overwrite: bool,
    listed: &PathInfo,
    mut active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
) -> Option<(ActiveTask, CopyOutcome)> {
    let total_size = if listed.is_directory() {
        let Some(size) = dir_total_size(&active, old_path) else {
            active.cancelled();
            return None;
        };
        active.set_total(size);
        size
    } else {
        listed.size
    };
    let mut buffer = vec![0; COPY_BUFFER_BYTES];
    let mut context = CopyContext::new(is_move, &mut active, &mut buffer, total_size);
    let finished = copy_path(&mut context, overwrite, old_path, new_path);
    let outcome = context.into_outcome();
    if !finished {
        cancel_logging(&outcome.errors, active);
        return None;
    }
    Some((active, outcome))
}

/// Best-effort recursive size for the progress total; unreadable entries are skipped. `None` when
/// cancelled.
pub(super) fn dir_total_size(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total: u64 = 0;
    scan_tree(active, root, |metadata| {
        if metadata.is_file() {
            total = total.saturating_add(metadata.len());
        }
    })?;
    Some(total)
}

/// Copies with `cp -R`/`mv` semantics: an entry that cannot be copied is recorded and the copy
/// continues. Symlinks are copied as links, never followed. Returns `false` only when cancelled,
/// and then the caller finalizes with `active.cancelled()`. A cancelled copy leaves its partial
/// destination.
///
/// With `overwrite`, a non-directory is written under a hidden temporary name beside the
/// destination and renamed over it once whole, so a failed or cancelled copy leaves the entry it
/// would have replaced. Without it a taken name fails the copy.
fn copy_path(
    context: &mut CopyContext<'_>,
    overwrite: bool,
    old_path: &Path,
    new_path: &Path,
) -> bool {
    context.roots = (old_path.to_path_buf(), new_path.to_path_buf());
    let is_move = context.is_move;
    let metadata = match old_path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            context
                .errors
                .push(failed_transfer(is_move, old_path, new_path, &error));
            return true;
        }
    };
    // A directory never replaces anything; its name is found taken instead.
    if !overwrite || metadata.is_dir() {
        return copy_entry(context, &metadata, old_path, new_path);
    }
    let temporary = new_path.with_file_name(temporary_name());
    context.temporary = Some(temporary.clone());
    let errors_before = context.errors.len();
    let finished = copy_entry(context, &metadata, old_path, &temporary);
    context.temporary = None;
    if finished && context.errors.len() == errors_before {
        match fs::rename(&temporary, new_path) {
            Ok(()) => return true,
            Err(error) => {
                context
                    .errors
                    .push(failed_transfer(is_move, old_path, new_path, &error));
            }
        }
    }
    let _ = fs::remove_file(&temporary);
    finished
}

/// A hidden name, unique to this process and call, that a replacement is written under beside the
/// entry it replaces.
fn temporary_name() -> OsString {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(".filectrl-{}-{serial}", std::process::id()).into()
}

/// Copies the entry `metadata` (from `lstat`) describes. Returns `false` only when cancelled.
fn copy_entry(context: &mut CopyContext<'_>, metadata: &Metadata, old: &Path, new: &Path) -> bool {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        copy_directory(context, metadata, old, new)
    } else if file_type.is_symlink() {
        copy_symlink(context, old, new);
        true
    } else if file_type.is_file() {
        copy_file(context, metadata, old, new)
    } else {
        copy_special(context, metadata, old, new);
        true
    }
}

/// Creates `new` with owner access, copies everything in `old` into it, then gives it its final
/// mode (and times for a move), even after a cancel. Returns `false` only when cancelled.
fn copy_directory(
    context: &mut CopyContext<'_>,
    metadata: &Metadata,
    old: &Path,
    new: &Path,
) -> bool {
    let creation = (metadata.mode() & 0o777) | 0o700;
    if let Err(error) = fs::DirBuilder::new().mode(creation).create(new) {
        context.errors.push(format!(
            "Failed to create directory {}: {error}",
            compact(context.shown(new))
        ));
        return true;
    }
    let finished = match list_entries(old) {
        Ok(entries) => copy_entries(context, entries, old, new),
        Err(error) => {
            context.errors.push(format!(
                "Failed to read directory {}: {error}",
                compact(old)
            ));
            true
        }
    };
    finish_directory(context, metadata, new);
    finished
}

/// Copies each of `entries` from `old` into `new`. Returns `false` only when cancelled.
fn copy_entries(context: &mut CopyContext<'_>, entries: Entries, old: &Path, new: &Path) -> bool {
    for (name, _) in entries {
        if context.active.is_cancelled() {
            return false;
        }
        let old = old.join(&name);
        let metadata = match old.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                context.errors.push(format!(
                    "Failed to read metadata for {}: {error}",
                    compact(&old)
                ));
                continue;
            }
        };
        if !copy_entry(context, &metadata, &old, &new.join(&name)) {
            return false;
        }
    }
    true
}

/// Gives a copied directory its final mode, and for a move its modification time.
fn finish_directory(context: &CopyContext<'_>, metadata: &Metadata, new: &Path) {
    if let Ok(directory) = File::open(new) {
        finish(context, metadata, new, &directory);
    }
}

/// Gives a copied file or directory its final mode through its handle, and for a move the source's
/// modification time. Best effort: a failure is only logged.
///
/// A move keeps the full mode, like `mv`. A copy keeps what the umask left of the mode it was
/// created with, removing only the owner bits a directory was given so it could be filled, like
/// `cp -R`.
fn finish(context: &CopyContext<'_>, metadata: &Metadata, new: &Path, file: &File) {
    let shown = compact(context.shown(new));
    let source = metadata.mode();
    let mode = if context.is_move {
        Some(source & 0o7777)
    } else {
        file.metadata().ok().and_then(|created| {
            let created = created.mode() & 0o7777;
            let mode = created & !(0o700 & !source);
            (mode != created).then_some(mode)
        })
    };
    if let Some(mode) = mode
        && let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode))
    {
        warn!("Failed to set the mode of {shown}: {error}");
    }
    if context.is_move
        && let Err(error) = metadata.modified().and_then(|time| file.set_modified(time))
    {
        warn!("Failed to set the modification time of {shown}: {error}");
    }
}
