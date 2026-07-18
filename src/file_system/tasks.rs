use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool, mpsc::Sender},
    thread,
};

use anyhow::{Result, anyhow};
use log::{info, warn};

use super::path_info::PathInfo;
use crate::{
    command::{
        Command,
        progress::{ActiveTask, CancellationToken, Task, TaskKind, Transfer},
        result::CommandResult,
    },
    file_system::debounce,
};

const BUFFER_SIZE_DIVISOR: u64 = 20;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size
/// Floor for the per-file copy read buffer. `buffer_bytes` can yield a very
/// small (even zero) size when a directory's scanned total is tiny or stale; a
/// file that appears or grows between the size scan and the copy must still be
/// read in reasonable chunks rather than a zero-length buffer that would read
/// `Ok(0)` immediately and write an empty destination. 8 KiB matches std's
/// default I/O buffer size.
const MIN_COPY_BUFFER_BYTES: usize = 8 * 1024;

pub struct CancelInfo {
    pub id: usize,
    pub token: CancellationToken,
    pub kind: TaskKind,
    /// Flips to `true` when the task can no longer be meaningfully cancelled
    /// (terminal state reached, or a non-interruptible stage entered). The
    /// cancel stack drops such entries without cancelling anything.
    pub uncancellable: Arc<AtomicBool>,
}

pub struct TaskRunResult {
    pub command_result: CommandResult,
    pub cancel_info: Option<CancelInfo>,
}

impl TaskRunResult {
    fn failed(result: CommandResult) -> Self {
        Self {
            command_result: result,
            cancel_info: None,
        }
    }

