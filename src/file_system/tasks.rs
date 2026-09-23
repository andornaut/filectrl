use std::{
    ffi::{CStr, CString, OsStr},
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    os::{
        fd::AsFd,
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::AtomicBool,
        mpsc::{self, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use log::{error, info, warn};
use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, openat, statat, unlinkat};

use super::{
    Occupant, PasteStep,
    conflicts::Conflicts,
    path_info::{PathInfo, compact},
    step,
};
use crate::{
    command::{
        Command,
        progress::{ActiveTask, CancellationToken, Task, TaskKind, Transfer},
        result::CommandResult,
    },
    file_system::debounce,
};

/// The settings and shared state one copy carries from the task down to every
/// entry of the tree, so that adding another does not lengthen every signature
/// in between.
struct CopyContext<'a> {
    /// One read buffer for the whole tree; see `copy_with_progress`.
    buffer: &'a mut [u8],
    /// The paste's standing `*All` answer, which settles a name already taken
    /// at a destination inside the tree. `None` for an operation that is not
    /// part of a paste, which records such a name instead.
    conflicts: Option<&'a Conflicts>,
    /// Restore each entry's modification time, so a cross-device move leaves
    /// what a same-device rename would have.
    preserve_times: bool,
    /// The top-level source file, already opened by `prepare_destination`
    /// before it cleared the destination. `copy_file` takes it in place of
    /// opening the path again. `None` for any other source.
    source: Option<File>,
    /// Entries a standing "skip all" left alone. Counted separately from the
    /// errors: skipping is a choice rather than a failure, but a move still
    /// must not remove a source whose entries never reached the destination.
    skipped: usize,
}

/// What a tree copy left behind: the entries that could not be written, and how
/// many a standing "skip all" left alone.
#[derive(Default)]
struct CopyOutcome {
    errors: Vec<String>,
    skipped: usize,
}

const BUFFER_SIZE_DIVISOR: u64 = 20;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size
/// Shortest gap between two progress updates for one task. The percentage above
/// bounds them per unit of work, which for a fast copy is a hundred redraws
/// inside a second; this bounds them per unit of time.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Floor for the per-file copy read buffer. `buffer_bytes` can yield a very
/// small (even zero) size from a tiny or stale scanned total, and a zero-length
/// buffer reads `Ok(0)` at once and writes an empty destination. 8 KiB matches
/// std's default I/O buffer size.
const MIN_COPY_BUFFER_BYTES: usize = 8 * 1024;

type Job = Box<dyn FnOnce() + Send>;

/// Queues file-operation work on a single background thread, so that pasting or
/// deleting N marked entries runs one operation at a time rather than spawning N
/// threads that compete for the same disk. Jobs run in the order they were
/// queued. The thread is started on first use and lives for the rest of the
/// process.
///
/// Only the worker side is queued: each task still validates and registers
/// itself on the calling thread, so a batch reports which sources failed before
/// any of them starts.
fn queue_operation(job: impl FnOnce() + Send + 'static) {
    static QUEUE: OnceLock<Sender<Job>> = OnceLock::new();

    let queue = QUEUE.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::spawn(move || {
            for job in rx {
                // A panicking job must not take the worker down with it: the
                // receiver would be dropped, and every later operation would
                // register a task, announce itself at 0%, and never run. The
                // job's `ActiveTask` still finalizes as it unwinds, so the
                // notice clears. Release builds abort on panic, so this only
                // has anything to catch in a debug build.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    error!("A file operation panicked; the queue is still running");
                }
            }
        });
        tx
    });
    // The worker never exits, so the receiver outlives the process.
    let _ = queue.send(Box::new(job));
}

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
    fn started(initial: &Task, token: CancellationToken, uncancellable: Arc<AtomicBool>) -> Self {
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

/// A file operation to run. The `bool` on `Copy` and `Move` is the caller's
/// answer to a destination that already exists: `true` replaces it, `false`
/// refuses. It covers the top level only; a name another process takes inside
/// the tree while the copy runs is settled by the paste's standing answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TaskCommand {
    Copy(PathInfo, PathInfo, bool),
    Delete(PathInfo),
    Move(PathInfo, PathInfo, bool),
}

impl TaskCommand {
    /// `conflicts` is the paste's standing `*All` answer, which a copy consults
    /// when it finds a name already taken inside the tree it is writing.
    /// `None` for a delete, which never collides.
    pub fn run(
        self,
        tx: Sender<Command>,
        conflicts: Option<&Conflicts>,
        buffer_min_bytes: u64,
        buffer_max_bytes: u64,
    ) -> TaskRunResult {
        match self {
            TaskCommand::Copy(path, dir, overwrite) => run_copy_task(
                tx,
                &path,
                &dir,
                overwrite,
                conflicts,
                buffer_min_bytes,
                buffer_max_bytes,
            ),
            TaskCommand::Delete(path) => run_delete_task(tx, &path),
            TaskCommand::Move(path, dir, overwrite) => run_move_task(
                tx,
                &path,
                &dir,
                overwrite,
                conflicts,
                buffer_min_bytes,
                buffer_max_bytes,
            ),
        }
    }
}

fn run_copy_task(
    tx: Sender<Command>,
    path: &PathInfo,
    dir: &PathInfo,
    overwrite: bool,
    conflicts: Option<&Conflicts>,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> TaskRunResult {
    let conflicts = conflicts.cloned();
    // Re-stat first: `validate_paths` decides whether the subtree check applies
    // from the source's type, so it must not read selection-time metadata.
    let path = match restat_source("copy", &path.path) {
        Ok(fresh) => fresh,
        Err(result) => return TaskRunResult::failed(result),
    };
    let (old_path, new_path) = match validate_paths(&path, dir, "copy", overwrite) {
        Ok(paths) => paths,
        Err(result) => return TaskRunResult::failed(result),
    };

    info!("Copying {} to {}", old_path.display(), new_path.display());
    let kind = TaskKind::Copy(Transfer {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    });

    let is_directory = path.is_directory();
    // Fail before the task is registered, so an unreadable directory creates no
    // progress notice. The recursive size walk still runs off the UI thread.
    // A symlink has `is_directory == false` even when it points at a directory,
    // so it skips this and is recreated as a link by `copy_symlink`.
    if is_directory && let Err(error) = fs::read_dir(&old_path) {
        return TaskRunResult::failed(
            Command::AlertError(format!(
                "Failed to read directory {}: {error}",
                compact(&old_path)
            ))
            .into(),
        );
    }

    // Seed with the entry's own size; a directory's real total is scanned in
    // the worker, off the UI thread, and applied via `active.set_total`.
    let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let file_size = path.size;
    let source_mode = path.mode();
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        let Some((active, source)) = check_cancelled(active).and_then(|active| {
            prepare_destination(active, "copy", &old_path, &new_path, overwrite, source_mode)
        }) else {
            return;
        };
        if let Some((active, outcome)) = copy_with_progress(
            &old_path,
            &new_path,
            active,
            source,
            file_size,
            is_directory,
            source_mode,
            // Like `cp`, which does not preserve timestamps without `-p`.
            false,
            conflicts.as_ref(),
            buffer_min_bytes,
            buffer_max_bytes,
        ) {
            // A skipped entry needs no mention here: nothing is left behind by
            // a copy that did not make it, and the standing "skip all" that
            // settled it was the user's own answer.
            finalize_copy(active, outcome.errors);
        }
    });

    TaskRunResult::started(&initial, token, uncancellable)
}

fn run_move_task(
    tx: Sender<Command>,
    path: &PathInfo,
    dir: &PathInfo,
    overwrite: bool,
    conflicts: Option<&Conflicts>,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> TaskRunResult {
    let conflicts = conflicts.cloned();
    let path = match restat_source("move", &path.path) {
        Ok(fresh) => fresh,
        Err(result) => return TaskRunResult::failed(result),
    };
    let (old_path, new_path) = match validate_paths(&path, dir, "move", overwrite) {
        Ok(paths) => paths,
        Err(result) => return TaskRunResult::failed(result),
    };

    info!("Moving {} to {}", old_path.display(), new_path.display());
    let kind = TaskKind::Move(Transfer {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    });
    let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let size = path.size;
    let source_mode = path.mode();
    let is_directory = path.is_directory();
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        let Some(mut active) = check_cancelled(active) else {
            return;
        };
        match rename_for_move(&old_path, &new_path, overwrite) {
            Ok(()) => {
                active.increment(size);
                active.done();
            }
            Err(error) => match error.kind() {
                // If the file is on a different device/mount-point, we must copy-then-delete it instead
                ErrorKind::CrossesDevices => {
                    // There is no rename to replace the destination here, and
                    // the copy below opens it with `create_new`, so a granted
                    // overwrite has to clear it first.
                    let Some((active, source)) = prepare_destination(
                        active,
                        "move",
                        &old_path,
                        &new_path,
                        overwrite,
                        source_mode,
                    ) else {
                        return;
                    };
                    let Some((active, outcome)) = copy_with_progress(
                        &old_path,
                        &new_path,
                        active,
                        source,
                        size,
                        is_directory,
                        source_mode,
                        // A same-device move is a rename, which keeps the
                        // timestamps; the copy fallback has to put them back
                        // so the result does not depend on which mount the
                        // destination happens to be on.
                        true,
                        conflicts.as_ref(),
                        buffer_min_bytes,
                        buffer_max_bytes,
                    ) else {
                        return;
                    };
                    finish_cross_device_move(active, outcome, &old_path, is_directory);
                }
                _ => active.error(format!(
                    "Failed to move {} to {}: {error}",
                    compact(&old_path),
                    compact(&new_path)
                )),
            },
        }
    });

    TaskRunResult::started(&initial, token, uncancellable)
}

fn run_delete_task(tx: Sender<Command>, path: &PathInfo) -> TaskRunResult {
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
    // Delete progress counts entries, not bytes: a directory's own size says
    // nothing about how much work removing it is. Seed with the single entry a
    // non-directory delete removes; a directory's real total is scanned in the
    // worker, off the UI thread, and applied via `active.set_total`.
    let (mut active, initial, token) = ActiveTask::new(tx, kind, 1);
    let is_directory = path.is_directory();
    let path = path.path.clone();
    info!("Deleting {}", path.display());
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        if is_directory {
            let Some(total) = dir_total_entries(&active, &path) else {
                active.cancelled();
                return;
            };
            active.set_total(total);
        }
        if let Some(active) = remove_path(&path, is_directory, active) {
            active.done();
        }
    });

    TaskRunResult::started(&initial, token, uncancellable)
}

fn buffer_bytes(len: u64, buffer_min_bytes: u64, buffer_max_bytes: u64) -> usize {
    let bytes = if len <= buffer_min_bytes {
        len
    } else if len >= (buffer_max_bytes * BUFFER_SIZE_DIVISOR) {
        buffer_max_bytes
    } else {
        std::cmp::max(buffer_min_bytes, len / BUFFER_SIZE_DIVISOR)
    };
    // The sizes are u64 from the config and from a scanned total, so on a
    // 32-bit target one can exceed what a buffer could be allocated at.
    // Saturating asks for the largest buffer the address space can express.
    usize::try_from(bytes).unwrap_or(usize::MAX)
}

/// The read-buffer size for a copy: `buffer_bytes` floored at
/// `MIN_COPY_BUFFER_BYTES` so a tiny or zero scanned total never yields a
/// buffer too small to make progress. The floor itself is capped at
/// `buffer_max_bytes`, which a user may configure below the floor.
fn copy_buffer_bytes(len: u64, buffer_min_bytes: u64, buffer_max_bytes: u64) -> usize {
    buffer_bytes(len, buffer_min_bytes, buffer_max_bytes)
        .max(MIN_COPY_BUFFER_BYTES.min(usize::try_from(buffer_max_bytes).unwrap_or(usize::MAX)))
}

/// The byte-copy stage shared by copy and cross-device move: for a directory
/// source, scans the real transfer total (a directory entry's own size is not
/// the transfer size) and applies it via `set_total`, sizes the read buffer,
/// and copies the tree. `source` is the handle `prepare_destination` opened.
///
/// Returns `None` when the task was cancelled, in which case it has already
/// been finalized via `active.cancelled()`. Otherwise returns the task and
/// what the walk left behind, for the caller to finalize.
#[allow(clippy::too_many_arguments)]
fn copy_with_progress(
    old_path: &Path,
    new_path: &Path,
    mut active: ActiveTask,
    source: Option<File>,
    entry_size: u64,
    is_directory: bool,
    source_mode: u32,
    preserve_times: bool,
    conflicts: Option<&Conflicts>,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> Option<(ActiveTask, CopyOutcome)> {
    let total_size = if is_directory {
        let Some(size) = dir_total_size(&active, old_path) else {
            active.cancelled();
            return None;
        };
        active.set_total(size);
        size
    } else {
        entry_size
    };
    // One buffer for the whole tree. It is sized from the tree's total, so
    // allocating it per file would hand every small file in a large directory
    // its own multi-megabyte allocation.
    let mut buffer = vec![0; copy_buffer_bytes(total_size, buffer_min_bytes, buffer_max_bytes)];
    let mut context = CopyContext {
        buffer: &mut buffer,
        conflicts,
        preserve_times,
        source,
        skipped: 0,
    };
    let mut errors = Vec::new();
    if !copy_path(
        old_path,
        new_path,
        &mut active,
        &mut errors,
        &mut context,
        is_directory,
        source_mode,
    ) {
        active.cancelled();
        return None;
    }
    Some((
        active,
        CopyOutcome {
            errors,
            skipped: context.skipped,
        },
    ))
}

/// Best-effort recursive size for the progress total. Entries that cannot be
/// read are skipped here; the copy itself reports them as errors.
///
/// Returns `None` when the task was cancelled. The walk runs before any bytes
/// are copied and takes as long as the tree is large, so it observes the token
/// itself rather than leaving a cancel acknowledged but still running.
fn dir_total_size(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if active.is_cancelled() {
            return None;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            if active.is_cancelled() {
                return None;
            }
            // `DirEntry::metadata` does not follow symlinks and avoids a
            // second path lookup per entry.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if !metadata.is_symlink() {
                total += metadata.len();
            }
        }
    }
    Some(total)
}

/// Best-effort recursive entry count for the delete progress total, including
/// `root` itself. Reads directories only, with no per-entry stat, so it is
/// cheaper than `dir_total_size`; a directory that cannot be listed counts as
/// the single entry it is. Returns `None` when the task was cancelled.
fn dir_total_entries(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total = 1; // `root` itself, which appears in no listing.
    let mut stack = vec![root.to_path_buf()];
    while let Some(path) = stack.pop() {
        if active.is_cancelled() {
            return None;
        }
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            if active.is_cancelled() {
                return None;
            }
            total += 1;
            // `file_type` does not follow symlinks, matching the delete walk:
            // a link to a directory is unlinked rather than descended into.
            if entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    Some(total)
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

/// Opens and lists the directory `$name` in `$parent` for `remove_path`:
/// unwraps the directory and its entries, finalizes `$active` as cancelled and
/// returns `None` if the drain was cancelled, or as an error naming `$dir` and
/// returns `None` if the open or the read failed.
macro_rules! list_or_abort {
    ($active:expr, $parent:expr, $name:expr, $dir:expr) => {{
        let dir = $dir;
        match open_and_list(&$active, $parent, $name) {
            Ok(Some(listed)) => listed,
            Ok(None) => {
                $active.cancelled();
                return None;
            }
            Err(error) => {
                $active.error(format!(
                    "Failed to read directory {}: {error}",
                    compact(dir)
                ));
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
        .map_err(|error| anyhow!("Failed to {operation} {}: {error}", compact(old_path)).into())
}

/// Finishes a cross-device move once the copy stage is done, removing the
/// source only when the destination holds every entry of it, like `mv`.
///
/// A failed entry keeps the whole source, including the entries that copied,
/// and the partial destination. A skipped one does the same: it is no more at
/// the destination than a failed one, so removing the source would delete what
/// nothing else holds.
fn finish_cross_device_move(
    active: ActiveTask,
    outcome: CopyOutcome,
    old_path: &Path,
    is_directory: bool,
) {
    if !outcome.errors.is_empty() {
        finalize_copy(active, outcome.errors);
        return;
    }
    if outcome.skipped > 0 {
        let skipped = outcome.skipped;
        let entries = if skipped == 1 { "entry" } else { "entries" };
        active.error(format!(
            "Skipped {skipped} {entries}, so the original {} was kept",
            compact(old_path)
        ));
        return;
    }
    // Not cancellable: the copy is complete, so removing the source outright is
    // the only way to finish the move. Mark it so a cancel keypress during this
    // stage does not claim to have cancelled anything.
    active.set_uncancellable();
    let removed = if is_directory {
        fs::remove_dir_all(old_path)
    } else {
        fs::remove_file(old_path)
    };
    match removed {
        Ok(()) => active.done(),
        Err(error) => active.error(format!(
            "Copy succeeded, but failed to delete original {}: {error}",
            compact(old_path)
        )),
    }
}

/// Finalizes a copy/move task the way coreutils does: success when no per-entry
/// error was recorded, otherwise one alert summarizing them. Skipped entries are
/// not failures and do not appear. Every error is also logged.
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
        format!("{} (and {} more)", errors[0], errors.len() - 1)
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
    context: &mut CopyContext<'_>,
) -> bool {
    match create_directory(new_path) {
        Ok(()) => {}
        // Something took this name while the copy was running: the destination
        // was free when the task started, so this is another process writing
        // into the tree. A directory is never replaced, so only a standing
        // "skip all" settles this one, and skipping drops the subtree.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !resolve_nested(context, errors, new_path) {
                return true;
            }
            if let Err(error) = remove_existing(new_path).and_then(|()| create_directory(new_path))
            {
                errors.push(format!(
                    "Failed to create directory {}: {error}",
                    compact(new_path)
                ));
                return true;
            }
        }
        Err(error) => {
            // The subtree cannot be copied at all; skip it and continue with
            // the siblings.
            errors.push(format!(
                "Failed to create directory {}: {error}",
                compact(new_path)
            ));
            return true;
        }
    }

    // Applied on every exit, cancelled included, after the contents: a source
    // mode without owner-write (e.g. 0o555) would otherwise stop us creating
    // this directory's own children, and until then the directory is
    // owner-only (`create_directory`). Matches `cp`. Read from the source's
    // metadata rather than its entries, so it applies even when the read below
    // fails.
    let preserve_times = context.preserve_times;
    let source_mode = fs::symlink_metadata(old_path)
        .ok()
        .map(|metadata| metadata.permissions().mode());
    let apply_source_mode = || {
        // After the contents, for the same reason the mode is: writing the
        // children is what moved the directory's own modification time.
        if preserve_times {
            apply_times_to_path(old_path, new_path);
        }
        if let Some(mode) = source_mode {
            apply_permissions(mode, new_path);
        }
    };

    let entries = match fs::read_dir(old_path) {
        Ok(entries) => entries,
        Err(error) => {
            errors.push(format!(
                "Failed to read directory {}: {error}",
                compact(old_path)
            ));
            apply_source_mode();
            return true;
        }
    };

    for entry in entries {
        if active.is_cancelled() {
            // Like interrupted `cp`: leave the partially copied destination in
            // place rather than removing it.
            apply_source_mode();
            return false;
        }

        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!(
                    "Failed to read entry in {}: {error}",
                    compact(old_path)
                ));
                continue;
            }
        };

        let src = entry.path();
        let dst = new_path.join(entry.file_name());
        let metadata = match fs::symlink_metadata(&src) {
            Ok(metadata) => metadata,
            Err(error) => {
                errors.push(format!(
                    "Failed to read metadata for {}: {error}",
                    compact(&src)
                ));
                continue;
            }
        };

        // `symlink_metadata`, so the mode carries `S_IFLNK` for a symlink and
        // `copy_path` dispatches links, directories, and files off it alike.
        if !copy_path(
            &src,
            &dst,
            active,
            errors,
            context,
            metadata.is_dir(),
            metadata.permissions().mode(),
        ) {
            apply_source_mode();
            return false;
        }
    }

    apply_source_mode();
    true
}

/// Recreates the symlink at `old_path` at `new_path`, pointing at the same
/// (possibly relative, possibly dangling) target. The target is never followed,
/// so no bytes are transferred and no permissions are applied:
/// `fs::set_permissions` would chmod the target rather than the link.
fn copy_symlink(
    old_path: &Path,
    new_path: &Path,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) {
    let target = match fs::read_link(old_path) {
        Ok(target) => target,
        Err(error) => {
            errors.push(format!(
                "Failed to read symlink {}: {error}",
                compact(old_path)
            ));
            return;
        }
    };
    match std::os::unix::fs::symlink(&target, new_path) {
        Ok(()) => {}
        // Raced; settled from the standing answer, or recorded.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !resolve_nested(context, errors, new_path) {
                return;
            }
            if let Err(error) = remove_existing(new_path)
                .and_then(|()| std::os::unix::fs::symlink(&target, new_path))
            {
                errors.push(format!("Failed to replace {}: {error}", compact(new_path)));
            }
        }
        Err(error) => errors.push(format!(
            "Failed to create symlink {}: {error}",
            compact(new_path)
        )),
    }
}

/// Copies a file chunk-by-chunk, sending debounced progress updates via
/// `active`. The destination is created owner-only (`create_file`) and gets
/// `source_mode`'s permissions once the copy stops, however it stops, so a
/// failed or cancelled copy is never readable by more users than the source.
/// Failures are recorded in `errors`; returns `false` only when cancelled.
fn copy_file(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    source_mode: u32,
) -> bool {
    let total_size = active.total_size();
    let opened = match context.source.take() {
        Some(file) => Ok(file),
        None => File::open(old_path),
    };
    let mut old_file = match opened {
        Ok(file) => file,
        Err(error) => {
            errors.push(format!(
                "Failed to copy {} to {}: {error}",
                compact(old_path),
                compact(new_path)
            ));
            return true;
        }
    };
    let mut new_file = match create_file(new_path, source_mode) {
        Ok(file) => file,
        // A name already taken inside the tree being copied. The top-level
        // collision was answered before the task started, so this one is a
        // race, settled from the paste's standing answer or recorded.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !resolve_nested(context, errors, new_path) {
                return true;
            }
            match remove_existing(new_path).and_then(|()| create_file(new_path, source_mode)) {
                Ok(file) => file,
                Err(error) => {
                    errors.push(format!("Failed to replace {}: {error}", compact(new_path)));
                    return true;
                }
            }
        }
        Err(error) => {
            errors.push(format!(
                "Failed to copy {} to {}: {error}",
                compact(old_path),
                compact(new_path)
            ));
            return true;
        }
    };

    let mut debouncer = debounce::ProgressDebouncer::new(
        PROGRESS_DEBOUNCE_PERCENTAGE,
        PROGRESS_MIN_INTERVAL,
        total_size,
    );

    let not_cancelled = loop {
        if active.is_cancelled() {
            // Like interrupted `cp`: leave the partially written destination
            // file in place rather than removing it.
            break false;
        }

        match old_file.read(context.buffer) {
            Ok(0) => {
                // Before `new_file` is dropped: writing is what moves the
                // modification time, so it has to be restored once the last
                // byte is written.
                if context.preserve_times {
                    apply_times(old_path, &new_file);
                }
                break true;
            }
            Ok(bytes) => match new_file.write_all(&context.buffer[..bytes]) {
                Ok(()) => {
                    active.increment(bytes as u64);
                    if debouncer.should_trigger(Instant::now(), bytes as u64) {
                        active.send_progress();
                    }
                }
                Err(error) => {
                    errors.push(format!("Failed to write {}: {error}", compact(new_path)));
                    break true;
                }
            },
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", compact(old_path)));
                break true;
            }
        }
    };
    // Through the open handle, so a path swapped since it was created cannot
    // redirect the change.
    if let Err(error) = new_file.set_permissions(fs::Permissions::from_mode(source_mode & 0o7777)) {
        warn!(
            "Failed to set permissions on {}: {error}",
            new_path.display()
        );
    }
    not_cancelled
}