    /// The initial progress snapshot has already been sent through the task's
    /// channel (before the worker thread was spawned, so it always precedes
    /// any terminal update), so no command is returned here.
    fn started(initial: Task, token: CancellationToken, uncancellable: Arc<AtomicBool>) -> Self {
        Self {
            cancel_info: Some(CancelInfo {
                id: initial.id(),
                token,
                kind: initial.kind().clone(),
                uncancellable,
            }),
            command_result: CommandResult::Handled,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TaskCommand {
    Copy(PathInfo, PathInfo),
    Delete(PathInfo),
    Move(PathInfo, PathInfo),
}

impl TaskCommand {
    pub fn run(
        self,
        tx: Sender<Command>,
        buffer_min_bytes: u64,
        buffer_max_bytes: u64,
    ) -> TaskRunResult {
        match self {
            TaskCommand::Copy(path, dir) => {
                run_copy_task(tx, path, dir, buffer_min_bytes, buffer_max_bytes)
            }
            TaskCommand::Delete(path) => run_delete_task(tx, path),
            TaskCommand::Move(path, dir) => {
                run_move_task(tx, path, dir, buffer_min_bytes, buffer_max_bytes)
            }
        }
    }
}

fn run_copy_task(
    tx: Sender<Command>,
    path: PathInfo,
    dir: PathInfo,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> TaskRunResult {
    let (old_path, new_path) = match validate_paths(&path, &dir, "copy") {
        Ok(paths) => paths,
        Err(result) => return TaskRunResult::failed(result),
    };
    let path = match restat_source("copy", &old_path) {
        Ok(fresh) => fresh,
        Err(result) => return TaskRunResult::failed(result),
    };

    info!("Copying {old_path:?} to {new_path:?}");
    let kind = TaskKind::Copy(Transfer {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    });

    let is_directory = path.is_directory();
    // Pre-flight: if the directory's top level cannot even be read, fail fast
    // *before* the task is registered (no progress notice is created). The
    // expensive part (the recursive size walk) still runs off the UI thread
    // below.
    //
    // A symlink (even to a directory) has `is_directory == false`, so it skips
    // this check and is later recreated as a link by `copy_symlink` rather than
    // followed.
    if is_directory && let Err(error) = fs::read_dir(&old_path) {
        return TaskRunResult::failed(
            Command::AlertError(format!("Failed to read directory {old_path:?}: {error}")).into(),
        );
    }

    // Seed with the entry's own size; for a directory the real total is
    // computed in the worker thread (the scan can be slow and must not block
    // the UI event loop) and applied via `active.set_total`.
    let (mut active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let file_size = path.size;
    let source_mode = path.mode();
    // Send the initial snapshot before spawning the worker so it always
    // precedes any terminal update on the channel.
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    thread::spawn(move || {
        let total_size = if is_directory {
            let size = dir_total_size(&old_path);
            active.set_total(size);
            size
        } else {
            file_size
        };
        let buffer_size = copy_buffer_bytes(total_size, buffer_min_bytes, buffer_max_bytes);
        let mut errors = Vec::new();
        if !copy_path(
            &old_path,
            &new_path,
            &mut active,
            &mut errors,
            buffer_size,
            is_directory,
            source_mode,
        ) {
            active.cancelled();
            return;
        }
        finalize_copy(active, errors);
    });

    TaskRunResult::started(initial, token, uncancellable)
}

fn run_move_task(
    tx: Sender<Command>,
    path: PathInfo,
    dir: PathInfo,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> TaskRunResult {
    let (old_path, new_path) = match validate_paths(&path, &dir, "move") {
        Ok(paths) => paths,
        Err(result) => return TaskRunResult::failed(result),
    };
    let path = match restat_source("move", &old_path) {
        Ok(fresh) => fresh,
        Err(result) => return TaskRunResult::failed(result),
    };

    info!("Moving {old_path:?} to {new_path:?}");
    let kind = TaskKind::Move(Transfer {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    });
    let (mut active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let size = path.size;
    let source_mode = path.mode();
    let is_directory = path.is_directory();
    // Send the initial snapshot before spawning the worker so it always
    // precedes any terminal update on the channel.
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    thread::spawn(move || {
        // Narrow the cancel-vs-rename window: a keypress that lands before
        // the worker starts is honored instead of racing the rename.
        if active.is_cancelled() {
            active.cancelled();
            return;
        }
        match rename_no_replace(&old_path, &new_path) {
            Ok(_) => {
                active.increment(size);
                active.done();
            }
            Err(error) => match error.kind() {
                // If the file is on a different device/mount-point, we must copy-then-delete it instead
                ErrorKind::CrossesDevices => {
                    // A directory entry's own size is not the transfer size: scan
                    // for the real total before copying (mirrors `run_copy_task`).
                    let total_size = if is_directory {
                        let size = dir_total_size(&old_path);
                        active.set_total(size);
                        size
                    } else {
                        size
                    };
                    let buffer_size =
                        copy_buffer_bytes(total_size, buffer_min_bytes, buffer_max_bytes);
                    let mut errors = Vec::new();
                    if !copy_path(
                        &old_path,
                        &new_path,
                        &mut active,
                        &mut errors,
                        buffer_size,
                        is_directory,
                        source_mode,
                    ) {
                        active.cancelled();
                        return;
                    }
                    // Like `mv`: the source is removed only after a fully clean
                    // copy. If any entry failed, the entire source is left in
                    // place (even entries that copied fine) and the partial
                    // destination is kept.
                    if !errors.is_empty() {
                        finalize_copy(active, errors);
                        return;
                    }
                    // Not cancellable: the copy is complete, so removing the
                    // source outright is the only way to finish the move. Mark it
                    // so a cancel keypress during this stage does not claim to
                    // have cancelled anything.
                    active.set_uncancellable();
                    let removed = if is_directory {
                        fs::remove_dir_all(&old_path)
                    } else {
                        fs::remove_file(&old_path)
                    };
                    match removed {
                        Ok(_) => active.done(),
                        Err(error) => active.error(format!(
                            "Copy succeeded, but failed to delete original {old_path:?}: {error}"
                        )),
                    }
                }
                _ => active.error(format!(
                    "Failed to move {old_path:?} to {new_path:?}: {error}"
                )),
            },
        }
    });

    TaskRunResult::started(initial, token, uncancellable)
}

fn run_delete_task(tx: Sender<Command>, path: PathInfo) -> TaskRunResult {
    // Same staleness class as copy/move: the selection-time metadata may be
    // outdated. A directory replaced by a symlink must be unlinked as a
    // link, not followed into its target.
    let path = match restat_source("delete", &path.path) {
        Ok(fresh) => fresh,
        Err(result) => return TaskRunResult::failed(result),
    };
    let kind = TaskKind::Delete {
        path: display_path(&path.path),
    };
    let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let is_directory = path.is_directory();
    let path = path.path.clone();
    info!("Deleting {path:?}");
    // Send the initial snapshot before spawning the worker so it always
    // precedes any terminal update on the channel.
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    thread::spawn(move || {
        if let Some(active) = remove_path(&path, is_directory, active) {
            active.done();
        }
    });

    TaskRunResult::started(initial, token, uncancellable)
}

fn buffer_bytes(len: u64, buffer_min_bytes: u64, buffer_max_bytes: u64) -> usize {
    // 1) For files ≤ buffer_min_bytes:
    //    Use len of the file as the buffer size
    // 2) For files ≥ (buffer_max_bytes * 20):
    //    Use buffer_max_bytes buffer
    // 3) For files > buffer_min_bytes and < (buffer_max_bytes * 20):
    //    Use the maximum of buffer_min_bytes or len / 20
    //    This ensures we never go below buffer_min_bytes
    if len <= buffer_min_bytes {
        len as usize
    } else if len >= (buffer_max_bytes * BUFFER_SIZE_DIVISOR) {
        buffer_max_bytes as usize
    } else {
        std::cmp::max(buffer_min_bytes, len / BUFFER_SIZE_DIVISOR) as usize
    }
}

/// The read-buffer size for a copy: `buffer_bytes` floored at
/// `MIN_COPY_BUFFER_BYTES` so a tiny or zero scanned total never yields a
/// buffer too small to make progress. The floor itself is capped at
/// `buffer_max_bytes`, which a user may configure below the floor.
fn copy_buffer_bytes(len: u64, buffer_min_bytes: u64, buffer_max_bytes: u64) -> usize {
    buffer_bytes(len, buffer_min_bytes, buffer_max_bytes)
        .max(MIN_COPY_BUFFER_BYTES.min(buffer_max_bytes as usize))
}

/// Best-effort recursive size for the progress total. Entries that cannot be
/// read are skipped here; the copy itself reports them as errors.
fn dir_total_size(root: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if !metadata.is_symlink() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Unwraps a `Result`, or finalizes `$active` with `"{$ctx}: {error}"` and
/// returns `None` from the enclosing function. `$ctx` must not reference an
/// `error` binding of its own (macro hygiene binds the error here).
macro_rules! try_or_abort {
    ($active:expr, $result:expr, $ctx:expr) => {
        match $result {
            Ok(value) => value,
            Err(error) => {
                $active.error(format!("{}: {error}", $ctx));
                return None;
            }
        }
    };
}

/// Lists `$dir` for `remove_path`: unwraps the collected entries, finalizes
/// `$active` as cancelled and returns `None` if the drain was cancelled, or
/// as an error and returns `None` if the read failed.
macro_rules! list_or_abort {
    ($active:expr, $dir:expr) => {{
        let dir = $dir;
        match list_entries(&$active, dir) {
            Ok(Some(entries)) => entries,
            Ok(None) => {
                $active.cancelled();
                return None;
            }
            Err(error) => {
                $active.error(format!("Failed to read directory {dir:?}: {error}"));
                return None;
            }
        }
    }};
}

/// Re-stats the source at task start: selection- and yank-time metadata may
/// be stale, and the path may have changed since (e.g. replaced by a FIFO,
/// which a byte-wise copy would block on forever, or by a symlink, which a
/// delete must unlink rather than follow).
fn restat_source(operation: &str, old_path: &Path) -> Result<PathInfo, CommandResult> {
    PathInfo::try_from(old_path)
        .map_err(|error| anyhow!("Cannot {operation} {old_path:?}: {error}").into())
}

/// Finalizes a copy/move task following coreutils semantics: success when no
/// per-entry errors were recorded, otherwise a single error alert summarizing
/// them. Every recorded error is also logged.
fn finalize_copy(active: ActiveTask, errors: Vec<String>) {
    if errors.is_empty() {
        active.done();
        return;
    }
    for error in &errors {
        warn!("{error}");
    }
    let summary = if errors.len() == 1 {
        errors.into_iter().next().expect("errors is non-empty")
    } else {
        format!(
            "{} ({} more errors; see the log)",
            errors[0],
            errors.len() - 1
        )
    };
    active.error(summary);
}

/// The copy functions below follow coreutils `cp -R`/`mv` semantics: an entry
/// that cannot be copied is recorded in `errors` and the copy continues with
/// the remaining entries. Each returns `false` only when the task was
/// cancelled, in which case the caller must finalize with
/// `active.cancelled()`; otherwise the caller finalizes via `finalize_copy`.
/// A cancelled copy leaves the partially copied destination in place, like an
/// interrupted `cp`; the destination is not removed.
fn copy_directory(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    buffer_size: usize,
) -> bool {
    if let Err(error) = fs::create_dir(new_path) {
        // The subtree cannot be copied at all; skip it and continue with
        // the siblings.
        errors.push(format!("Failed to create directory {new_path:?}: {error}"));
        return true;
    }

    // Capture the source's mode now, but apply it only on every non-cancel
    // exit, after the contents are copied. Applying it up front would, for a
    // source mode without owner-write (e.g. 0o555), stop us from creating this
    // directory's own children. Matches `cp`, which sets directory permissions
    // last. The mode comes from the source's metadata, not from reading its
    // entries, so it is applied even when the read below fails.
    let source_mode = fs::symlink_metadata(old_path)
        .ok()
        .map(|metadata| metadata.permissions().mode());
    let apply_source_mode = || {
        if let Some(mode) = source_mode {
            apply_permissions(mode, new_path);
        }
    };

    let entries = match fs::read_dir(old_path) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!("Failed to read directory {old_path:?}: {error}"));
            apply_source_mode();
            return true;
        }
    };

    for entry in entries {
        if active.is_cancelled() {
            // Like interrupted `cp`: leave the partially copied destination in
            // place rather than removing it.
            return false;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("Failed to read entry in {old_path:?}: {error}"));
                continue;
            }
        };

        let src = entry.path();
        let dst = new_path.join(entry.file_name());
        let metadata = match fs::symlink_metadata(&src) {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(format!("Failed to read metadata for {src:?}: {error}"));
                continue;
            }
        };

        // `metadata` comes from `symlink_metadata`, so its mode carries
        // `S_IFLNK` for a symlink; `copy_path` dispatches links, directories,
        // and files uniformly off that mode.
        if !copy_path(
            &src,
            &dst,
            active,
            errors,
            buffer_size,
            metadata.is_dir(),
            metadata.permissions().mode(),
        ) {
            return false;
        }
    }

    apply_source_mode();
    true
}

/// Recreates the symlink at `old_path` as a new symlink at `new_path` pointing
/// to the same (possibly relative, possibly dangling) target. The link itself
/// is copied; its target is never followed, so no bytes are transferred and no
/// permissions are applied (`fs::set_permissions` would follow the link and
/// chmod the target instead of the link).
fn copy_symlink(old_path: &Path, new_path: &Path, errors: &mut Vec<String>) {
    match fs::read_link(old_path) {
        Ok(target) => {
            if let Err(error) = std::os::unix::fs::symlink(&target, new_path) {
                errors.push(format!("Failed to create symlink {new_path:?}: {error}"));
            }
        }
        Err(error) => errors.push(format!("Failed to read symlink {old_path:?}: {error}")),
    }
}

/// Copies a file chunk-by-chunk, sending debounced progress updates via
/// `active` and applying `source_mode`'s permissions on success. Failures are
/// recorded in `errors`; returns `false` only when cancelled.
fn copy_file(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    buffer_size: usize,
    source_mode: u32,
) -> bool {
    let total_size = active.total_size();
    let (mut old_file, mut new_file) = match open_files(old_path, new_path) {
        Ok(files) => files,
        Err(error) => {
            errors.push(format!(
                "Failed to copy {old_path:?} to {new_path:?}: {error}"
            ));
            return true;
        }
    };

    let mut buffer = vec![0; buffer_size];
    let mut debouncer = debounce::BytesDebouncer::new(PROGRESS_DEBOUNCE_PERCENTAGE, total_size);

    loop {
        if active.is_cancelled() {
            // Like interrupted `cp`: leave the partially written destination
            // file in place rather than removing it.
            return false;
        }

        match old_file.read(&mut buffer) {
            Ok(0) => {
                apply_permissions(source_mode, new_path);
                return true;
            }
            Ok(bytes) => match new_file.write_all(&buffer[..bytes]) {
                Ok(()) => {
                    active.increment(bytes as u64);
                    if debouncer.should_trigger(bytes as u64) {
                        active.send_progress();
                    }
                }
                Err(error) => {
                    errors.push(format!("Failed to write {new_path:?}: {error}"));
                    return true;
                }
            },
            Err(error) => {
                errors.push(format!("Failed to read {old_path:?}: {error}"));
                return true;
            }
        }
    }
}

/// Copies a directory, file, symlink, or special file, dispatching on
/// `is_directory` and `source_mode`. Per-entry failures accumulate in
/// `errors`; returns `false` only when the task was cancelled.
fn copy_path(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    buffer_size: usize,
    is_directory: bool,
    source_mode: u32,
) -> bool {
    // Check symlink first: `source_mode` comes from `symlink_metadata`, so a
    // symlink (even one pointing at a directory) is recreated as a link rather
    // than followed. `is_directory` is already false for any symlink.
    if unix_mode::is_symlink(source_mode) {
        copy_symlink(old_path, new_path, errors);
        true
    } else if is_directory {
        copy_directory(old_path, new_path, active, errors, buffer_size)
    } else if unix_mode::is_file(source_mode) {
        copy_file(old_path, new_path, active, errors, buffer_size, source_mode)
    } else {
        copy_special(old_path, new_path, errors, source_mode);
        true
    }
}

/// Recreates a special file (FIFO, socket, or device node) as a fresh node at
/// `new_path` with the source's permission bits, like `cp -R` does. No bytes
/// are transferred: reading a FIFO would block until a writer appears. FIFOs
/// and sockets need no privileges; device nodes require root, so as a normal
/// user they record a "not permitted" error here, exactly as `cp` reports.
fn copy_special(old_path: &Path, new_path: &Path, errors: &mut Vec<String>, source_mode: u32) {
    use nix::sys::stat::{Mode, SFlag, mknod};

    let kind = if unix_mode::is_fifo(source_mode) {
        SFlag::S_IFIFO
    } else if unix_mode::is_socket(source_mode) {
        SFlag::S_IFSOCK
    } else if unix_mode::is_block_device(source_mode) {
        SFlag::S_IFBLK
    } else if unix_mode::is_char_device(source_mode) {
        SFlag::S_IFCHR
    } else {
        errors.push(format!("Cannot copy {old_path:?}: unsupported file type"));
        return;
    };

    // Device nodes need the source's device numbers; zero for the rest.
    let rdev = if matches!(kind, SFlag::S_IFBLK | SFlag::S_IFCHR) {
        use std::os::unix::fs::MetadataExt;
        match fs::symlink_metadata(old_path) {
            Ok(metadata) => metadata.rdev() as nix::libc::dev_t,
            Err(error) => {
                errors.push(format!("Failed to read metadata for {old_path:?}: {error}"));
                return;
            }
        }
    } else {
        0
    };

    // `mode_t` is u32 on Linux but u16 on macOS, so cast rather than assume.
    let permissions = Mode::from_bits_truncate(source_mode as nix::libc::mode_t);
    if let Err(error) = mknod(new_path, kind, permissions, rdev) {
        errors.push(format!("Cannot create special file {new_path:?}: {error}"));
    }
}

/// Removes a file or directory tree, checking for cancellation between
/// entries. Cancelling mid-delete leaves whatever has not been removed yet.
/// Iterative (explicit stack), so directory depth cannot overflow the thread
/// stack.
///
/// Returns `Some(active)` on success, leaving finalization to the caller.
/// Returns `None` when cancelled or on error, in which case the task has
/// already been finalized via `active.cancelled()` / `active.error()`.
fn remove_path(path: &Path, is_directory: bool, active: ActiveTask) -> Option<ActiveTask> {
    if active.is_cancelled() {
        active.cancelled();
        return None;
    }
    if !is_directory {
        // Symlinks are removed as links (never followed): `is_directory` comes
        // from `symlink_metadata`, so a link to a directory takes this branch.
        try_or_abort!(
            active,
            fs::remove_file(path),
            format!("Failed to delete {path:?}")
        );
        return Some(active);
    }

    // Post-order walk. Each directory is drained into a Vec before anything
    // is deleted: the directory fd closes immediately (depth is not bounded
    // by the fd limit) and nothing is unlinked while a ReadDir stream is
    // open (mutating a directory mid-iteration can skip entries on some
    // filesystems, e.g. NFS). The trade is peak memory: each level's
    // not-yet-visited entries stay held on the stack, so memory is
    // proportional to the widest root-to-leaf path rather than O(1) per
    // level. A directory is removed once its collected entries are done, so
    // a cancelled or failed delete leaves each subtree either fully removed
    // or still intact.
    let root = path.to_path_buf();
    let entries = list_or_abort!(active, &root);
    let mut stack = vec![(root, entries.into_iter())];
    while let Some(top) = stack.last_mut() {
        if active.is_cancelled() {
            active.cancelled();
            return None;
        }
        match top.1.next() {
            // This directory's entries are done; remove it.
            None => {
                let (directory, _) = stack.pop().expect("stack is non-empty");
                try_or_abort!(
                    active,
                    fs::remove_dir(&directory),
                    format!("Failed to delete {directory:?}")
                );
            }
            Some((entry_path, true)) => {
                let entries = list_or_abort!(active, &entry_path);
                stack.push((entry_path, entries.into_iter()));
            }
            Some((entry_path, false)) => {
                try_or_abort!(
                    active,
                    fs::remove_file(&entry_path),
                    format!("Failed to delete {entry_path:?}")
                );
            }
        }
    }
    Some(active)
}

/// Collects `(path, is_directory)` for each entry of `directory`, closing the
/// directory's fd before the caller deletes anything. `file_type()` does not
/// follow symlinks, so a link to a directory reports `false` and is later
/// unlinked rather than descended into. Checks `active` for cancellation once
/// per entry (matching the per-entry cadence of the delete loop) so a huge
/// directory does not delay a cancel; returns `Ok(None)` when cancelled.
fn list_entries(
    active: &ActiveTask,
    directory: &Path,
) -> std::io::Result<Option<Vec<(PathBuf, bool)>>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        if active.is_cancelled() {
            return Ok(None);
        }
        let entry = entry?;
        let file_type = entry.file_type()?;
        entries.push((entry.path(), file_type.is_dir()));
    }
    Ok(Some(entries))
}