/// Copies a directory, file, symlink, or special file, dispatching on
/// `is_directory` and `source_mode`. Per-entry failures accumulate in
/// `errors`; returns `false` only when the task was cancelled.
fn copy_path(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    is_directory: bool,
    source_mode: u32,
) -> bool {
    // Check symlink first: `source_mode` comes from `symlink_metadata`, so a
    // symlink (even one pointing at a directory) is recreated as a link rather
    // than followed. `is_directory` is already false for any symlink.
    if unix_mode::is_symlink(source_mode) {
        copy_symlink(old_path, new_path, errors, context);
        true
    } else if is_directory {
        copy_directory(old_path, new_path, active, errors, context)
    } else if unix_mode::is_file(source_mode) {
        copy_file(old_path, new_path, active, errors, context, source_mode)
    } else {
        copy_special(old_path, new_path, errors, context, source_mode);
        true
    }
}

/// Recreates a special file (FIFO, socket, or device node) as a fresh node at
/// `new_path` with the source's permission bits, like `cp -R` does. No bytes
/// are transferred: reading a FIFO would block until a writer appears. FIFOs
/// and sockets need no privileges; device nodes require root, so as a normal
/// user they record a "not permitted" error here, exactly as `cp` reports.
fn copy_special(
    old_path: &Path,
    new_path: &Path,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    source_mode: u32,
) {
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
        errors.push(format!(
            "Cannot copy {}: unsupported file type",
            compact(old_path)
        ));
        return;
    };

    // Device nodes need the source's device numbers; zero for the rest.
    let rdev = if matches!(kind, SFlag::S_IFBLK | SFlag::S_IFCHR) {
        use std::os::unix::fs::MetadataExt;
        match fs::symlink_metadata(old_path) {
            // `MetadataExt::rdev` widens the platform's `dev_t` (i32 on
            // macOS) to u64, so narrowing it back loses nothing.
            #[allow(clippy::cast_possible_truncation)]
            Ok(metadata) => metadata.rdev() as nix::libc::dev_t,
            Err(error) => {
                errors.push(format!(
                    "Failed to read metadata for {}: {error}",
                    compact(old_path)
                ));
                return;
            }
        }
    } else {
        0
    };

    // `mode_t` is u32 on Linux but u16 on macOS, so cast rather than assume.
    // The permission bits `from_bits_truncate` keeps fit in either.
    #[allow(clippy::cast_possible_truncation)]
    let permissions = Mode::from_bits_truncate(source_mode as nix::libc::mode_t);
    match mknod(new_path, kind, permissions, rdev) {
        Ok(()) => {}
        // Raced; settled from the standing answer, or recorded.
        Err(nix::errno::Errno::EEXIST) => {
            if !resolve_nested(context, errors, new_path) {
                return;
            }
            if let Err(error) = remove_existing(new_path)
                .map_err(|error| error.to_string())
                .and_then(|()| {
                    mknod(new_path, kind, permissions, rdev).map_err(|error| error.to_string())
                })
            {
                errors.push(format!("Failed to replace {}: {error}", compact(new_path)));
            }
        }
        Err(error) => errors.push(format!(
            "Failed to create special file {}: {error}",
            compact(new_path)
        )),
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
fn remove_path(path: &Path, is_directory: bool, mut active: ActiveTask) -> Option<ActiveTask> {
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
            format!("Failed to delete {}", compact(path))
        );
        active.increment(1);
        return Some(active);
    }

    // Post-order walk, each directory drained into a Vec before anything is
    // deleted: nothing is unlinked while a directory is still being read, which
    // on some filesystems (NFS) can skip entries. The trade is peak memory,
    // which is proportional to the widest root-to-leaf path rather than O(1)
    // per level. A directory is removed once its entries are done, so a
    // cancelled or failed delete leaves each subtree either fully removed or
    // intact.
    //
    // Every open and unlink is relative to the parent directory's fd, and no
    // directory is opened through a symlink, so a directory swapped for a link
    // after it was listed fails to open instead of leading the walk outside the
    // tree.
    //
    // Only the directory being worked in holds an fd, so depth is not bounded
    // by the open-file limit. Descending closes the parent's fd; returning
    // reopens it through the child's "..", which is never a symlink, and
    // refuses to continue unless it is the same directory (device and inode)
    // that was listed, since the child may have been moved in between.
    let (dir, entries) = list_or_abort!(active, None, path, path);
    let id = try_or_abort!(
        active,
        DirId::of(&dir),
        format!("Failed to read directory {}", compact(path))
    );
    let mut stack = vec![Level {
        dir: Some(dir),
        id,
        name: None,
        path: path.to_path_buf(),
        entries: entries.into_iter(),
    }];
    // One unit of progress per entry removed, against the total counted by
    // `dir_total_entries` before the walk. Debounced so a wide tree does not
    // put one progress command per entry ahead of terminal input.
    let mut debouncer = debounce::ProgressDebouncer::new(
        PROGRESS_DEBOUNCE_PERCENTAGE,
        PROGRESS_MIN_INTERVAL,
        active.total_size(),
    );
    while let Some(top) = stack.last_mut() {
        if active.is_cancelled() {
            active.cancelled();
            return None;
        }
        match top.entries.next() {
            // This directory's entries are done; remove it.
            None => {
                let level = stack.pop().expect("stack is non-empty");
                if let Err((path, error)) = remove_level(&mut stack, level) {
                    active.error(format!("Failed to delete {}: {error}", compact(&path)));
                    return None;
                }
            }
            Some((name, true)) => {
                let entry_path = top.path.join(OsStr::from_bytes(name.to_bytes()));
                let parent = top
                    .dir
                    .as_ref()
                    .expect("the level being worked in holds its fd");
                let (dir, entries) = list_or_abort!(active, Some(parent), &name, &entry_path);
                let id = try_or_abort!(
                    active,
                    DirId::of(&dir),
                    format!("Failed to read directory {}", compact(&entry_path))
                );
                // Closed until the walk returns here, through `reopen_parent`.
                top.dir = None;
                stack.push(Level {
                    dir: Some(dir),
                    id,
                    name: Some(name),
                    path: entry_path,
                    entries: entries.into_iter(),
                });
                // Descending is not a removal, so it advances no progress.
                continue;
            }
            Some((name, false)) => {
                let entry_path = top.path.join(OsStr::from_bytes(name.to_bytes()));
                try_or_abort!(
                    active,
                    unlink(
                        top.dir
                            .as_ref()
                            .expect("the level being worked in holds its fd"),
                        &name,
                        AtFlags::empty()
                    ),
                    format!("Failed to delete {}", compact(&entry_path))
                );
            }
        }
        active.increment(1);
        if debouncer.should_trigger(Instant::now(), 1) {
            active.send_progress();
        }
    }
    Some(active)
}

/// Removes the directory `level` names, now that its entries are gone, from
/// the parent at the top of `stack`, reopening that parent's fd first. The
/// error carries the directory that could not be reopened or removed.
fn remove_level(stack: &mut [Level], level: Level) -> Result<(), (PathBuf, std::io::Error)> {
    let Level {
        dir, name, path, ..
    } = level;
    let Some(parent) = stack.last_mut() else {
        drop(dir);
        // The root, named by the path the user chose. `rmdir` never follows a
        // symlink in the last component.
        return fs::remove_dir(&path).map_err(|error| (path, error));
    };
    let child = dir
        .as_ref()
        .expect("the level being worked in holds its fd");
    let reopened = reopen_parent(child, parent.id).map_err(|error| (parent.path.clone(), error))?;
    drop(dir);
    let name = name.expect("only the root has no name");
    unlink(&reopened, &name, AtFlags::REMOVEDIR).map_err(|error| (path, error))?;
    parent.dir = Some(reopened);
    Ok(())
}

/// A directory's entries as `remove_path` lists them: each name, and whether it
/// is a directory to descend into.
type Entries = Vec<(CString, bool)>;

/// One directory on `remove_path`'s stack: the open directory its entries are
/// unlinked through (`None` while the walk is below it), its identity, its name
/// in the parent (`None` for the root), its path for messages, and the entries
/// not yet removed.
struct Level {
    dir: Option<Dir>,
    id: DirId,
    name: Option<CString>,
    path: PathBuf,
    entries: <Entries as IntoIterator>::IntoIter,
}

/// The device and inode of a directory, to tell whether a directory reopened
/// by name is the one that was listed.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DirId {
    dev: rustix::fs::Dev,
    ino: u64,
}

impl DirId {
    fn of(dir: &Dir) -> std::io::Result<Self> {
        let stat = rustix::fs::fstat(dir.fd()?)?;
        Ok(Self {
            dev: stat.st_dev,
            ino: stat.st_ino,
        })
    }
}

/// Reopens the parent of `dir` through its "..", refusing it unless it is the
/// directory `expected` identifies: a directory moved elsewhere during the walk
/// has a different "..", and following it would delete outside the tree.
fn reopen_parent(dir: &Dir, expected: DirId) -> std::io::Result<Dir> {
    let parent = open_directory(dir.fd()?, "..")?;
    if DirId::of(&parent)? != expected {
        return Err(std::io::Error::other(
            "it was moved while its contents were being deleted",
        ));
    }
    Ok(parent)
}

/// Opens the directory `name` in `parent` without following a symlink:
/// `O_NOFOLLOW` with `O_DIRECTORY` refuses a link in the last component (ENOTDIR
/// on Linux, ELOOP elsewhere), and `O_DIRECTORY` anything else that is not a
/// directory.
fn open_directory(parent: impl AsFd, name: impl rustix::path::Arg) -> std::io::Result<Dir> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let fd = openat(parent, name, flags, Mode::empty())?;
    Ok(Dir::new(fd)?)
}

/// `open_directory` then `list_entries`. A `None` parent resolves `name`
/// against the current directory, like a path.
fn open_and_list(
    active: &ActiveTask,
    parent: Option<&Dir>,
    name: impl rustix::path::Arg,
) -> std::io::Result<Option<(Dir, Entries)>> {
    let parent = match parent {
        Some(parent) => parent.fd()?,
        None => CWD,
    };
    let mut dir = open_directory(parent, name)?;
    Ok(list_entries(active, &mut dir)?.map(|entries| (dir, entries)))
}

/// Unlinks `name` in `dir`: a file or symlink with no `flags`, an empty
/// directory with `AtFlags::REMOVEDIR`. Neither follows a symlink.
fn unlink(dir: &Dir, name: &CStr, flags: AtFlags) -> std::io::Result<()> {
    Ok(unlinkat(dir.fd()?, name, flags)?)
}

/// Collects `(name, is_directory)` for each entry of `dir`, read to the end
/// before the caller deletes anything. The type comes from the directory entry,
/// or from an `lstat` where the filesystem does not report one, so a link to a
/// directory reports `false` and is unlinked rather than descended into.
/// Checked for cancellation once per entry, so a huge directory does not delay
/// a cancel; returns `Ok(None)` when cancelled.
fn list_entries(active: &ActiveTask, dir: &mut Dir) -> std::io::Result<Option<Entries>> {
    let mut entries = Vec::new();
    while let Some(entry) = dir.read() {
        if active.is_cancelled() {
            return Ok(None);
        }
        let entry = entry?;
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        let file_type = match entry.file_type() {
            FileType::Unknown => {
                FileType::from_raw_mode(statat(dir.fd()?, name, AtFlags::SYMLINK_NOFOLLOW)?.st_mode)
            }
            file_type => file_type,
        };
        entries.push((name.to_owned(), file_type == FileType::Directory));
    }
    Ok(Some(entries))
}

/// Copies `source`'s access and modification times onto an already-open
/// `target`. Best effort: a filesystem that cannot record them is not a reason
/// to fail the operation.
fn apply_times(source: &Path, target: &File) {
    let Ok(metadata) = fs::metadata(source) else {
        return;
    };
    let mut times = fs::FileTimes::new();
    if let Ok(accessed) = metadata.accessed() {
        times = times.set_accessed(accessed);
    }
    if let Ok(modified) = metadata.modified() {
        times = times.set_modified(modified);
    }
    if let Err(error) = target.set_times(times) {
        warn!("Failed to set times on {}: {error}", compact(source));
    }
}

/// The same, for a path that is not already open. Opening a directory
/// read-only is enough to set its times when the caller owns it.
fn apply_times_to_path(source: &Path, target: &Path) {
    if let Ok(file) = File::open(target) {
        apply_times(source, &file);
    }
}