fn apply_permissions(mode: u32, path: &Path) {
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        warn!("Failed to set permissions on {path:?}: {e}");
    }
}

/// Renames `old_path` to `new_path`, failing atomically with an
/// `AlreadyExists` error if `new_path` already exists (unlike `fs::rename`,
/// which silently replaces it). `validate_paths` already rejects an existing
/// destination, but that check runs on the UI thread before the worker starts;
/// this folds the check into the rename so a destination that appears in the
/// interim is not clobbered.
///
/// Linux uses `renameat2(RENAME_NOREPLACE)` and macOS uses
/// `renameatx_np(RENAME_EXCL)`, both via rustix's safe `renameat_with` wrapper
/// (so no `unsafe`, which is forbidden crate-wide). Other targets fall back to
/// `fs::rename`, retaining only the pre-existing narrow race.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_no_replace(old_path: &Path, new_path: &Path) -> std::io::Result<()> {
    use rustix::{
        fs::{CWD, RenameFlags, renameat_with},
        io::Errno,
    };

    // `CWD` as both dirfds: absolute paths ignore it and relative paths resolve
    // against the current directory, matching `fs::rename`.
    match renameat_with(CWD, old_path, CWD, new_path, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        // Some filesystems reject the no-replace flag; fall back to a plain
        // rename (validate_paths already guarded the destination, so only the
        // narrow race reopens here).
        Err(Errno::NOSYS | Errno::INVAL | Errno::NOTSUP) => fs::rename(old_path, new_path),
        // Preserve the errno (e.g. XDEV -> CrossesDevices, EXIST ->
        // AlreadyExists) so callers can dispatch on `error.kind()`.
        Err(errno) => Err(errno.into()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn rename_no_replace(old_path: &Path, new_path: &Path) -> std::io::Result<()> {
    fs::rename(old_path, new_path)
}

fn open_files(source: &Path, target: &Path) -> Result<(File, File)> {
    let source = File::open(source)?;
    // `create_new` (O_EXCL|O_CREAT) fails atomically if the target already
    // exists, folding the check into the open. `validate_paths` already rejects
    // an existing top-level destination, but that check runs on the UI thread
    // before the worker starts; this closes the window where a file appears at
    // the destination in between and would otherwise be truncated.
    let target = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    Ok((source, target))
}

/// An absolute, display-friendly rendering of `path` for the operations
/// notice. Lexical only (no filesystem access), so it works for destination
/// paths that do not exist yet; falls back to the original path if it cannot
/// be absolutized.
fn display_path(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

/// Collapses `.` and `..` components purely lexically (no filesystem access,
/// so it works for destinations that do not exist yet). `..` pops the previous
/// component; at the root it is a no-op.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn validate_paths(
    source: &PathInfo,
    destination_directory: &PathInfo,
    operation: &str,
) -> Result<(PathBuf, PathBuf), CommandResult> {
    let old_path = source.path.clone();
    let new_path = destination_directory.path.join(&source.display_name);

    if old_path == new_path {
        return Err(anyhow!("Cannot {operation} {old_path:?} to {new_path:?}: Source and destination paths must be different").into());
    }

    // Reject copying/moving a directory into its own subtree. Without this, a
    // copy creates the destination under the source and then recurses into it
    // forever, filling the disk. Compare lexically-absolute, `..`-collapsed
    // paths so the component-wise prefix check is not fooled by relative paths
    // or parent-dir segments (e.g. `/a/c/../b`).
    let abs_old =
        lexical_normalize(&std::path::absolute(&old_path).unwrap_or_else(|_| old_path.clone()));
    let abs_new =
        lexical_normalize(&std::path::absolute(&new_path).unwrap_or_else(|_| new_path.clone()));
    if abs_new.starts_with(&abs_old) {
        return Err(anyhow!(
            "Cannot {operation} {old_path:?} into its own subdirectory {new_path:?}"
        )
        .into());
    }

    // Refuse to overwrite an existing destination. `File::create`/`fs::rename`
    // would silently replace it, and truncating a destination that is a hard
    // link to the source would destroy the source's contents as well.
    if new_path.symlink_metadata().is_ok() {
        return Err(anyhow!(
            "Cannot {operation} {old_path:?} to {new_path:?}: destination already exists"
        )
        .into());
    }

    Ok((old_path, new_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    // (len, min, max) -> expected buffer size. With BUFFER_SIZE_DIVISOR == 20.
    #[test_case(0, 10, 100 => 0 ; "zero length")]
    #[test_case(5, 10, 100 => 5 ; "below min uses len")]
    #[test_case(10, 10, 100 => 10 ; "equal to min uses len")]
    #[test_case(2000, 10, 100 => 100 ; "at max*divisor uses max")]
    #[test_case(5000, 10, 100 => 100 ; "above max*divisor uses max")]
    #[test_case(400, 10, 100 => 20 ; "mid range uses len/divisor")]
    #[test_case(30, 10, 100 => 10 ; "mid range floored at min")]
    fn buffer_bytes_is_correct(len: u64, min: u64, max: u64) -> usize {
        buffer_bytes(len, min, max)
    }

    #[test]
    fn copy_buffer_bytes_floors_at_min_copy_buffer() {
        // A zero scanned total would map to a 0-length buffer; the floor keeps
        // it at MIN_COPY_BUFFER_BYTES so a file read into it makes progress.
        assert_eq!(
            MIN_COPY_BUFFER_BYTES,
            copy_buffer_bytes(0, 64_000, 64_000_000)
        );
    }

    #[test]
    fn copy_buffer_bytes_never_exceeds_max() {
        // The floor must not override a user-configured buffer_max_bytes set
        // below MIN_COPY_BUFFER_BYTES.
        let max = 4_000;
        assert!((MIN_COPY_BUFFER_BYTES as u64) > max);
        assert_eq!(max as usize, copy_buffer_bytes(0, max, max));
    }

    #[test]
    fn copy_buffer_bytes_is_a_noop_for_large_buffers() {
        // Well above the floor: passes through buffer_bytes unchanged.
        assert_eq!(
            buffer_bytes(1_000_000, 64_000, 64_000_000),
            copy_buffer_bytes(1_000_000, 64_000, 64_000_000)
        );
    }

    fn path_info(path: &str, basename: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.path = PathBuf::from(path);
        info.display_name = basename.to_string();
        info
    }

    #[test]
    fn validate_paths_rejects_identical_source_and_destination() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a", "a");
        assert!(validate_paths(&src, &dest, "copy").is_err());
    }

    #[test]
    fn validate_paths_rejects_destination_inside_source() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/b/c", "c");
        assert!(validate_paths(&src, &dest, "copy").is_err());
    }

    #[test]
    fn validate_paths_rejects_destination_inside_source_via_parent_dir() {
        // new_path = "/a/c/../b" must normalize to "/a/b" and be rejected,
        // even though a raw component-wise prefix check would not catch it.
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/c/..", "c");
        assert!(validate_paths(&src, &dest, "copy").is_err());
    }

    #[test]
    fn validate_paths_allows_sibling_destination() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/x", "x");
        let (old_path, new_path) = validate_paths(&src, &dest, "copy").expect("should be allowed");
        assert_eq!(PathBuf::from("/a/b"), old_path);
        assert_eq!(PathBuf::from("/x/b"), new_path);
    }

    #[test]
    fn validate_paths_allows_destination_with_shared_prefix_but_different_component() {
        // "/a/bb" must not be treated as inside "/a/b".
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/bb", "bb");
        assert!(validate_paths(&src, &dest, "copy").is_ok());
    }

    #[test]
    fn validate_paths_rejects_existing_destination() {
        let fx = TempDir::new();
        std::fs::write(fx.dir.join("existing.txt"), b"x").unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.dir.to_str().unwrap(), "dir");
        assert!(validate_paths(&src, &dest, "copy").is_err());
    }

    #[test]
    fn validate_paths_rejects_existing_broken_symlink_destination() {
        let fx = TempDir::new();
        let link = fx.dir.join("existing.txt");
        std::os::unix::fs::symlink(fx.dir.join("missing"), &link).unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.dir.to_str().unwrap(), "dir");
        assert!(validate_paths(&src, &dest, "copy").is_err());
    }

    fn copy_task(tx: std::sync::mpsc::Sender<Command>) -> ActiveTask {
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        active
    }

    #[test]
    fn copy_path_recreates_socket() {
        let fx = TempDir::new();
        let src = fx.dir.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&src).unwrap();
        let dst = fx.dir.join("sock_copy");
        let mode = std::fs::symlink_metadata(&src)
            .unwrap()
            .permissions()
            .mode();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        assert!(copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            64,
            false,
            mode
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let dst_mode = std::fs::symlink_metadata(&dst)
            .unwrap()
            .permissions()
            .mode();
        assert!(unix_mode::is_socket(dst_mode));
        assert!(src.exists());
        active.done();
    }

    #[test]
    fn copy_path_recreates_fifo() {
        let fx = TempDir::new();
        let src = fx.dir.join("fifo");
        nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
        let dst = fx.dir.join("fifo_copy");
        let mode = std::fs::symlink_metadata(&src)
            .unwrap()
            .permissions()
            .mode();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        assert!(copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            64,
            false,
            mode
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let dst_mode = std::fs::symlink_metadata(&dst)
            .unwrap()
            .permissions()
            .mode();
        assert!(unix_mode::is_fifo(dst_mode));
        assert_eq!(0o644, dst_mode & 0o7777);
        active.done();
    }

    #[test]
    fn copy_path_continues_past_unreadable_entries() {
        // Root can read a chmod-000 file, which would defeat the test.
        if nix::unistd::geteuid().is_root() {
            return;
        }
        let fx = TempDir::new();
        let src = fx.dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("bad"), b"x").unwrap();
        std::fs::write(src.join("c.txt"), b"c").unwrap();
        let mut permissions = std::fs::metadata(src.join("bad")).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(src.join("bad"), permissions).unwrap();

        let dst = fx.dir.join("dst");
        let mode = std::fs::symlink_metadata(&src)
            .unwrap()
            .permissions()
            .mode();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        // Like cp -R: the unreadable entry is recorded and the rest is copied.
        assert!(copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            64,
            true,
            mode
        ));
        assert_eq!(1, errors.len(), "expected one error: {errors:?}");
        assert!(errors[0].contains("bad"), "unexpected error: {}", errors[0]);
        assert!(dst.join("a.txt").exists());
        assert!(dst.join("c.txt").exists());
        assert!(!dst.join("bad").exists());
        active.done();
    }

    #[test]
    fn rename_no_replace_moves_to_new_destination() {
        let fx = TempDir::new();
        let src = fx.dir.join("a.txt");
        std::fs::write(&src, b"x").unwrap();
        let dst = fx.dir.join("b.txt");

        rename_no_replace(&src, &dst).unwrap();
        assert!(!src.exists());
        assert_eq!(b"x".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn rename_no_replace_refuses_existing_destination() {
        let fx = TempDir::new();
        let src = fx.dir.join("a.txt");
        let dst = fx.dir.join("b.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::write(&dst, b"dst").unwrap();

        match rename_no_replace(&src, &dst) {
            Err(error) => {
                assert_eq!(std::io::ErrorKind::AlreadyExists, error.kind());
                // Both files must be untouched.
                assert_eq!(b"src".to_vec(), std::fs::read(&src).unwrap());
                assert_eq!(b"dst".to_vec(), std::fs::read(&dst).unwrap());
            }
            // Filesystem lacks an atomic no-replace rename and fell back to
            // fs::rename, which overwrites; nothing to assert here.
            Ok(()) => {}
        }
    }

    #[test]
    fn remove_path_deletes_directory_tree() {
        let fx = TempDir::new();
        let root = fx.dir.join("doomed");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        assert!(remove_path(&root, true, active).is_some());
        assert!(!root.exists());
    }

    #[test]
    fn remove_path_stops_when_already_cancelled() {
        let fx = TempDir::new();
        let root = fx.dir.join("kept");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        token.cancel();

        assert!(remove_path(&root, true, active).is_none());
        assert!(root.join("sub").join("f.txt").exists());
    }

    #[test]
    fn list_entries_reports_cancellation_during_the_drain() {
        let fx = TempDir::new();
        std::fs::write(fx.dir.join("a.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        token.cancel();

        // A cancel observed while listing yields Ok(None), so the caller
        // aborts instead of deleting a directory it never finished reading.
        assert!(list_entries(&active, &fx.dir).unwrap().is_none());
    }

    /// Self-cleaning unique temp directory.
    struct TempDir {
        dir: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            // A per-process counter guarantees a unique directory even when two
            // fixtures are created in the same nanosecond on parallel threads,
            // so one fixture's Drop never wipes another's directory.
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("filectrl_tasks_{}_{seq}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn new_active_task() -> ActiveTask {
        // Leak the receiver so the channel stays open for the task's lifetime;
        // ActiveTask ignores send errors anyway.
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let kind = TaskKind::Copy(Transfer {
            source: String::new(),
            destination: String::new(),
        });
        let (active, _initial, _token) = ActiveTask::new(tx, kind, 0);
        active
    }

    #[test]
    fn copy_path_recreates_a_symlink_without_following_it() {
        use std::os::unix::fs::PermissionsExt;

        let fx = TempDir::new();
        let target = fx.dir.join("target.txt");
        std::fs::write(&target, b"hello").unwrap();
        std::fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        let link = fx.dir.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let dst = fx.dir.join("copied_link.txt");

        let source_mode = fs::symlink_metadata(&link).unwrap().permissions().mode();
        assert!(unix_mode::is_symlink(source_mode));

        let mut active = new_active_task();
        let mut errors = Vec::new();
        assert!(copy_path(
            &link,
            &dst,
            &mut active,
            &mut errors,
            1024,
            false,
            source_mode
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        // The destination must itself be a symlink pointing at the same target,
        // not a regular file containing the target's bytes.
        let dst_meta = fs::symlink_metadata(&dst).unwrap();
        assert!(dst_meta.is_symlink(), "destination must be a symlink");
        assert_eq!(fs::read_link(&dst).unwrap(), target);

        // The link's target must be untouched: copy must not chmod through the
        // link or rewrite its contents.
        let target_mode = fs::symlink_metadata(&target).unwrap().permissions().mode() & 0o7777;
        assert_eq!(target_mode, 0o600, "copy must not chmod the symlink target");
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
    }
}