fn apply_permissions(mode: u32, path: &Path) {
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        warn!("Failed to set permissions on {}: {e}", path.display());
    }
}

/// Renames `old_path` to `new_path`, failing atomically with `AlreadyExists` if
/// `new_path` is taken, unlike `fs::rename`, which replaces it. `validate_paths`
/// rejects an existing destination already, but on the UI thread before the
/// worker starts; folding the check into the rename closes the window where one
/// appears in between.
///
/// Linux uses `renameat2(RENAME_NOREPLACE)` and macOS `renameatx_np`, both
/// through rustix's safe wrapper, since `unsafe` is denied crate-wide. Other
/// targets fall back to `fs::rename` and keep the narrow race.
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

/// Creates the destination of a file copy. `create_new` (O_EXCL|O_CREAT) fails
/// atomically if the target exists, closing the same window as
/// `rename_no_replace`; without it a file that appeared since `validate_paths`
/// ran would be truncated.
///
/// Created with the source's owner bits only, like `cp`, so the partly written
/// file is readable by no one the source is not; `copy_file` applies the full
/// mode when it stops. Owner read-write is added whatever the source says, or
/// the copy could not write it.
fn create_file(target: &Path, source_mode: u32) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode((source_mode & 0o700) | 0o600)
        .open(target)
}

/// Creates the destination of a directory copy owner-only, for the same reason
/// as `create_file`. Owner-writable and searchable so its children can be
/// created; `copy_directory` applies the source's mode when it stops.
fn create_directory(target: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(target)
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

/// The path with symlinks and `..` components resolved. Falls back to a lexical
/// absolutize-and-normalize when the path cannot be resolved, which is the
/// normal case for a destination that does not exist yet.
fn resolve(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| {
        lexical_normalize(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
    })
}

/// The entry `path` names, with symlinks resolved in its parent directories
/// but not in its own name: a symlink is compared as itself, never as the file
/// it points at, which is a different entry.
pub(super) fn resolve_entry(path: &Path) -> PathBuf {
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            let parent = if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            };
            resolve(parent).join(name)
        }
        _ => resolve(path),
    }
}

/// Whether both paths name one file: the same device and inode, without
/// following a symlink in either.
pub(super) fn is_same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.symlink_metadata(), b.symlink_metadata()) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

/// Whether `link` is a symlink that resolves to the entry `entry` names.
pub(super) fn is_link_to(link: &Path, entry: &Path) -> bool {
    link.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_symlink())
        && link
            .canonicalize()
            .is_ok_and(|target| target == resolve_entry(entry))
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
    overwrite: bool,
) -> Result<(PathBuf, PathBuf), CommandResult> {
    let old_path = source.path.clone();
    // Join the source's raw `OsStr` file name rather than its display name:
    // the display name is lossy UTF-8, which would silently mangle a non-UTF8
    // name at the destination.
    let Some(file_name) = source.path.file_name() else {
        return Err(anyhow!(
            "Cannot {operation} {}: path has no file name",
            compact(&old_path)
        )
        .into());
    };
    let new_path = destination_directory.path.join(file_name);

    // Compare resolved paths so that neither a parent-dir segment (e.g.
    // `/a/c/../b`) nor a destination directory that is a symlink to the
    // source's own directory can disguise one path as another. The
    // destination's own name does not exist yet, so its parent is resolved and
    // the name rejoined. The source is resolved the same way, so a symlink is
    // compared as the link it is, not as the entry it points at.
    let abs_old = resolve_entry(&old_path);
    let abs_new = resolve(&destination_directory.path).join(file_name);

    // The two paths name the same entry. This has to be caught here whatever
    // the source's type: a granted overwrite clears the destination before
    // copying, so letting an aliased path through would unlink the source and
    // leave nothing to copy.
    if abs_old == abs_new {
        // Equal resolved paths mean the destination directory is the source's
        // own, so the message names the entry once.
        return Err(anyhow!(
            "Cannot {operation} {} into its own directory",
            compact(&old_path)
        )
        .into());
    }

    // Two names of one file (hard links, or one entry spelled two ways on a
    // case-insensitive mount): replacing one with the other either deletes the
    // file or does nothing, depending on how the names relate. `cp` and `mv`
    // refuse both as the same file.
    if is_same_file(&old_path, &new_path) {
        return Err(anyhow!(
            "Cannot {operation} {} to {}: they are the same file",
            compact(&old_path),
            compact(&new_path)
        )
        .into());
    }

    // A symlink pasted over the entry it points at would replace that entry,
    // the only copy of its data, with a link to itself. `cp` and `mv` refuse
    // the same paste as the same file.
    if is_link_to(&old_path, &abs_new) {
        return Err(anyhow!(
            "Cannot {operation} {}: it links to {}, the entry it would replace",
            compact(&old_path),
            compact(&new_path)
        )
        .into());
    }

    // Without this a copy creates the destination under the source and recurses
    // into it forever, filling the disk. Only a real directory can: `copy_path`
    // recreates a symlink as a link and copies a file as bytes, so neither
    // descends into what it is creating. Restricting the check to directories
    // also keeps it from refusing a symlink copied into the directory it points
    // at.
    if source.is_directory() && abs_new.starts_with(&abs_old) {
        return Err(anyhow!(
            "Cannot {operation} {} into its own subdirectory {}",
            compact(&old_path),
            compact(&new_path)
        )
        .into());
    }

    // Refuse to replace an existing destination unless the paste asked for it:
    // `File::create`/`fs::rename` would otherwise do so silently. An existing
    // directory is never replaced whatever was asked, because removing it would
    // take its contents with it and merging into it is not supported.
    match new_path.symlink_metadata() {
        // Both messages name the destination directory rather than the full
        // destination path: it differs from the source only in its directory,
        // so repeating the file name says nothing.
        Ok(metadata) if metadata.is_dir() => Err(anyhow!(
            "Cannot {operation} {} into {}: a directory of that name is already there",
            compact(&old_path),
            compact(&destination_directory.path)
        )
        .into()),
        Ok(_) if !overwrite => Err(anyhow!(
            "Cannot {operation} {} into {}: it already exists there",
            compact(&old_path),
            compact(&destination_directory.path)
        )
        .into()),
        _ => Ok((old_path, new_path)),
    }
}

/// What to do about `new_path`, a name another process took at a destination
/// inside the tree being copied: the destination was free when the task
/// started. Returns whether to replace it, having counted or recorded it
/// otherwise.
///
/// The same decision the queue makes for a top-level collision, minus the one
/// outcome a worker cannot produce: it never asks, so a collision the paste's
/// standing answer does not settle is recorded like any other entry that could
/// not be written.
fn resolve_nested(
    context: &mut CopyContext<'_>,
    errors: &mut Vec<String>,
    new_path: &Path,
) -> bool {
    // A directory is never replaced, so only the skip choices apply to one,
    // exactly as at the top level.
    let occupant = if new_path.symlink_metadata().is_ok_and(|it| it.is_dir()) {
        Occupant::Directory
    } else {
        Occupant::Replaceable
    };
    let standing = context.conflicts.and_then(Conflicts::standing);
    match step(standing, Some(occupant)) {
        PasteStep::Run { overwrite } => overwrite,
        PasteStep::Skip => {
            context.skipped += 1;
            false
        }
        PasteStep::Ask { .. } => {
            errors.push(format!("{} already exists", compact(new_path)));
            false
        }
    }
}

/// Removes an existing non-directory destination, treating an already-absent
/// path as success: the entry the user agreed to replace may have been removed
/// by something else in the meantime, which is not a reason to fail the paste.
fn remove_existing(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Honors a cancel that landed while the task sat in the queue, which can be a
/// long time: a single worker runs every operation in turn. Returns `None` when
/// the task was finalized here and must not continue.
fn check_cancelled(active: ActiveTask) -> Option<ActiveTask> {
    if active.is_cancelled() {
        active.cancelled();
        return None;
    }
    Some(active)
}

/// Opens a regular-file source, then clears a destination the paste granted
/// permission to replace. `verb` names the operation in the error, "copy" or
/// "move". Returns the task and the opened source, or `None`
/// when the task was finalized here and must not continue.
///
/// Opening first means a source that cannot be read (mode 000, or gone) fails
/// the task with the destination still in place, and the copy then reads the
/// handle that was checked rather than reopening the path. Other types are not
/// opened: a FIFO would block, and a directory or symlink is not read as bytes.
fn prepare_destination(
    active: ActiveTask,
    verb: &str,
    old_path: &Path,
    new_path: &Path,
    overwrite: bool,
    source_mode: u32,
) -> Option<(ActiveTask, Option<File>)> {
    let source = if unix_mode::is_file(source_mode) {
        match File::open(old_path) {
            Ok(file) => Some(file),
            Err(error) => {
                active.error(format!(
                    "Failed to {verb} {} to {}: {error}",
                    compact(old_path),
                    compact(new_path)
                ));
                return None;
            }
        }
    } else {
        None
    };
    clear_destination(active, old_path, new_path, overwrite).map(|active| (active, source))
}

/// Clears a destination the paste granted permission to replace. Returns `None`
/// when the task was finalized here and must not continue.
///
/// Runs in the worker, once the operation is about to write. The caller queues
/// every source of an "overwrite all" paste in one pass, so clearing there would
/// delete every destination up front and leave a hole wherever a later task is
/// cancelled or fails.
///
/// The source is re-checked first for the same reason: removing the destination
/// for a copy that then finds nothing to read would leave neither entry. It
/// narrows the window rather than closing it.
fn clear_destination(
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    overwrite: bool,
) -> Option<ActiveTask> {
    if !overwrite {
        return Some(active);
    }
    if let Err(error) = old_path.symlink_metadata() {
        active.error(format!(
            "Failed to replace {}: failed to read {}: {error}",
            compact(new_path),
            compact(old_path)
        ));
        return None;
    }
    if let Err(error) = remove_existing(new_path) {
        active.error(format!("Failed to replace {}: {error}", compact(new_path)));
        return None;
    }
    Some(active)
}

/// Renames `old_path` onto `new_path`, replacing an existing destination when
/// `overwrite`.
///
/// Prefers the kernel's atomic replace: no window with the destination missing,
/// and a rename that fails for an unrelated reason (a vanished source, a
/// permission error) leaves it untouched rather than destroyed for nothing. The
/// destination is cleared only for the case the kernel refuses outright,
/// replacing a non-directory with a directory.
fn rename_for_move(old_path: &Path, new_path: &Path, overwrite: bool) -> std::io::Result<()> {
    if !overwrite {
        return rename_no_replace(old_path, new_path);
    }
    match fs::rename(old_path, new_path) {
        Err(error) if error.kind() == ErrorKind::NotADirectory => {
            remove_existing(new_path)?;
            fs::rename(old_path, new_path)
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use test_case::test_case;

    use super::*;
    use crate::{
        command::{ConflictChoice, progress::Progress},
        test_support::TempDir,
    };

    // (len, min, max) -> expected buffer size. With BUFFER_SIZE_DIVISOR == 20.
    #[test_case(0, 10, 100 => 0 ; "zero length")]
    #[test_case(5, 10, 100 => 5 ; "below min uses len")]
    #[test_case(10, 10, 100 => 10 ; "equal to min uses len")]
    #[test_case(2000, 10, 100 => 100 ; "at max*divisor uses max")]
    #[test_case(5000, 10, 100 => 100 ; "above max*divisor uses max")]
    #[test_case(400, 10, 100 => 20 ; "mid range uses len/divisor")]
    #[test_case(30, 10, 100 => 10 ; "mid range floored at min")]
    fn buffer_bytes_scales_with_the_length_between_min_and_max(
        len: u64,
        min: u64,
        max: u64,
    ) -> usize {
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
        assert!(u64::try_from(MIN_COPY_BUFFER_BYTES).unwrap() > max);
        assert_eq!(
            usize::try_from(max).unwrap(),
            copy_buffer_bytes(0, max, max)
        );
    }

    /// A source built from `/`, so it reports as a directory: the subdirectory
    /// check below only applies to one, and every test using this helper is
    /// about a path rule rather than about the source's type.
    fn path_info(path: &str, basename: &str) -> PathInfo {
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.path = PathBuf::from(path);
        info.display_name = basename.to_string();
        info
    }

    /// The message a refusal carried. `validate_paths` has five separate
    /// reasons to refuse, so `is_err` alone cannot tell whether a fixture
    /// reached the rule it was built for.
    fn rejection(result: Result<(PathBuf, PathBuf), CommandResult>) -> String {
        let refusal = result.expect_err("the paths should have been refused");
        match Command::try_from(refusal) {
            Ok(Command::AlertError(message)) => message,
            other => panic!("expected an AlertError, got {other:?}"),
        }
    }

    #[test]
    fn validate_paths_rejects_identical_source_and_destination() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a", "a");
        let message = rejection(validate_paths(&src, &dest, "copy", false));
        assert!(message.ends_with("into its own directory"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_destination_inside_source() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/b/c", "c");
        let message = rejection(validate_paths(&src, &dest, "copy", false));
        assert!(message.contains("into its own subdirectory"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_destination_inside_source_via_parent_dir() {
        // "/a/c/../b/d" resolves to "/a/b/d", inside the source, which a raw
        // component-wise prefix check on the path as written would not catch.
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/c/../b/d", "d");
        let message = rejection(validate_paths(&src, &dest, "copy", false));
        assert!(message.contains("into its own subdirectory"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_a_destination_that_symlinks_into_the_source() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(src.join("inner")).unwrap();
        // A lexical prefix check cannot see that `link` resolves inside `src`.
        // Copying through it would create the destination under the source and
        // then recurse into what it is creating, filling the disk.
        let link = fx.join("link");
        std::os::unix::fs::symlink(src.join("inner"), &link).unwrap();

        let source = path_info(src.to_str().unwrap(), "src");
        let dest = path_info(link.to_str().unwrap(), "link");
        let message = rejection(validate_paths(&source, &dest, "copy", false));
        assert!(message.contains("into its own subdirectory"), "{message}");
    }

    #[test]
    fn validate_paths_allows_a_symlink_copied_into_the_directory_it_points_at() {
        let fx = TempDir::new("tasks");
        let target = fx.join("target");
        std::fs::create_dir_all(&target).unwrap();
        let link = fx.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        // The source resolves to the destination, but copying a symlink
        // recreates a link rather than descending into anything, so there is
        // no subtree to recurse into and nothing to reject.
        let source = PathInfo::try_from(link.as_path()).unwrap();
        let dest = PathInfo::try_from(target.as_path()).unwrap();
        assert!(validate_paths(&source, &dest, "copy", false).is_ok());
    }

    #[test_case(true  ; "overwrite granted")]
    #[test_case(false ; "overwrite not granted")]
    fn validate_paths_rejects_a_destination_that_aliases_the_source(overwrite: bool) {
        let fx = TempDir::new("tasks");
        let real = fx.join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("f.txt"), b"precious").unwrap();
        // A second path to the same directory, which the clipboard can easily
        // carry: it holds whatever absolute path the other window was showing.
        std::os::unix::fs::symlink(&real, fx.join("link")).unwrap();

        let src = PathInfo::try_from(real.join("f.txt").as_path()).unwrap();
        let dest = PathInfo::try_from(fx.join("link").as_path()).unwrap();

        // The two paths name one file. A granted overwrite clears the
        // destination before copying, so letting this through would unlink the
        // source and leave nothing to copy from. Without a granted overwrite
        // the existing-destination rule refuses the same paste, so the message
        // is what says the alias check is the one that fired.
        let message = rejection(validate_paths(&src, &dest, "copy", overwrite));
        assert!(message.ends_with("into its own directory"), "{message}");
        assert!(real.join("f.txt").exists());
    }

    /// `a/foo` is a symlink. Its own name is what is compared, so a paste into
    /// `b` meets `b/foo`, not the source's own directory, and whether that is a
    /// collision or a refusal depends on where the link points.
    fn link_pasted_into(label: &str, target: &str) -> (TempDir, PathInfo, PathInfo) {
        let fx = TempDir::new(label);
        let (a, b, c) = (fx.join("a"), fx.join("b"), fx.join("c"));
        for dir in [&a, &b, &c] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(b.join("foo"), b"data").unwrap();
        std::fs::write(c.join("foo"), b"other").unwrap();
        std::os::unix::fs::symlink(fx.join(target).join("foo"), a.join("foo")).unwrap();
        let src = PathInfo::try_from(a.join("foo").as_path()).unwrap();
        let dest = PathInfo::try_from(b.as_path()).unwrap();
        (fx, src, dest)
    }

    #[test]
    fn validate_paths_refuses_a_symlink_pasted_over_the_entry_it_points_at() {
        let (fx, src, dest) = link_pasted_into("tasks_link_over_target", "b");

        // Even with the overwrite granted: replacing `b/foo` with the link
        // would leave a link to itself and lose the only copy of the data.
        let message = rejection(validate_paths(&src, &dest, "copy", true));

        assert!(message.ends_with("the entry it would replace"), "{message}");
        assert_eq!(
            b"data".to_vec(),
            std::fs::read(fx.join("b").join("foo")).unwrap()
        );
    }

    #[test_case("copy" ; "a copy")]
    #[test_case("move" ; "a move")]
    fn validate_paths_refuses_a_hard_link_of_the_source(operation: &str) {
        let fx = TempDir::new("tasks_hard_link");
        let (a, b) = (fx.join("a"), fx.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("f"), b"data").unwrap();
        std::fs::hard_link(a.join("f"), b.join("f")).unwrap();
        let src = PathInfo::try_from(a.join("f").as_path()).unwrap();
        let dest = PathInfo::try_from(b.as_path()).unwrap();

        let message = rejection(validate_paths(&src, &dest, operation, true));

        assert!(message.ends_with("they are the same file"), "{message}");
    }

    #[test]
    fn validate_paths_treats_a_symlink_to_another_file_as_an_ordinary_collision() {
        let (_fx, src, dest) = link_pasted_into("tasks_link_elsewhere", "c");

        let message = rejection(validate_paths(&src, &dest, "copy", false));

        assert!(message.ends_with("already exists there"), "{message}");
    }

    #[test]
    fn validate_paths_allows_sibling_destination() {
        let src = path_info("/a/b", "b");
        let dest = path_info("/x", "x");
        let (old_path, new_path) =
            validate_paths(&src, &dest, "copy", false).expect("should be allowed");
        assert_eq!(PathBuf::from("/a/b"), old_path);
        assert_eq!(PathBuf::from("/x/b"), new_path);
    }

    #[test]
    fn validate_paths_allows_destination_with_shared_prefix_but_different_component() {
        // "/a/bb" must not be treated as inside "/a/b".
        let src = path_info("/a/b", "b");
        let dest = path_info("/a/bb", "bb");
        assert!(validate_paths(&src, &dest, "copy", false).is_ok());
    }

    #[test]
    fn validate_paths_preserves_non_utf8_source_names() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        // The display name is lossy UTF-8; the destination must be built from
        // the raw file name so the bytes survive.
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        let mut src = path_info("/a/placeholder", "placeholder");
        src.path = PathBuf::from("/a").join(name);
        let dest = path_info("/x", "x");
        let (_, new_path) = validate_paths(&src, &dest, "copy", false).expect("should be allowed");
        assert_eq!(PathBuf::from("/x").join(name), new_path);
    }

    #[test]
    fn validate_paths_rejects_source_without_file_name() {
        let src = path_info("/", "");
        let dest = path_info("/x", "x");
        let message = rejection(validate_paths(&src, &dest, "copy", false));
        assert!(message.ends_with("path has no file name"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_existing_destination() {
        let fx = TempDir::new("tasks");
        std::fs::write(fx.join("existing.txt"), b"x").unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.path().to_str().unwrap(), "dir");
        let message = rejection(validate_paths(&src, &dest, "copy", false));
        assert!(message.ends_with("it already exists there"), "{message}");
    }

    #[test]
    fn validate_paths_rejects_a_destination_directory_of_the_same_name() {
        let fx = TempDir::new("tasks");
        std::fs::create_dir_all(fx.join("existing.txt")).unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.path().to_str().unwrap(), "dir");

        // A directory is refused whatever the answer: removing it would take
        // its contents with it, and merging is not supported.
        for overwrite in [false, true] {
            let message = rejection(validate_paths(&src, &dest, "copy", overwrite));
            assert!(
                message.ends_with("a directory of that name is already there"),
                "{message}"
            );
        }
    }

    #[test]
    fn validate_paths_rejects_existing_broken_symlink_destination() {
        let fx = TempDir::new("tasks");
        let link = fx.join("existing.txt");
        std::os::unix::fs::symlink(fx.join("missing"), &link).unwrap();
        let src = path_info("/elsewhere/existing.txt", "existing.txt");
        let dest = path_info(fx.path().to_str().unwrap(), "dir");

        // `symlink_metadata` rather than `exists`, which follows the link and
        // reports a dangling one as absent, silently overwriting it.
        let message = rejection(validate_paths(&src, &dest, "copy", false));
        assert!(message.ends_with("it already exists there"), "{message}");
    }

    /// A copy context for a test: no paste behind it, so a nested collision
    /// records an error rather than asking.
    fn context(preserve_times: bool, buffer: &mut [u8]) -> CopyContext<'_> {
        CopyContext {
            buffer,
            conflicts: None,
            preserve_times,
            source: None,
            skipped: 0,
        }
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

    // Linux only: recreating a socket goes through mknod, which macOS refuses
    // to an unprivileged process with EPERM.
    #[cfg(target_os = "linux")]
    #[test]
    fn copy_path_recreates_socket() {
        let fx = TempDir::new("tasks");
        let src = fx.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&src).unwrap();
        let dst = fx.join("sock_copy");
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
            &mut context(false, &mut [0u8; 64]),
            false,
            mode,
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
        let fx = TempDir::new("tasks");
        let src = fx.join("fifo");
        nix::unistd::mkfifo(&src, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
        let dst = fx.join("fifo_copy");
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
            &mut context(false, &mut [0u8; 64]),
            false,
            mode,
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
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        std::fs::write(src.join("bad"), b"x").unwrap();
        std::fs::write(src.join("c.txt"), b"c").unwrap();
        let mut permissions = std::fs::metadata(src.join("bad")).unwrap().permissions();
        permissions.set_mode(0o000);
        std::fs::set_permissions(src.join("bad"), permissions).unwrap();
        // A chmod-000 file is still readable by root (CAP_DAC_OVERRIDE) and on
        // mounts that ignore permissions. Probe what this filesystem actually
        // does rather than inspecting the euid, so the assertions below match
        // the environment instead of being skipped in it.
        let is_unreadable = std::fs::File::open(src.join("bad")).is_err();

        let dst = fx.join("dst");
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
            &mut context(false, &mut [0u8; 64]),
            true,
            mode,
        ));

        if is_unreadable {
            // Like cp -R: the unreadable entry is recorded, not fatal.
            assert_eq!(1, errors.len(), "expected one error: {errors:?}");
            assert!(errors[0].contains("bad"), "unexpected error: {}", errors[0]);
            assert!(!dst.join("bad").exists());
        } else {
            // Nothing was unreadable here, so this is a plain full copy.
            assert!(errors.is_empty(), "unexpected errors: {errors:?}");
            assert!(dst.join("bad").exists());
        }
        // The walk must reach the entries on both sides of "bad" either way: a
        // failed entry must not abort the siblings.
        assert!(dst.join("a.txt").exists());
        assert!(dst.join("c.txt").exists());
        active.done();
    }

    #[test]
    fn copy_path_reuses_one_buffer_without_leaking_bytes_between_files() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a_long.txt"), b"aaaaaaaaaaaaaaaa").unwrap();
        std::fs::write(src.join("b_short.txt"), b"b").unwrap();
        let dst = fx.join("dst");
        let mode = std::fs::symlink_metadata(&src)
            .unwrap()
            .permissions()
            .mode();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        // One buffer serves the whole tree, so a short file copied after a
        // longer one must not pick up the previous file's trailing bytes.
        assert!(copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            true,
            mode,
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(
            b"aaaaaaaaaaaaaaaa".to_vec(),
            std::fs::read(dst.join("a_long.txt")).unwrap()
        );
        assert_eq!(
            b"b".to_vec(),
            std::fs::read(dst.join("b_short.txt")).unwrap()
        );
    }

    /// The modification times of `path` and everything under it, by relative
    /// name, so a tree can be compared against its copy.
    fn modified_times(root: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
        let mut times = vec![(
            PathBuf::from("."),
            fs::metadata(root).unwrap().modified().unwrap(),
        )];
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                let metadata = fs::metadata(&path).unwrap();
                if metadata.is_dir() {
                    stack.push(path.clone());
                }
                times.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    metadata.modified().unwrap(),
                ));
            }
        }
        times.sort();
        times
    }

    #[test]
    fn a_preserving_copy_keeps_the_modification_times_of_the_whole_tree() {
        let fx = TempDir::new("tasks_times");
        let src = fx.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        fs::write(src.join("sub").join("b.txt"), b"b").unwrap();
        // Backdate everything, deepest first so writing a child does not move
        // the parent's time again.
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        for relative in ["sub/b.txt", "sub", "a.txt", "."] {
            let file = File::options().read(true).open(src.join(relative)).unwrap();
            file.set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
        let before = modified_times(&src);
        let dst = fx.join("dst");
        let mode = fs::symlink_metadata(&src).unwrap().permissions().mode();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        // A same-device move is a rename, which keeps the timestamps. The
        // cross-device fallback copies, so it has to put them back or the
        // result depends on which mount the destination is on.
        assert!(copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(true, &mut [0u8; 64]),
            true,
            mode,
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(before, modified_times(&dst));
    }

    /// Runs `task` on the shared worker and waits for how it finished. `Err`
    /// carries the alerts of a task that was refused before it started.
    fn run_to_end(task: TaskCommand) -> Result<Task, Vec<Command>> {
        let (tx, rx) = mpsc::channel();
        let result = task.run(tx, None, 64_000, 64_000_000);
        if result.cancel_info.is_none() {
            return Err(result.command_result.into_commands());
        }
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Command::Progress(task)) if task.is_terminal() => return Ok(task),
                Ok(_) => {}
                Err(error) => panic!("task did not finish: {error}"),
            }
        }
    }

    #[test]
    fn a_plain_copy_does_not_keep_the_modification_times() {
        let fx = TempDir::new("tasks_times_off");
        let src = fx.join("a.txt");
        fs::write(&src, b"a").unwrap();
        let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        File::options()
            .write(true)
            .open(&src)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
        let dst = fx.join("dst");
        fs::create_dir(&dst).unwrap();

        // `cp` does not preserve timestamps without `-p`, so a copy must not
        // start doing it just because the move path needs to.
        let task = run_to_end(TaskCommand::Copy(
            PathInfo::try_from(src.as_path()).unwrap(),
            PathInfo::try_from(dst.as_path()).unwrap(),
            false,
        ))
        .expect("the copy should start");

        assert_eq!(None, task.error_message());
        assert_ne!(
            old,
            fs::metadata(dst.join("a.txt")).unwrap().modified().unwrap()
        );
    }

    #[test]
    fn a_copy_rereads_a_source_whose_type_changed_since_it_was_listed() {
        let fx = TempDir::new("tasks_copy_restat");
        let src = fx.join("was_a_directory");
        fs::write(&src, b"now a file").unwrap();
        let dst = fx.join("dst");
        fs::create_dir(&dst).unwrap();
        // Listed as a directory: `path_info` reports one whatever is on disk.
        let stale = path_info(src.to_str().unwrap(), "was_a_directory");

        // The type decides how the source is copied, so the one read when it
        // was listed must not be the one acted on.
        let task = run_to_end(TaskCommand::Copy(
            stale,
            PathInfo::try_from(dst.as_path()).unwrap(),
            false,
        ))
        .expect("the copy should start");

        assert_eq!(None, task.error_message());
        assert_eq!(
            b"now a file".to_vec(),
            fs::read(dst.join("was_a_directory")).unwrap()
        );
    }

    #[test]
    fn a_copy_of_an_unreadable_directory_is_refused_before_it_starts() {
        let fx = TempDir::new("tasks_copy_unreadable");
        let src = fx.join("locked");
        fs::create_dir(&src).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        // Root lists a mode-000 directory anyway; probe rather than inspect
        // the euid.
        let is_unreadable = fs::read_dir(&src).is_err();
        let dst = fx.join("dst");
        fs::create_dir(&dst).unwrap();

        let result = run_to_end(TaskCommand::Copy(
            PathInfo::try_from(src.as_path()).unwrap(),
            PathInfo::try_from(dst.as_path()).unwrap(),
            false,
        ));
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();

        if is_unreadable {
            // Refused on the calling thread, so no progress notice appears
            // for a copy that could not have copied anything.
            let Err(commands) = result else {
                panic!("the copy should have been refused");
            };
            let [Command::AlertError(message)] = commands.as_slice() else {
                panic!("expected one alert, got {commands:?}");
            };
            assert!(message.starts_with("Failed to read directory"), "{message}");
            assert!(!dst.join("locked").exists());
        } else {
            assert_eq!(
                None,
                result.expect("a readable copy starts").error_message()
            );
        }
    }

    #[test]
    fn a_directory_delete_counts_its_entries_as_the_progress_total() {
        let fx = TempDir::new("tasks_delete_task");
        let root = fx.join("doomed");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"x").unwrap();
        fs::write(root.join("sub").join("b.txt"), b"x").unwrap();

        let task = run_to_end(TaskCommand::Delete(
            PathInfo::try_from(root.as_path()).unwrap(),
        ))
        .expect("the delete should start");

        assert!(!root.exists());
        assert_eq!(None, task.error_message());
        assert!(!task.is_cancelled());
        // root, a.txt, sub, sub/b.txt: the unit is an entry removed, not the
        // single entry the task is seeded with.
        assert_eq!(4, task.combine_progress(&Progress::default()).total);
    }

    #[test]
    fn a_delete_unlinks_a_source_that_became_a_symlink_since_it_was_listed() {
        let fx = TempDir::new("tasks_delete_restat");
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let link = fx.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        // Listed as a directory before it was swapped for a link.
        let stale = path_info(link.to_str().unwrap(), "link");

        let task = run_to_end(TaskCommand::Delete(stale)).expect("the delete should start");

        // Deleted as the link it is now, not refused as a directory that
        // cannot be opened, and never followed.
        assert_eq!(None, task.error_message());
        assert!(link.symlink_metadata().is_err());
        assert_eq!(
            b"keep".to_vec(),
            fs::read(outside.join("keep.txt")).unwrap()
        );
    }

    /// A copy context whose collisions are settled by a standing choice, which
    /// is the only kind of answer that reaches a worker.
    fn answered_context<'a>(
        standing: ConflictChoice,
        buffer: &'a mut [u8],
        conflicts: &'a Conflicts,
    ) -> CopyContext<'a> {
        conflicts.answer(standing);
        CopyContext {
            buffer,
            conflicts: Some(conflicts),
            preserve_times: false,
            source: None,
            skipped: 0,
        }
    }

    /// A source entry and a destination path that another process took while
    /// the copy was already running. A copy's destination is free when it
    /// starts, so a race part way through is the only way a collision appears
    /// underneath it, and each entry is reached individually.
    fn raced(label: &str) -> (TempDir, PathBuf, PathBuf) {
        let fx = TempDir::new(label);
        let src = fx.join("src");
        let dst = fx.join("dst");
        fs::create_dir_all(&src).unwrap();
        // What the copy had created before the other process interfered.
        fs::create_dir_all(&dst).unwrap();
        (fx, src, dst)
    }

    /// The three pieces every raced-entry test needs. The receiver is leaked
    /// rather than returned as a fourth: nothing here reads it, and it only has
    /// to outlive the task, whose sends are best-effort anyway.
    fn raced_parts() -> (ActiveTask, Vec<String>, Conflicts) {
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        (copy_task(tx), Vec::new(), Conflicts::default())
    }

    fn mode_of(path: &Path) -> u32 {
        fs::symlink_metadata(path).unwrap().permissions().mode()
    }

    #[test]
    fn a_raced_file_is_replaced_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_overwrite");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            false,
            mode_of(&src.join("a.txt")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(b"src".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    #[test]
    fn a_raced_file_is_left_alone_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_skip");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = answered_context(ConflictChoice::SkipAll, &mut buffer, &conflicts);

        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("a.txt")),
        ));
        // A skipped name is a choice, not a failure, so nothing is reported.
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        // It is still counted: a move must not remove a source whose entries
        // are not all at the destination.
        assert_eq!(1, context.skipped);
        active.done();

        assert_eq!(b"raced".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    #[test]
    fn a_raced_name_with_no_standing_answer_is_recorded_rather_than_asked_about() {
        let (_fx, src, dst) = raced("tasks_raced_unanswered");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = CopyContext {
            buffer: &mut buffer,
            conflicts: Some(&conflicts),
            preserve_times: false,
            source: None,
            skipped: 0,
        };

        // A worker never asks: the queue that could have prompted is gone by
        // the time it runs.
        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("a.txt")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        // Recorded, not skipped: the entry was left behind by a failure to
        // settle it, so a move must report it rather than call it a choice.
        assert_eq!(0, context.skipped);
        assert_eq!(b"raced".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    #[test]
    fn a_raced_directory_is_recorded_when_only_overwrite_all_stands() {
        let (_fx, src, dst) = raced("tasks_raced_directory_overwrite");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::create_dir_all(dst.join("sub")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        // A directory is never replaced, so "overwrite all" cannot settle this
        // one and there is nobody to ask.
        assert!(copy_path(
            &src.join("sub"),
            &dst.join("sub"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            true,
            mode_of(&src.join("sub")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
    }

    #[test]
    fn a_raced_symlink_is_replaced_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_symlink");
        std::os::unix::fs::symlink("target", src.join("link")).unwrap();
        std::os::unix::fs::symlink("raced", dst.join("link")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        assert!(copy_path(
            &src.join("link"),
            &dst.join("link"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            false,
            mode_of(&src.join("link")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(
            PathBuf::from("target"),
            fs::read_link(dst.join("link")).unwrap()
        );
    }

    #[test]
    fn a_raced_symlink_with_no_standing_answer_is_recorded() {
        let (_fx, src, dst) = raced("tasks_raced_symlink_unanswered");
        std::os::unix::fs::symlink("target", src.join("link")).unwrap();
        std::os::unix::fs::symlink("raced", dst.join("link")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = CopyContext {
            buffer: &mut buffer,
            conflicts: Some(&conflicts),
            preserve_times: false,
            source: None,
            skipped: 0,
        };

        assert!(copy_path(
            &src.join("link"),
            &dst.join("link"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("link")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        assert_eq!(
            PathBuf::from("raced"),
            fs::read_link(dst.join("link")).unwrap()
        );
    }

    #[test]
    fn a_raced_special_file_is_left_alone_when_that_is_the_answer() {
        use nix::sys::stat::Mode;

        let (_fx, src, dst) = raced("tasks_raced_fifo");
        nix::unistd::mkfifo(&src.join("pipe"), Mode::from_bits_truncate(0o644)).unwrap();
        nix::unistd::mkfifo(&dst.join("pipe"), Mode::from_bits_truncate(0o600)).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];
        let mut context = answered_context(ConflictChoice::SkipAll, &mut buffer, &conflicts);

        assert!(copy_path(
            &src.join("pipe"),
            &dst.join("pipe"),
            &mut active,
            &mut errors,
            &mut context,
            false,
            mode_of(&src.join("pipe")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        assert_eq!(1, context.skipped);
        active.done();

        // Left exactly as the other process made it.
        assert_eq!(0o600, mode_of(&dst.join("pipe")) & 0o7777);
    }

    #[test]
    fn a_raced_file_where_a_directory_goes_is_replaced_when_that_is_the_answer() {
        let (_fx, src, dst) = raced("tasks_raced_file_for_directory");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("deep.txt"), b"src").unwrap();
        fs::write(dst.join("sub"), b"raced").unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        // A file is replaceable even where a directory is being copied, so
        // "overwrite all" settles this one.
        assert!(copy_path(
            &src.join("sub"),
            &dst.join("sub"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::OverwriteAll, &mut buffer, &conflicts),
            true,
            mode_of(&src.join("sub")),
        ));
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        active.done();

        assert_eq!(
            b"src".to_vec(),
            fs::read(dst.join("sub").join("deep.txt")).unwrap()
        );
    }

    #[test]
    fn a_raced_directory_is_never_replaced_so_its_subtree_is_dropped() {
        let (_fx, src, dst) = raced("tasks_raced_directory");
        fs::create_dir_all(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("deep.txt"), b"src").unwrap();
        fs::create_dir_all(dst.join("sub")).unwrap();
        let (mut active, mut errors, conflicts) = raced_parts();
        let mut buffer = [0u8; 64];

        // Only the skip choices apply to a directory, so "skip all" is what can
        // answer this one; "overwrite all" would still have to ask.
        assert!(copy_path(
            &src.join("sub"),
            &dst.join("sub"),
            &mut active,
            &mut errors,
            &mut answered_context(ConflictChoice::SkipAll, &mut buffer, &conflicts),
            true,
            mode_of(&src.join("sub")),
        ));
        active.done();

        // Dropped, not merged into: a directory is never replaced, and the copy
        // does not descend into one it did not create.
        assert!(!dst.join("sub").join("deep.txt").exists());
    }

    #[test]
    fn a_raced_name_with_no_paste_behind_it_is_recorded_rather_than_asked_about() {
        let (_fx, src, dst) = raced("tasks_raced_no_paste");
        fs::write(src.join("a.txt"), b"src").unwrap();
        fs::write(dst.join("a.txt"), b"raced").unwrap();
        let (mut active, mut errors, _conflicts) = raced_parts();

        // No paste at all (an operation that is not one), so there is not even
        // a standing answer to consult: the entry is left alone and reported.
        assert!(copy_path(
            &src.join("a.txt"),
            &dst.join("a.txt"),
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            false,
            mode_of(&src.join("a.txt")),
        ));
        active.done();

        assert_eq!(1, errors.len(), "expected the collision: {errors:?}");
        assert!(errors[0].contains("already exists"), "{}", errors[0]);
        assert_eq!(b"raced".to_vec(), fs::read(dst.join("a.txt")).unwrap());
    }

    // ── finishing a cross-device move, which is the only path that deletes ───

    /// A source tree, and the channel the finished task reports on.
    fn moved(label: &str) -> (TempDir, PathBuf, mpsc::Receiver<Command>, ActiveTask) {
        let fx = TempDir::new(label);
        let src = fx.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), b"src").unwrap();
        let (tx, rx) = mpsc::channel();
        let active = copy_task(tx);
        (fx, src, rx, active)
    }

    /// The last task the operation sent, which carries how it finished.
    fn finished_task(rx: &mpsc::Receiver<Command>) -> Task {
        let mut last = None;
        while let Ok(command) = rx.try_recv() {
            if let Command::Progress(task) = command {
                last = Some(task);
            }
        }
        last.expect("the task sent no progress")
    }

    #[test]
    fn a_cross_device_move_removes_its_source_once_every_entry_arrived() {
        let (_fx, src, rx, active) = moved("tasks_move_complete");

        finish_cross_device_move(active, CopyOutcome::default(), &src, true);

        assert!(!src.exists());
        assert_eq!(None, finished_task(&rx).error_message());
    }

    #[test]
    fn a_cross_device_move_keeps_its_source_when_an_entry_was_skipped() {
        let (_fx, src, rx, active) = moved("tasks_move_skipped");

        finish_cross_device_move(
            active,
            CopyOutcome {
                errors: Vec::new(),
                skipped: 1,
            },
            &src,
            true,
        );

        // The skipped entry never reached the destination, so removing the
        // source would delete the only copy of it. A skip is not an error, so
        // the emptiness of `errors` must not be what decides this.
        assert!(src.join("a.txt").exists());
        let message = finished_task(&rx)
            .error_message()
            .expect("the kept source must be reported");
        assert!(message.contains("Skipped 1 entry"), "{message}");
    }

    #[test]
    fn a_cross_device_move_keeps_its_source_when_an_entry_failed() {
        let (_fx, src, rx, active) = moved("tasks_move_failed");

        finish_cross_device_move(
            active,
            CopyOutcome {
                errors: vec!["a.txt already exists".to_string()],
                skipped: 0,
            },
            &src,
            true,
        );

        assert!(src.join("a.txt").exists());
        assert_eq!(
            Some("a.txt already exists".to_string()),
            finished_task(&rx).error_message()
        );
    }

    #[test]
    fn several_failed_entries_are_reported_as_the_first_and_a_count_of_the_rest() {
        let (tx, rx) = mpsc::channel();

        finalize_copy(
            copy_task(tx),
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string(),
            ],
        );

        assert_eq!(
            Some("first (and 2 more)".to_string()),
            finished_task(&rx).error_message()
        );
    }

    /// `copy_path_continues_past_unreadable_entries` degrades to a plain full
    /// copy under root, so the error-recording path is pinned here instead,
    /// with a failure the kernel enforces for every user.
    #[test]
    fn copy_path_records_a_directory_it_cannot_create() {
        let fx = TempDir::new("tasks");
        let src = fx.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.txt"), b"a").unwrap();
        // A regular file already occupies the destination, so creating the
        // directory fails with EEXIST regardless of privileges.
        let dst = fx.join("dst");
        std::fs::write(&dst, b"in the way").unwrap();
        let mode = std::fs::symlink_metadata(&src)
            .unwrap()
            .permissions()
            .mode();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();

        // The subtree is skipped rather than failing the whole task, so a
        // multi-source paste still copies its other sources.
        assert!(copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            true,
            mode,
        ));

        assert_eq!(1, errors.len(), "expected one error: {errors:?}");
        assert!(errors[0].contains("dst"), "unexpected error: {}", errors[0]);
        // The occupying file must be left exactly as it was.
        assert_eq!(b"in the way".to_vec(), std::fs::read(&dst).unwrap());
        active.done();
    }

    // ── prepare_destination: the ordering every queued operation depends on ──

    /// A source file holding "src", a destination file holding "dest", and an
    /// `ActiveTask` for the operation that would replace one with the other.
    /// The receiver is leaked for the same reason as in `raced_parts`.
    fn destination(label: &str) -> (TempDir, PathBuf, PathBuf, ActiveTask, CancellationToken) {
        let fx = TempDir::new(label);
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::write(&dst, b"dest").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::mem::forget(rx);
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        (fx, src, dst, active, token)
    }

    #[test]
    fn a_cancel_that_lands_while_queued_stops_the_task() {
        let (_fx, _src, dst, active, token) = destination("tasks_prepare_cancelled");
        token.cancel();

        // The queue can hold a task for as long as the operations ahead of it
        // take, so a cancel has to be honored before anything is written, and
        // before the destination it would have replaced is cleared.
        assert!(check_cancelled(active).is_none());
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn a_granted_overwrite_clears_the_destination() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_overwrite");

        let active = clear_destination(active, &src, &dst, true).expect("the task should continue");

        assert!(!dst.exists());
        active.done();
    }

    #[test]
    fn a_destination_survives_when_overwrite_was_not_granted() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_no_overwrite");

        let active =
            clear_destination(active, &src, &dst, false).expect("the task should continue");

        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
        active.done();
    }

    #[test]
    fn a_destination_that_already_vanished_is_not_an_error() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_vanished");
        std::fs::remove_file(&dst).unwrap();

        // Something else removed it between the prompt and the worker, which
        // is the outcome the user asked for anyway.
        assert!(clear_destination(active, &src, &dst, true).is_some());
    }

    #[test]
    fn an_unreadable_source_leaves_the_destination_it_would_have_replaced() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_unreadable");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads a mode-000 file anyway (CAP_DAC_OVERRIDE), as do mounts
        // that ignore permissions; probe rather than inspect the euid.
        let is_unreadable = File::open(&src).is_err();

        // The source still exists, so only opening it can tell that the copy
        // is bound to fail. Clearing first would leave the user with neither
        // file's contents.
        let prepared = prepare_destination(active, "copy", &src, &dst, true, mode_of(&src));

        if is_unreadable {
            assert!(prepared.is_none());
            assert_eq!(b"dest".to_vec(), fs::read(&dst).unwrap());
        } else {
            let (active, source) = prepared.expect("a readable source should continue");
            assert!(source.is_some());
            assert!(!dst.exists());
            active.done();
        }
    }

    #[test_case("copy" ; "a copy")]
    #[test_case("move" ; "a move")]
    fn an_unreadable_source_is_reported_under_the_operation_that_failed(verb: &str) {
        let fx = TempDir::new("tasks_prepare_verb");
        let src = fx.join("missing.txt");
        let dst = fx.join("dest.txt");
        let (tx, rx) = mpsc::channel();
        // A regular-file mode with no file behind it, so opening fails for
        // root as well.
        let prepared = prepare_destination(copy_task(tx), verb, &src, &dst, false, 0o100_644);

        assert!(prepared.is_none());
        let message = finished_task(&rx)
            .error_message()
            .expect("an error")
            .clone();
        assert!(
            message.starts_with(&format!("Failed to {verb} ")),
            "{message}"
        );
    }

    #[test]
    fn a_copied_file_is_created_owner_only() {
        let fx = TempDir::new("tasks_create_file_mode");
        let dst = fx.join("dst.txt");

        // The source's group and other bits wait until the copy stops, so a
        // file still being written is readable by no one the source is not.
        let _file = create_file(&dst, 0o100_644).unwrap();

        assert_eq!(0o600, mode_of(&dst) & 0o7777);
    }

    #[test]
    fn a_copied_directory_is_created_owner_only() {
        let fx = TempDir::new("tasks_create_directory_mode");
        let dst = fx.join("dst");

        create_directory(&dst).unwrap();

        assert_eq!(0o700, mode_of(&dst) & 0o7777);
    }

    /// A copy task already cancelled, so the walk stops at its first check.
    fn cancelled_copy_task() -> ActiveTask {
        let (tx, rx) = mpsc::channel();
        std::mem::forget(rx);
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        token.cancel();
        active
    }

    #[test]
    fn a_cancelled_file_copy_is_left_with_the_source_mode() {
        let fx = TempDir::new("tasks_cancelled_file_mode");
        let src = fx.join("src.txt");
        fs::write(&src, b"src").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o640)).unwrap();
        let dst = fx.join("dst.txt");
        let mut active = cancelled_copy_task();
        let mut errors = Vec::new();

        // Cancelled after the destination is created and before a byte is
        // written. The partial file stays, like an interrupted `cp`, and gets
        // the source's mode: neither the owner-only mode it was created with
        // nor anything broader than the source.
        assert!(!copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            false,
            mode_of(&src),
        ));
        active.cancelled();

        assert!(dst.exists());
        assert_eq!(0o640, mode_of(&dst) & 0o7777);
    }

    #[test]
    fn a_cancelled_directory_copy_is_left_with_the_source_mode() {
        let fx = TempDir::new("tasks_cancelled_directory_mode");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), b"a").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o750)).unwrap();
        let dst = fx.join("dst");
        let mut active = cancelled_copy_task();
        let mut errors = Vec::new();

        // Cancelled at the first entry, after the directory is created.
        assert!(!copy_path(
            &src,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            true,
            mode_of(&src),
        ));
        active.cancelled();

        assert!(!dst.join("a.txt").exists());
        assert_eq!(0o750, mode_of(&dst) & 0o7777);
    }

    #[test]
    fn a_vanished_source_leaves_the_destination_it_would_have_replaced() {
        let (_fx, src, dst, active, _token) = destination("tasks_prepare_source_gone");
        std::fs::remove_file(&src).unwrap();

        // A task can wait behind a long operation on the shared worker, so the
        // source it was going to copy may be gone by the time it runs. Clearing
        // the destination for a copy that can no longer happen would leave the
        // user with neither entry.
        assert!(clear_destination(active, &src, &dst, true).is_none());
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn a_failed_overwriting_move_leaves_the_destination_in_place() {
        let fx = TempDir::new("tasks_move_failure");
        let src = fx.join("gone.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&dst, b"dest").unwrap();

        // The source vanished while the task sat in the queue, which a single
        // worker running a long copy makes easy. Clearing the destination up
        // front would lose it for a move that then cannot happen, leaving the
        // user with neither file.
        assert!(rename_for_move(&src, &dst, true).is_err());
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn an_overwriting_move_replaces_a_file_without_clearing_it_first() {
        let fx = TempDir::new("tasks_move_atomic");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::write(&dst, b"dest").unwrap();

        // The kernel replaces atomically here, so there is no moment in which
        // the destination is missing.
        rename_for_move(&src, &dst, true).unwrap();
        assert_eq!(b"src".to_vec(), std::fs::read(&dst).unwrap());
        assert!(!src.exists());
    }

    #[test]
    fn an_overwriting_move_clears_a_file_that_a_directory_must_replace() {
        let fx = TempDir::new("tasks_move_dir_over_file");
        let src = fx.join("srcdir");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("inner.txt"), b"src").unwrap();
        let dst = fx.join("dest");
        std::fs::write(&dst, b"dest").unwrap();

        // `rename` refuses to replace a non-directory with a directory, so
        // this is the one case that has to clear the destination itself.
        rename_for_move(&src, &dst, true).unwrap();
        assert_eq!(
            b"src".to_vec(),
            std::fs::read(dst.join("inner.txt")).unwrap()
        );
    }

    #[test]
    fn a_move_without_overwrite_refuses_an_existing_destination() {
        let fx = TempDir::new("tasks_move_no_overwrite");
        let src = fx.join("src.txt");
        let dst = fx.join("dest.txt");
        std::fs::write(&src, b"src").unwrap();
        std::fs::write(&dst, b"dest").unwrap();

        // Without a granted overwrite the destination is never touched, even
        // though the same function would replace it with one.
        assert!(rename_for_move(&src, &dst, false).is_err());
        assert_eq!(b"dest".to_vec(), std::fs::read(&dst).unwrap());
        assert!(src.exists());
    }

    #[test]
    fn a_panicking_job_does_not_stop_the_queue() {
        // The worker is shared by every file operation. If a panic took it
        // down, every later operation would register a task, announce itself
        // at 0%, and never run or finish.
        queue_operation(|| panic!("a file operation panicked on purpose"));
        let (tx, rx) = std::sync::mpsc::channel();
        queue_operation(move || {
            let _ = tx.send(());
        });

        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "the job queued after a panicking one never ran"
        );
    }

    #[test]
    fn rename_no_replace_moves_to_new_destination() {
        let fx = TempDir::new("tasks");
        let src = fx.join("a.txt");
        std::fs::write(&src, b"x").unwrap();
        let dst = fx.join("b.txt");

        rename_no_replace(&src, &dst).unwrap();
        assert!(!src.exists());
        assert_eq!(b"x".to_vec(), std::fs::read(&dst).unwrap());
    }

    #[test]
    fn remove_path_deletes_directory_tree() {
        let fx = TempDir::new("tasks");
        let root = fx.join("doomed");
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
    fn dir_total_entries_counts_the_root_and_every_descendant() {
        let fx = TempDir::new("tasks");
        let root = fx.join("tree");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"x").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let active = copy_task(tx);

        // root, a.txt, sub, sub/b.txt: one unit per removal the delete walk
        // makes, so the bar reaches exactly 100% when the tree is gone.
        assert_eq!(Some(4), dir_total_entries(&active, &root));
        active.done();
    }

    #[test]
    fn dir_total_size_sums_the_files_of_the_whole_tree_but_not_its_symlinks() {
        let fx = TempDir::new("tasks");
        let root = fx.join("tree");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("a.txt"), b"abc").unwrap();
        fs::write(root.join("sub").join("b.txt"), b"defgh").unwrap();
        // Recreated as a link, so no bytes are transferred for it.
        std::os::unix::fs::symlink(root.join("a.txt"), root.join("link")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let active = copy_task(tx);

        assert_eq!(Some(8), dir_total_size(&active, &root));
        active.done();
    }

    #[test]
    fn copy_path_advances_progress_from_the_bytes_written() {
        let fx = TempDir::new("tasks_copy_progress");
        let src = fx.join("src.bin");
        fs::write(&src, [7u8; 200]).unwrap();
        let (tx, rx) = mpsc::channel();
        let (mut active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            200,
        );
        let mut errors = Vec::new();

        assert!(copy_path(
            &src,
            &fx.join("dst.bin"),
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 64]),
            false,
            mode_of(&src),
        ));
        active.done();
        let completed: Vec<u64> = rx
            .try_iter()
            .filter_map(|command| match command {
                Command::Progress(task) if !task.is_terminal() => {
                    Some(task.combine_progress(&Progress::default()).completed)
                }
                _ => None,
            })
            .collect();

        // One 64-byte chunk is the first update, before `done` fills the bar,
        // and each later update reports more than the last.
        assert_eq!(Some(&64), completed.first());
        assert!(completed.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test_case(false ; "a file")]
    #[test_case(true  ; "a symlink to a directory")]
    fn remove_path_unlinks_a_single_entry_without_following_it(is_symlink: bool) {
        let fx = TempDir::new("tasks_delete_single");
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let entry = fx.join("entry");
        if is_symlink {
            std::os::unix::fs::symlink(&outside, &entry).unwrap();
        } else {
            fs::write(&entry, b"x").unwrap();
        }
        let (tx, _rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        // `is_directory` is false for both: it comes from `symlink_metadata`.
        remove_path(&entry, false, active)
            .expect("the entry should be removed")
            .done();

        assert!(entry.symlink_metadata().is_err());
        assert_eq!(
            b"keep".to_vec(),
            fs::read(outside.join("keep.txt")).unwrap()
        );
    }

    #[test]
    fn the_pre_scans_stop_when_the_task_is_cancelled() {
        let fx = TempDir::new("tasks");
        std::fs::write(fx.join("a.txt"), b"x").unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        token.cancel();

        // Both scans run before any file is touched and take as long as the
        // tree is large, so a cancel must not have to wait one out.
        assert_eq!(None, dir_total_size(&active, fx.path()));
        assert_eq!(None, dir_total_entries(&active, fx.path()));
        active.done();
    }

    #[test]
    fn remove_path_advances_progress_from_the_removals() {
        let fx = TempDir::new("tasks");
        let root = fx.join("doomed");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("f.txt"), b"x").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        // Larger than the three entries removed, so an overcount is not
        // clamped away.
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            100,
        );

        let active = remove_path(&root, true, active).expect("the tree should be removed");
        let completed_of = |command| match command {
            Command::Progress(task) => Some(task.combine_progress(&Progress::default()).completed),
            _ => None,
        };
        let completed: Vec<u64> = rx.try_iter().filter_map(completed_of).collect();
        active.send_progress();
        let final_count = rx.try_iter().find_map(completed_of);
        active.done();

        // The bar must advance from the removals themselves rather than from
        // `done` filling it in at the end, so a long delete shows motion
        // instead of sitting at 0%.
        assert_eq!(Some(&1), completed.first());
        // How many updates arrive depends on how much of
        // `PROGRESS_MIN_INTERVAL` this tree takes to remove. What must hold
        // either way is that each reports more than the last.
        assert!(completed.windows(2).all(|pair| pair[0] < pair[1]));
        // One unit per removal (root, sub, sub/f.txt), none for descending.
        assert_eq!(Some(3), final_count);
    }

    #[test]
    fn remove_path_stops_when_already_cancelled() {
        let fx = TempDir::new("tasks");
        let root = fx.join("kept");
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
        let fx = TempDir::new("tasks");
        std::fs::write(fx.join("a.txt"), b"x").unwrap();
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
        let mut dir = open_directory(CWD, fx.path()).unwrap();
        assert!(list_entries(&active, &mut dir).unwrap().is_none());
    }

    /// Run by `remove_path_deletes_a_tree_deeper_than_the_open_file_limit`
    /// under a low limit; on its own it proves nothing.
    #[test]
    #[ignore = "run under a lowered open-file limit by the test below"]
    fn remove_path_under_a_low_open_file_limit() {
        let fx = TempDir::new("tasks_delete_deep");
        let root = fx.join("doomed");
        let mut deepest = root.clone();
        for _ in 0..200 {
            deepest.push("d");
        }
        fs::create_dir_all(&deepest).unwrap();
        fs::write(deepest.join("leaf.txt"), b"x").unwrap();
        let (tx, rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        let finished = remove_path(&root, true, active);

        let error = finished
            .is_none()
            .then(|| finished_task(&rx).error_message())
            .flatten();
        assert_eq!(None, error);
        assert!(!root.exists());
    }

    /// Holding an fd per level would need 200 here, far over the limit of 64
    /// the tree is deleted under.
    #[cfg(target_os = "linux")]
    #[test]
    fn remove_path_deletes_a_tree_deeper_than_the_open_file_limit() {
        let output = std::process::Command::new("sh")
            .args(["-c", r#"ulimit -Sn 64 && exec "$0" "$@""#])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "file_system::tasks::tests::remove_path_under_a_low_open_file_limit",
                "--ignored",
                "--test-threads=1",
            ])
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[test]
    fn reopening_a_parent_finds_the_directory_that_was_listed() {
        let fx = TempDir::new("tasks_reopen_parent");
        let parent = fx.join("parent");
        fs::create_dir_all(parent.join("child")).unwrap();
        let expected = DirId::of(&open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, parent.join("child")).unwrap();

        let reopened = reopen_parent(&child, expected).unwrap();

        assert_eq!(expected, DirId::of(&reopened).unwrap());
    }

    /// A child moved elsewhere during the walk has a different "..", which the
    /// walk must not continue into.
    #[test]
    fn reopening_the_parent_of_a_moved_directory_is_refused() {
        let fx = TempDir::new("tasks_reopen_moved");
        let parent = fx.join("parent");
        let elsewhere = fx.join("elsewhere");
        fs::create_dir_all(parent.join("child")).unwrap();
        fs::create_dir_all(&elsewhere).unwrap();
        let expected = DirId::of(&open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, parent.join("child")).unwrap();
        fs::rename(parent.join("child"), elsewhere.join("child")).unwrap();

        let error = reopen_parent(&child, expected).unwrap_err();

        assert_eq!(
            "it was moved while its contents were being deleted",
            error.to_string()
        );
    }

    #[test]
    fn remove_path_unlinks_a_symlink_to_a_directory_without_following_it() {
        let fx = TempDir::new("tasks_delete_symlink");
        let outside = fx.join("outside");
        fs::create_dir_all(outside.join("sub")).unwrap();
        fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let root = fx.join("doomed");
        fs::create_dir_all(root.join("sub")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("sub").join("link")).unwrap();
        let (tx, _rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        assert!(remove_path(&root, true, active).is_some());

        // The link goes with the tree; what it points at is outside it.
        assert!(!root.exists());
        assert_eq!(
            b"keep".to_vec(),
            fs::read(outside.join("keep.txt")).unwrap()
        );
        assert!(outside.join("sub").is_dir());
    }

    #[test]
    fn open_directory_refuses_a_symlink_to_a_directory() {
        let fx = TempDir::new("tasks_open_directory_symlink");
        fs::create_dir(fx.join("real")).unwrap();
        std::os::unix::fs::symlink(fx.join("real"), fx.join("link")).unwrap();
        let parent = open_directory(CWD, fx.path()).unwrap();

        // A directory swapped for a link after it was listed has to fail to
        // open, or the walk would descend into the link's target.
        let error = open_directory(parent.fd().unwrap(), "link")
            .expect_err("a symlink must not be opened as a directory");

        let errno = error.raw_os_error();
        assert!(
            errno == Some(nix::libc::ENOTDIR) || errno == Some(nix::libc::ELOOP),
            "{error}"
        );
        assert!(open_directory(parent.fd().unwrap(), "real").is_ok());
    }

    #[test]
    fn copy_path_recreates_a_symlink_without_following_it() {
        use std::os::unix::fs::PermissionsExt;

        let fx = TempDir::new("tasks");
        let target = fx.join("target.txt");
        std::fs::write(&target, b"hello").unwrap();
        std::fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        let link = fx.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let dst = fx.join("copied_link.txt");

        let source_mode = fs::symlink_metadata(&link).unwrap().permissions().mode();
        assert!(unix_mode::is_symlink(source_mode));

        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        assert!(copy_path(
            &link,
            &dst,
            &mut active,
            &mut errors,
            &mut context(false, &mut [0u8; 1024]),
            false,
            source_mode,
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
