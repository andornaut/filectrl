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
use rustix::{
    fs::{
        AtFlags, CWD, Dir, FileType, Mode, OFlags, fcntl_getfl, fcntl_setfl, fstat, mkdirat,
        openat, readlinkat, statat, symlinkat, unlinkat,
    },
    io::Errno,
};

use super::{
    Occupant, PasteStep,
    conflicts::Conflicts,
    path_info::{PathInfo, compact, is_same_entry, lists_stable_inodes},
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
    /// Keep each entry's full mode and its times, as `mv` does, so a
    /// cross-device move leaves what a same-device rename would have. A copy
    /// takes the umask and drops the special bits instead, as `cp` does.
    preserve: bool,
    /// The top-level source file, already opened by `prepare_destination`
    /// before it cleared the destination. `copy_file` takes it in place of
    /// opening the path again. `None` for any other source.
    source: Option<File>,
    /// Entries a standing "skip all" left alone. Counted separately from the
    /// errors: skipping is a choice rather than a failure, but a move still
    /// must not remove a source whose entries never reached the destination.
    skipped: usize,
    /// The directories this copy created, which it never descends into as a
    /// source: a destination swapped for a link into the source tree, or a
    /// bind mount of it, would otherwise copy the copy into itself without end.
    created: std::collections::HashSet<DirId>,
    /// The top-level source that was copied, which is the entry a move removes
    /// afterwards and no other.
    root: Option<DirId>,
    /// One debouncer for the whole tree, against its total: one per file would
    /// send an update for every file, since a debouncer's first call triggers.
    progress: debounce::ProgressDebouncer,
}

impl<'a> CopyContext<'a> {
    fn new(
        buffer: &'a mut [u8],
        conflicts: Option<&'a Conflicts>,
        preserve: bool,
        source: Option<File>,
        total_size: u64,
    ) -> Self {
        Self {
            buffer,
            conflicts,
            preserve,
            source,
            skipped: 0,
            created: std::collections::HashSet::new(),
            root: None,
            progress: debounce::ProgressDebouncer::new(
                PROGRESS_DEBOUNCE_PERCENTAGE,
                PROGRESS_MIN_INTERVAL,
                total_size,
            ),
        }
    }
}

/// What a tree copy left behind: the entries that could not be written, how
/// many a standing "skip all" left alone, and which entry was copied.
#[derive(Default)]
struct CopyOutcome {
    errors: Vec<String>,
    skipped: usize,
    root: Option<DirId>,
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
    let path = match restat_listed(path) {
        Ok(fresh) => fresh,
        Err(stale) => return TaskRunResult::failed(stale.into_error("copy", &path.path).into()),
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
    let path = match restat_listed(path) {
        Ok(fresh) => fresh,
        Err(stale) => return TaskRunResult::failed(stale.into_error("move", &path.path).into()),
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
    let path = match restat_listed(path) {
        Ok(fresh) => fresh,
        Err(stale) => return TaskRunResult::failed(stale.into_error("delete", &path.path).into()),
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
    let expected = DirId::of_listed(&path);
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
        if let Some(active) = remove_path(&path, expected, is_directory, active, Removal::Delete) {
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
    preserve: bool,
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
    let mut context = CopyContext::new(&mut buffer, conflicts, preserve, source, total_size);
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
            root: context.root,
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
    let mut total: u64 = 0;
    scan_tree(active, root, |dir, name, is_directory| {
        if is_directory {
            return;
        }
        let stat = dir
            .fd()
            .and_then(|fd| statat(fd, name, AtFlags::SYMLINK_NOFOLLOW));
        if let Ok(stat) = stat
            && FileType::from_raw_mode(stat.st_mode) != FileType::Symlink
        {
            total = total.saturating_add(u64::try_from(stat.st_size).unwrap_or(0));
        }
    })?;
    Some(total)
}

/// Best-effort recursive entry count for the delete progress total, including
/// `root` itself. Reads directories only, with no per-entry stat, so it is
/// cheaper than `dir_total_size`; a directory that cannot be listed counts as
/// the single entry it is. Returns `None` when the task was cancelled.
fn dir_total_entries(active: &ActiveTask, root: &Path) -> Option<u64> {
    let mut total: u64 = 1; // `root` itself, which appears in no listing.
    scan_tree(active, root, |_, _, _| total += 1)?;
    Some(total)
}

/// Walks the tree below `root` for a pre-scan, calling `visit` with each
/// entry's directory, its name, and whether it is a directory. The walk is the
/// delete walk's: relative to open directories, never through a symlink (a
/// directory swapped for one after it was listed is not descended into), and
/// holding only the directory being read open. A directory that cannot be
/// opened is skipped, and one whose parent cannot be reopened ends the walk
/// early, which leaves a smaller total rather than an error. Returns `None`
/// when the task was cancelled.
fn scan_tree(
    active: &ActiveTask,
    root: &Path,
    mut visit: impl FnMut(&Dir, &CStr, bool),
) -> Option<()> {
    let level = |listed: std::io::Result<(Dir, Entries)>| {
        let (dir, entries) = listed.ok()?;
        DirId::of_dir(&dir).ok().map(|id| Level {
            dir: Some(dir),
            id,
            name: None,
            entries: entries.into_iter(),
        })
    };
    let Some(root) = level(open_and_list(None, root)) else {
        return Some(());
    };
    let mut stack = vec![root];
    while let Some(top) = stack.last_mut() {
        if active.is_cancelled() {
            return None;
        }
        let Some((name, is_directory)) = top.entries.next() else {
            let level = stack.pop().expect("stack is non-empty");
            let Some(parent) = stack.last_mut() else {
                break;
            };
            let child = level
                .dir
                .as_ref()
                .expect("the level being read holds its fd");
            match reopen_parent(child, parent.id) {
                Ok(dir) => parent.dir = Some(dir),
                Err(_) => break,
            }
            continue;
        };
        let dir = top.dir.as_ref().expect("the level being read holds its fd");
        visit(dir, &name, is_directory);
        if is_directory && let Some(child) = level(open_and_list(Some(dir), &name)) {
            // Closed until the walk returns here, through `reopen_parent`.
            top.dir = None;
            stack.push(child);
        }
    }
    Some(())
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
/// unwraps the directory and its entries, or finalizes `$active` as an error
/// naming `$dir` and returns `None` if the open or the read failed.
macro_rules! list_or_abort {
    ($active:expr, $parent:expr, $name:expr, $dir:expr) => {{
        let dir = $dir;
        match open_and_list($parent, $name) {
            Ok(listed) => listed,
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

/// Why an entry was not acted on as it was listed.
pub(super) enum Stale {
    /// The path could not be read again: gone, or no longer reachable.
    Unreadable(anyhow::Error),
    /// The path names another entry than the one listed.
    Changed,
}

impl Stale {
    pub(super) fn is_not_found(&self) -> bool {
        match self {
            Self::Unreadable(error) => error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == ErrorKind::NotFound),
            Self::Changed => false,
        }
    }

    /// The alert for refusing to `operation` the entry at `path`.
    pub(super) fn into_error(self, operation: &str, path: &Path) -> anyhow::Error {
        match self {
            Self::Unreadable(error) => anyhow!("Failed to {operation} {}: {error}", compact(path)),
            Self::Changed => anyhow!(
                "Cannot {operation} {}: it changed since it was listed",
                compact(path)
            ),
        }
    }
}

/// Re-reads the entry `listed` names, without following a symlink in its own
/// name, and refuses it unless it is still the entry that was listed: the same
/// device and inode. The path is resolved again at action time, so without
/// this a parent directory swapped for a symlink since the listing would lead
/// the operation to a same-named entry the user never saw. Returns the fresh
/// metadata, since the listed mode and size may be out of date.
pub(super) fn restat_listed(listed: &PathInfo) -> Result<PathInfo, Stale> {
    let fresh = PathInfo::try_from(listed.as_path()).map_err(Stale::Unreadable)?;
    if !listed.is_still_listed_as(&fresh) {
        return Err(Stale::Changed);
    }
    Ok(fresh)
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
    // Not cancellable: the copy is complete, so removing the source is the only
    // way to finish the move. Mark it so a cancel keypress during this stage
    // does not claim to have cancelled anything.
    active.set_uncancellable();
    // Only the entry that was copied: another renamed onto its name since
    // would be lost with nothing to show for it. The copy records it whenever
    // it read the source, which a clean outcome always did.
    let Some(root) = outcome.root else {
        active.error(Removal::MovedSource.refusal(old_path));
        return;
    };
    // Like `mv`, an entry written into the source while it was being copied is
    // removed with the rest.
    if let Some(active) = remove_path(old_path, root, is_directory, active, Removal::MovedSource) {
        active.done();
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
///
/// Every entry is read, created and changed relative to an open directory on
/// its side, never through a path, and nothing is opened through a symlink. An
/// entry swapped for a link while the copy runs therefore fails rather than
/// leading the copy out of the tree: reading a file outside the source, or
/// creating one or changing a mode outside the destination. Only the top-level
/// parents, which the user chose, are opened by path.
///
/// An entry's mode and owner are read from the file actually copied, never
/// from an earlier stat of its name: another file renamed over the name in
/// between would otherwise be given the first one's mode. `listed_mode` is the
/// type the task was started for, and a source that is no longer of that type
/// is refused.
fn copy_path(
    old_path: &Path,
    new_path: &Path,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    is_directory: bool,
    listed_mode: u32,
) -> bool {
    let failed = |error: &dyn std::fmt::Display| {
        format!(
            "Failed to copy {} to {}: {error}",
            compact(old_path),
            compact(new_path)
        )
    };
    let (Some(src_name), Some(dst_name)) = (c_name(old_path), c_name(new_path)) else {
        errors.push(format!(
            "Cannot copy {}: path has no file name",
            compact(old_path)
        ));
        return true;
    };
    let opened = open_parent(old_path).and_then(|src| {
        // The file `prepare_destination` opened is the one copied, so its
        // metadata is the one that counts.
        let stat = match &context.source {
            Some(file) => fstat(file)?,
            None => statat(&src, &src_name, AtFlags::SYMLINK_NOFOLLOW)?,
        };
        Ok((src, open_parent(new_path)?, stat))
    });
    let (src_parent, dst_parent, stat) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };
    context.root = Some(DirId {
        dev: stat.st_dev,
        ino: stat.st_ino,
    });
    if raw_type(stat_mode(&stat)) != raw_type(listed_mode) {
        errors.push(format!(
            "Cannot copy {}: its type changed since it was selected",
            compact(old_path)
        ));
        return true;
    }
    let mut paths = Paths {
        old: old_path.to_path_buf(),
        new: new_path.to_path_buf(),
    };
    let at = At {
        src: &src_parent,
        dst: &dst_parent,
        src_name: &src_name,
        dst_name: &dst_name,
    };
    if !is_directory {
        return copy_entry(&at, &paths, active, errors, context, &stat);
    }
    let Some(level) = enter_directory(&at, &paths, errors, context, &stat) else {
        return true;
    };
    // Only the directories being worked in are held open.
    drop((src_parent, dst_parent));
    copy_tree(level, &mut paths, active, errors, context)
}

/// Copies the directory tree below `root`, then finishes each directory: its
/// mode, and its times for a move. Iterative, so depth cannot overflow the
/// thread stack, and like the delete walk only the directory being worked in
/// holds its handles, so depth is not bounded by the open-file limit either.
/// Descending closes the parent's handles; returning reopens them through the
/// child's "..", refusing to continue unless they are the directories that
/// were listed, since the child may have been moved in between.
///
/// A cancel stops the walk between entries, and every directory entered is
/// still finished on the way out, so none is left owner-only.
fn copy_tree(
    root: CopyLevel,
    paths: &mut Paths,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) -> bool {
    let mut stack = vec![root];
    let mut cancelled = false;
    while let Some(top) = stack.last_mut() {
        cancelled |= active.is_cancelled();
        let next = if cancelled { None } else { top.names.next() };
        let Some(name) = next else {
            let level = stack.pop().expect("stack is non-empty");
            let (src, dst) = level.handles();
            finish_directory(Some(src), dst, &level.source, context);
            let Some(parent) = stack.last_mut() else {
                break;
            };
            paths.pop();
            match (
                reopen_parent_of(src, parent.src_id),
                reopen_parent_of(dst, parent.dst_id),
            ) {
                (Ok(src), Ok(dst)) => {
                    parent.src = Some(src);
                    parent.dst = Some(dst);
                }
                (Err(error), _) | (_, Err(error)) => {
                    // Neither this directory nor any above it can be reached
                    // again, so the walk ends here. They keep the owner-only
                    // mode they were created with.
                    errors.push(format!("Failed to copy {}: {error}", compact(&paths.old)));
                    return !cancelled;
                }
            }
            continue;
        };
        paths.push(&name);
        let (src, dst) = top.handles();
        let stat = match statat(src, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) => {
                errors.push(format!(
                    "Failed to read metadata for {}: {error}",
                    compact(&paths.old)
                ));
                paths.pop();
                continue;
            }
        };
        let at = At {
            src,
            dst,
            src_name: &name,
            dst_name: &name,
        };
        if FileType::from_raw_mode(stat.st_mode) == FileType::Directory {
            if let Some(child) = enter_directory(&at, paths, errors, context, &stat) {
                // Closed until the walk returns here.
                top.src = None;
                top.dst = None;
                stack.push(child);
                // The child's path stays pushed until it is finished.
                continue;
            }
        } else if !copy_entry(&at, paths, active, errors, context, &stat) {
            cancelled = true;
        }
        paths.pop();
    }
    !cancelled
}

/// Creates and opens the destination directory `at` names, then opens and
/// lists the source. `None` when there is nothing to descend into, having
/// recorded why.
///
/// The destination is created owner-writable and searchable so its children
/// can be created, and `finish_directory` gives it its mode once they are.
/// For a copy it starts with the other bits `stat` gives the source, which the
/// umask trims: `finish_directory` reads back what the umask left. The source
/// opened must be the directory `stat` describes, and its own metadata is what
/// the copy finishes with. A directory this copy created is refused before
/// anything is created for it.
fn enter_directory(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &rustix::fs::Stat,
) -> Option<CopyLevel> {
    let id = DirId {
        dev: stat.st_dev,
        ino: stat.st_ino,
    };
    if context.created.contains(&id) {
        errors.push(format!(
            "Cannot copy {}: it is inside the copy being made",
            compact(&paths.old)
        ));
        return None;
    }
    let creation = if context.preserve {
        0o700
    } else {
        (stat_mode(stat) & 0o777) | 0o700
    };
    let created = match mkdirat(at.dst, at.dst_name, mode_bits(creation)) {
        // Something took this name while the copy was running: the destination
        // was free when the task started, so this is another process writing
        // into the tree. A directory is never replaced, so only a standing
        // "skip all" settles this one, and skipping drops the subtree.
        Err(Errno::EXIST) => {
            if !resolve_nested(context, errors, at.dst, at.dst_name, &paths.new) {
                return None;
            }
            remove_existing_at(at.dst, at.dst_name)
                .and_then(|()| Ok(mkdirat(at.dst, at.dst_name, mode_bits(creation))?))
        }
        result => result.map_err(Into::into),
    };
    let opened = created.and_then(|()| {
        let dst = open_directory_file(at.dst, at.dst_name)?;
        Ok((DirId::of(&dst)?, dst))
    });
    let (dst_id, dst) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            // The subtree cannot be copied at all; skip it and continue with
            // the siblings.
            errors.push(format!(
                "Failed to create directory {}: {error}",
                compact(&paths.new)
            ));
            return None;
        }
    };
    context.created.insert(dst_id);
    // Abandons the subtree, leaving the destination directory empty and with
    // its final mode.
    let give_up = |errors: &mut Vec<String>, context: &CopyContext<'_>, message: String| {
        errors.push(message);
        finish_directory(None, &dst, &Source::of(stat), context);
    };
    let read_failure = |error: &dyn std::fmt::Display| {
        format!("Failed to read directory {}: {error}", compact(&paths.old))
    };
    let opened = open_directory_file(at.src, at.src_name).and_then(|src| Ok((fstat(&src)?, src)));
    let (opened, src) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            give_up(errors, context, read_failure(&error));
            return None;
        }
    };
    if (opened.st_dev, opened.st_ino) != (id.dev, id.ino) {
        let message = format!(
            "Cannot copy {}: it was replaced while being copied",
            compact(&paths.old)
        );
        give_up(errors, context, message);
        return None;
    }
    let names = match list_names(&src) {
        Ok(names) => names,
        Err(error) => {
            give_up(errors, context, read_failure(&error));
            return None;
        }
    };
    Some(CopyLevel {
        src: Some(src),
        dst: Some(dst),
        src_id: id,
        dst_id,
        source: Source::of(&opened),
        names: names.into_iter(),
    })
}

/// Gives a copied directory its final mode, and for a move the source's
/// times, now that its children are written: writing them is what moved its
/// own modification time, and a mode without owner-write would have stopped
/// them being created. Through the handle, so a path swapped since cannot
/// redirect either.
fn finish_directory(src: Option<&File>, dst: &File, source: &Source, context: &CopyContext<'_>) {
    if context.preserve
        && let Some(src) = src
    {
        apply_times(src, dst);
    }
    apply_final_mode(dst, source, context);
}

/// Copies the non-directory entry `at` names, of the type `stat` gives it: a
/// symlink, a regular file, or a special file. Returns `false` only when
/// cancelled.
fn copy_entry(
    at: &At<'_>,
    paths: &Paths,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &rustix::fs::Stat,
) -> bool {
    // The type comes from an `lstat`, so a symlink (even one pointing at a
    // directory) is recreated as a link rather than followed.
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => {
            copy_symlink(at, paths, errors, context);
            true
        }
        FileType::RegularFile => copy_file(at, paths, active, errors, context),
        _ => {
            copy_special(at, paths, errors, context, stat);
            true
        }
    }
}

/// Recreates the symlink `at` names, pointing at the same (possibly relative,
/// possibly dangling) target. The target is never followed, so no bytes are
/// transferred and no permissions are applied.
fn copy_symlink(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) {
    let target = match readlinkat(at.src, at.src_name, Vec::new()) {
        Ok(read) => read,
        Err(error) => {
            errors.push(format!(
                "Failed to read symlink {}: {error}",
                compact(&paths.old)
            ));
            return;
        }
    };
    let created = match symlinkat(&target, at.dst, at.dst_name) {
        // Raced; settled from the standing answer, or recorded.
        Err(Errno::EXIST) => {
            if !resolve_nested(context, errors, at.dst, at.dst_name, &paths.new) {
                return;
            }
            remove_existing_at(at.dst, at.dst_name)
                .and_then(|()| Ok(symlinkat(&target, at.dst, at.dst_name)?))
                .map_err(|error| format!("Failed to replace {}: {error}", compact(&paths.new)))
        }
        result => result
            .map_err(|error| format!("Failed to create symlink {}: {error}", compact(&paths.new))),
    };
    if let Err(message) = created {
        errors.push(message);
    }
}

/// Copies a file chunk-by-chunk, sending debounced progress updates via
/// `active`. The destination is never more readable than the source while it
/// is written (see `create_file_at`), and gets its final mode through the
/// handle once the copy stops, however it stops. Failures are recorded in
/// `errors`; returns `false` only when cancelled.
///
/// The mode and owner applied are those of the file opened.
fn copy_file(
    at: &At<'_>,
    paths: &Paths,
    active: &mut ActiveTask,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
) -> bool {
    let failed = |error: &dyn std::fmt::Display| {
        format!(
            "Failed to copy {} to {}: {error}",
            compact(&paths.old),
            compact(&paths.new)
        )
    };
    // Opened before the destination is created, so a source that cannot be
    // read leaves nothing behind.
    let opened = match context.source.take() {
        Some(file) => Ok(file),
        None => open_source_file(at.src, at.src_name),
    };
    let opened = opened.and_then(|file| Ok((fstat(&file)?, file)));
    let (stat, mut old_file) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };
    let source = &Source::of(&stat);
    let mut new_file = match create_file_at(at.dst, at.dst_name, source.mode, context.preserve) {
        Ok(file) => file,
        // A name already taken inside the tree being copied. The top-level
        // collision was answered before the task started, so this one is a
        // race, settled from the paste's standing answer or recorded.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !resolve_nested(context, errors, at.dst, at.dst_name, &paths.new) {
                return true;
            }
            match remove_existing_at(at.dst, at.dst_name)
                .and_then(|()| create_file_at(at.dst, at.dst_name, source.mode, context.preserve))
            {
                Ok(file) => file,
                Err(error) => {
                    errors.push(format!(
                        "Failed to replace {}: {error}",
                        compact(&paths.new)
                    ));
                    return true;
                }
            }
        }
        Err(error) => {
            errors.push(failed(&error));
            return true;
        }
    };

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
                if context.preserve {
                    apply_times(&old_file, &new_file);
                }
                break true;
            }
            Ok(bytes) => match new_file.write_all(&context.buffer[..bytes]) {
                Ok(()) => {
                    active.increment(bytes as u64);
                    if context
                        .progress
                        .should_trigger(Instant::now(), bytes as u64)
                    {
                        active.send_progress();
                    }
                }
                Err(error) => {
                    errors.push(format!("Failed to write {}: {error}", compact(&paths.new)));
                    break true;
                }
            },
            Err(error) => {
                errors.push(format!("Failed to read {}: {error}", compact(&paths.old)));
                break true;
            }
        }
    };
    apply_final_mode(&new_file, source, context);
    not_cancelled
}

/// Recreates a special file (FIFO, socket, or device node) as a fresh node
/// with the source's permission bits, like `cp -R` does. No bytes are
/// transferred: reading a FIFO would block until a writer appears. FIFOs and
/// sockets need no privileges; device nodes require root, so as a normal user
/// they record a "not permitted" error here, exactly as `cp` reports.
fn copy_special(
    at: &At<'_>,
    paths: &Paths,
    errors: &mut Vec<String>,
    context: &mut CopyContext<'_>,
    stat: &rustix::fs::Stat,
) {
    let file_type = FileType::from_raw_mode(stat.st_mode);
    if !matches!(
        file_type,
        FileType::Fifo | FileType::Socket | FileType::BlockDevice | FileType::CharacterDevice
    ) {
        errors.push(format!(
            "Cannot copy {}: unsupported file type",
            compact(&paths.old)
        ));
        return;
    }
    // Everything comes from the one `lstat` the type came from. Device nodes
    // need the source's device numbers; the rest take zero.
    let make = || make_node(at, paths, file_type, stat_mode(stat), stat.st_rdev);
    let created = match make() {
        // Raced; settled from the standing answer, or recorded.
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            if !resolve_nested(context, errors, at.dst, at.dst_name, &paths.new) {
                return;
            }
            remove_existing_at(at.dst, at.dst_name)
                .and_then(|()| make())
                .map_err(|error| format!("Failed to replace {}: {error}", compact(&paths.new)))
        }
        result => result.map_err(|error| {
            format!(
                "Failed to create special file {}: {error}",
                compact(&paths.new)
            )
        }),
    };
    if let Err(message) = created {
        errors.push(message);
    }
}

/// Creates the node `at` names in the destination directory.
#[cfg(not(target_os = "macos"))]
fn make_node(
    at: &At<'_>,
    _paths: &Paths,
    file_type: FileType,
    source_mode: u32,
    device: rustix::fs::Dev,
) -> std::io::Result<()> {
    let device = if matches!(file_type, FileType::BlockDevice | FileType::CharacterDevice) {
        device
    } else {
        0
    };
    Ok(rustix::fs::mknodat(
        at.dst,
        at.dst_name,
        file_type,
        mode_bits(source_mode & 0o777),
        device,
    )?)
}

/// Creates the node `at` names, by path: macOS has no `mknodat`. A parent
/// swapped for a symlink could therefore place the node outside the tree, but
/// an empty FIFO or socket there carries nothing, and a device node needs root.
#[cfg(target_os = "macos")]
fn make_node(
    _at: &At<'_>,
    paths: &Paths,
    file_type: FileType,
    source_mode: u32,
    device: rustix::fs::Dev,
) -> std::io::Result<()> {
    use nix::sys::stat::{Mode as NixMode, SFlag, mknod};

    let (kind, device) = match file_type {
        FileType::Fifo => (SFlag::S_IFIFO, 0),
        FileType::Socket => (SFlag::S_IFSOCK, 0),
        FileType::BlockDevice => (SFlag::S_IFBLK, device),
        _ => (SFlag::S_IFCHR, device),
    };
    // `mode_t` is u16 on macOS; the permission bits fit.
    #[allow(clippy::cast_possible_truncation)]
    let permissions = NixMode::from_bits_truncate((source_mode & 0o777) as nix::libc::mode_t);
    Ok(mknod(&paths.new, kind, permissions, device)?)
}

/// `st_mode` as a u32: it is u32 on Linux but u16 on macOS.
#[allow(clippy::useless_conversion)]
fn stat_mode(stat: &rustix::fs::Stat) -> u32 {
    u32::from(stat.st_mode)
}

/// The file type bits of `mode`, in the width `FileType::from_raw_mode` takes.
fn raw_type(mode: u32) -> rustix::fs::RawMode {
    // `mode_t` is u32 on Linux but u16 on macOS; the type bits fit either.
    #[allow(clippy::cast_possible_truncation)]
    let raw = (mode & 0o170_000) as rustix::fs::RawMode;
    raw
}

/// The permission bits of `mode` as a `Mode`.
fn mode_bits(mode: u32) -> Mode {
    // As in `raw_type`: the permission bits fit a u16 `mode_t`.
    #[allow(clippy::cast_possible_truncation)]
    let raw = (mode & 0o7777) as rustix::fs::RawMode;
    Mode::from_bits_truncate(raw)
}

/// Opens a regular file to copy from. `O_NOFOLLOW` refuses a symlink swapped
/// in since the entry was listed, and `O_NONBLOCK` keeps a FIFO swapped in
/// from blocking the open (and with it the worker every operation shares);
/// anything that is not a regular file is then refused before a byte is read,
/// so a device such as `/dev/zero` cannot be read without end either.
fn open_source_file(dir: impl AsFd, name: &CStr) -> std::io::Result<File> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = openat(dir, name, flags, Mode::empty())?;
    if FileType::from_raw_mode(fstat(&fd)?.st_mode) != FileType::RegularFile {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "it is no longer a regular file",
        ));
    }
    fcntl_setfl(&fd, fcntl_getfl(&fd)? - OFlags::NONBLOCK)?;
    Ok(File::from(fd))
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
fn create_file_at(
    dir: impl AsFd,
    name: &CStr,
    source_mode: u32,
    preserve: bool,
) -> std::io::Result<File> {
    let creation = if preserve {
        (source_mode & 0o700) | 0o600
    } else {
        source_mode & 0o777
    };
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(File::from(openat(dir, name, flags, mode_bits(creation))?))
}

/// Applies the mode a copied entry ends with, through its handle.
///
/// A copy keeps the permission bits the umask left at creation, and never the
/// setuid, setgid or sticky bits, like `cp` without `-p`: a file copied into a
/// directory someone else can reach must not become a setuid program of the
/// user who copied it. The creation mode had owner access added so the entry
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
fn apply_final_mode(file: &File, source: &Source, context: &CopyContext<'_>) {
    if context.preserve {
        let _ = rustix::fs::fchown(file, None, Some(rustix::fs::Gid::from_raw(source.gid)));
    }
    let Ok(created) = fstat(file) else {
        return;
    };
    let created_mode = stat_mode(&created) & 0o777;
    let mode = if context.preserve {
        let special = if created.st_uid == source.uid && created.st_gid == source.gid {
            0o7000
        } else {
            0o1000
        };
        source.mode & (0o777 | special)
    } else {
        (created_mode & 0o077) | (source.mode & created_mode & 0o700)
    };
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(mode)) {
        warn!("Failed to set permissions on a copied entry: {error}");
    }
}

/// Copies `source`'s access and modification times onto `target`, both open.
/// Best effort: a filesystem that cannot record them is not a reason to fail
/// the operation.
fn apply_times(source: &File, target: &File) {
    let Ok(metadata) = source.metadata() else {
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
        warn!("Failed to set times on a copied entry: {error}");
    }
}

/// Removes an existing non-directory entry `name` in `dir`, treating one that
/// is already gone as success.
fn remove_existing_at(dir: impl AsFd, name: &CStr) -> std::io::Result<()> {
    match unlinkat(dir, name, AtFlags::empty()) {
        Err(Errno::NOENT) => Ok(()),
        result => Ok(result?),
    }
}

/// Opens the directory `path` is in, following symlinks: the top-level
/// directories are the ones the user chose, however they are reached.
fn open_parent(path: &Path) -> std::io::Result<File> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
    Ok(File::from(openat(CWD, parent, flags, Mode::empty())?))
}

/// `path`'s file name, for the `*at` calls.
fn c_name(path: &Path) -> Option<CString> {
    CString::new(path.file_name()?.as_bytes()).ok()
}

/// Opens the directory `name` in `parent` as a `File`, without following a
/// symlink (see `open_directory`).
fn open_directory_file(parent: impl AsFd, name: impl rustix::path::Arg) -> std::io::Result<File> {
    Ok(File::from(open_directory_fd(parent, name)?))
}

/// Reopens the parent of the open directory `dir` through its "..", refusing it
/// unless it is the directory `expected` identifies.
fn reopen_parent_of(dir: &File, expected: DirId) -> std::io::Result<File> {
    let parent = open_directory_file(dir, "..")?;
    if DirId::of(&parent)? != expected {
        return Err(std::io::Error::other(
            "it was moved while it was being copied",
        ));
    }
    Ok(parent)
}

/// The names in the open directory `dir`, read to the end. `copy_tree` checks
/// for a cancel between the entries.
fn list_names(dir: &File) -> std::io::Result<Vec<CString>> {
    let mut names = Vec::new();
    let mut entries = Dir::read_from(dir)?;
    while let Some(entry) = entries.read() {
        let entry = entry?;
        let name = entry.file_name();
        if name != c"." && name != c".." {
            names.push(name.to_owned());
        }
    }
    Ok(names)
}

/// An entry to copy, named relative to an open directory on each side.
struct At<'a> {
    src: &'a File,
    dst: &'a File,
    src_name: &'a CStr,
    dst_name: &'a CStr,
}

/// The paths of the entry being copied, for messages only: every system call
/// goes through an `At`. One pair for the whole walk, extended on the way down
/// and cut back on the way up, so memory does not grow with depth squared.
struct Paths {
    old: PathBuf,
    new: PathBuf,
}

impl Paths {
    fn push(&mut self, name: &CStr) {
        let name = OsStr::from_bytes(name.to_bytes());
        self.old.push(name);
        self.new.push(name);
    }

    fn pop(&mut self) {
        self.old.pop();
        self.new.pop();
    }
}

/// What the copy needs of a source entry: its mode, and its owner for deciding
/// whether a move keeps the setuid and setgid bits.
#[derive(Clone, Copy)]
struct Source {
    mode: u32,
    uid: u32,
    gid: u32,
}

impl Source {
    fn of(stat: &rustix::fs::Stat) -> Self {
        Self {
            mode: stat_mode(stat),
            uid: stat.st_uid,
            gid: stat.st_gid,
        }
    }
}

/// One directory on `copy_tree`'s stack: the open source and destination
/// (`None` while the walk is below it), their identities for reopening them,
/// the source's metadata for finishing it, and the names not yet copied.
struct CopyLevel {
    src: Option<File>,
    dst: Option<File>,
    src_id: DirId,
    dst_id: DirId,
    source: Source,
    names: std::vec::IntoIter<CString>,
}

impl CopyLevel {
    fn handles(&self) -> (&File, &File) {
        match (&self.src, &self.dst) {
            (Some(src), Some(dst)) => (src, dst),
            _ => unreachable!("the level being worked in holds its directories"),
        }
    }
}

/// Which operation `remove_path` finishes.
#[derive(Clone, Copy, PartialEq)]
enum Removal {
    /// A delete, which a cancel stops between entries and which advances the
    /// task's progress per entry.
    Delete,
    /// The source of a cross-device move, which is past the point where a
    /// cancel could stop it, and whose progress is already complete.
    MovedSource,
}

impl Removal {
    /// The refusal to remove `path`, which names another entry than the one
    /// the operation was started for.
    fn refusal(self, path: &Path) -> String {
        match self {
            Self::Delete => Stale::Changed.into_error("delete", path).to_string(),
            Self::MovedSource => format!(
                "Cannot remove {}: it was replaced after it was copied",
                compact(path)
            ),
        }
    }
}

/// Removes a file or directory tree. Cancelling mid-delete leaves whatever has
/// not been removed yet. Iterative (explicit stack), so directory depth cannot
/// overflow the thread stack.
///
/// Refuses unless `path` still names `expected`, the entry the operation was
/// started for: another renamed onto its name since would be removed in its
/// place. The comparison is made on what was opened relative to the parent's
/// fd, and every removal goes through that fd rather than the path. For a
/// non-directory the stat and the unlink are two calls on that fd, which
/// narrows the window rather than closing it, as it does for `mv`.
///
/// Returns `Some` with the task, leaving finalization to the caller. Returns
/// `None` when cancelled or on error, in which case the task has already been
/// finalized via `active.cancelled()` / `active.error()`.
fn remove_path(
    path: &Path,
    expected: DirId,
    is_directory: bool,
    mut active: ActiveTask,
    removal: Removal,
) -> Option<ActiveTask> {
    let cancellable = removal == Removal::Delete;
    if cancellable && active.is_cancelled() {
        active.cancelled();
        return None;
    }
    let Some(root_name) = c_name(path) else {
        active.error(format!(
            "Cannot delete {}: path has no file name",
            compact(path)
        ));
        return None;
    };
    let root_parent = try_or_abort!(
        active,
        open_parent(path),
        format!("Failed to delete {}", compact(path))
    );
    let root = RootAt {
        dir: &root_parent,
        name: &root_name,
    };
    if !is_directory {
        return remove_file_entry(path, &root, expected, active, removal);
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
    //
    // A level holds its name rather than its path, and a path is built from
    // the stack only for a message: one path per level would make memory grow
    // with the square of the depth.
    let (dir, id, entries) = match open_root(path, &root, expected, removal) {
        Ok(opened) => opened,
        Err(message) => {
            active.error(message);
            return None;
        }
    };
    let mut stack = vec![Level {
        dir: Some(dir),
        id,
        name: None,
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
        if cancellable && active.is_cancelled() {
            active.cancelled();
            return None;
        }
        let Some((name, is_dir)) = top.entries.next() else {
            // This directory's entries are done; remove it.
            let level = stack.pop().expect("stack is non-empty");
            match remove_level(path, &root, &mut stack, level) {
                Ok(()) => advance(&mut active, &mut debouncer, cancellable),
                Err((failed, error)) => {
                    active.error(format!("Failed to delete {}: {error}", compact(&failed)));
                    return None;
                }
            }
            continue;
        };
        let parent = stack
            .last()
            .and_then(|level| level.dir.as_ref())
            .expect("the level being worked in holds its fd");
        if is_dir {
            let (dir, entries) = list_or_abort!(
                active,
                Some(parent),
                &name,
                &entry_path(path, &stack, &name)
            );
            let id = try_or_abort!(
                active,
                DirId::of_dir(&dir),
                format!(
                    "Failed to read directory {}",
                    compact(&entry_path(path, &stack, &name))
                )
            );
            let top = stack.last_mut().expect("stack is non-empty");
            // Closed until the walk returns here, through `reopen_parent`.
            top.dir = None;
            stack.push(Level {
                dir: Some(dir),
                id,
                name: Some(name),
                entries: entries.into_iter(),
            });
            // Descending is not a removal, so it advances no progress.
            continue;
        }
        try_or_abort!(
            active,
            unlink(parent, &name, AtFlags::empty()),
            format!(
                "Failed to delete {}",
                compact(&entry_path(path, &stack, &name))
            )
        );
        advance(&mut active, &mut debouncer, cancellable);
    }
    Some(active)
}

/// Opens and lists the directory `root` names for `remove_path`, refusing it
/// unless it is `expected`. The error is the message to finalize with.
fn open_root(
    path: &Path,
    root: &RootAt<'_>,
    expected: DirId,
    removal: Removal,
) -> Result<(Dir, DirId, Entries), String> {
    let failed =
        |error: std::io::Error| format!("Failed to read directory {}: {error}", compact(path));
    let dir = open_directory(root.dir, root.name).map_err(failed)?;
    let id = DirId::of_dir(&dir).map_err(failed)?;
    if !is_same_entry(expected.pair(), id.pair(), || lists_stable_inodes(path)) {
        return Err(removal.refusal(path));
    }
    let (dir, entries) = list_dir(dir).map_err(failed)?;
    Ok((dir, id, entries))
}

/// Counts one entry removed by a delete. A move's removals count nothing: its
/// progress measured the bytes copied, and is complete.
fn advance(active: &mut ActiveTask, debouncer: &mut debounce::ProgressDebouncer, counts: bool) {
    if counts {
        active.increment(1);
        if debouncer.should_trigger(Instant::now(), 1) {
            active.send_progress();
        }
    }
}

/// `remove_path` for anything that is not a directory. Symlinks are removed as
/// links (never followed): `is_directory` comes from `symlink_metadata`, so a
/// link to a directory takes this path.
fn remove_file_entry(
    path: &Path,
    at: &RootAt<'_>,
    expected: DirId,
    mut active: ActiveTask,
    removal: Removal,
) -> Option<ActiveTask> {
    let failed = || format!("Failed to delete {}", compact(path));
    let stat = try_or_abort!(
        active,
        statat(at.dir, at.name, AtFlags::SYMLINK_NOFOLLOW),
        failed()
    );
    if !is_same_entry(expected.pair(), DirId::of_stat(&stat).pair(), || {
        lists_stable_inodes(path)
    }) {
        active.error(removal.refusal(path));
        return None;
    }
    try_or_abort!(
        active,
        unlinkat(at.dir, at.name, AtFlags::empty()),
        failed()
    );
    if removal == Removal::Delete {
        active.increment(1);
    }
    Some(active)
}

/// The path of `name` in the directory at the top of `stack`, for a message.
fn entry_path(root: &Path, stack: &[Level], name: &CStr) -> PathBuf {
    let mut path = level_path(root, stack);
    path.push(OsStr::from_bytes(name.to_bytes()));
    path
}

/// The path of the directory at the top of `stack`, for a message.
fn level_path(root: &Path, stack: &[Level]) -> PathBuf {
    let mut path = root.to_path_buf();
    for name in stack.iter().filter_map(|level| level.name.as_ref()) {
        path.push(OsStr::from_bytes(name.to_bytes()));
    }
    path
}

/// Removes the directory `level` names, now that its entries are gone, from
/// the parent at the top of `stack`, reopening that parent's fd first, or from
/// `root_at` for the root. The error carries the directory that could not be
/// reopened or removed.
fn remove_level(
    root: &Path,
    root_at: &RootAt<'_>,
    stack: &mut [Level],
    level: Level,
) -> Result<(), (PathBuf, std::io::Error)> {
    let Level { dir, name, .. } = level;
    let Some(parent) = stack.last() else {
        drop(dir);
        // Through the parent the root was opened in, which `rmdir` does not
        // follow a symlink out of.
        return unlinkat(root_at.dir, root_at.name, AtFlags::REMOVEDIR)
            .map_err(|error| (root.to_path_buf(), error.into()));
    };
    let name = name.expect("only the root has no name");
    let child = dir
        .as_ref()
        .expect("the level being worked in holds its fd");
    let reopened =
        reopen_parent(child, parent.id).map_err(|error| (level_path(root, stack), error))?;
    drop(dir);
    let parent = stack.last_mut().expect("the parent is on the stack");
    let removed = unlink(parent.dir.insert(reopened), &name, AtFlags::REMOVEDIR);
    removed.map_err(|error| (entry_path(root, stack, &name), error))
}

/// A directory's entries as `remove_path` lists them: each name, and whether it
/// is a directory to descend into.
type Entries = Vec<(CString, bool)>;

/// One directory on `remove_path`'s stack: the open directory its entries are
/// unlinked through (`None` while the walk is below it), its identity, its name
/// in the parent (`None` for the root), and the entries not yet removed.
struct Level {
    dir: Option<Dir>,
    id: DirId,
    name: Option<CString>,
    entries: <Entries as IntoIterator>::IntoIter,
}

/// The device and inode of an entry, to tell whether one reopened by name is
/// the one that was listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DirId {
    dev: rustix::fs::Dev,
    ino: u64,
}

impl DirId {
    fn of(dir: impl AsFd) -> std::io::Result<Self> {
        let stat = fstat(dir)?;
        Ok(Self {
            dev: stat.st_dev,
            ino: stat.st_ino,
        })
    }

    fn of_dir(dir: &Dir) -> std::io::Result<Self> {
        Self::of(dir.fd()?)
    }

    fn of_stat(stat: &rustix::fs::Stat) -> Self {
        Self {
            dev: stat.st_dev,
            ino: stat.st_ino,
        }
    }

    /// The (device, inode) pair `is_same_entry` compares.
    fn pair(self) -> (rustix::fs::Dev, u64) {
        (self.dev, self.ino)
    }

    /// The identity of the entry `listed` names, as it was read.
    // std widens `st_dev` to u64 on every target; narrowing it back to the
    // target's `dev_t` restores the value it was read as.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    fn of_listed(listed: &PathInfo) -> Self {
        let (dev, ino) = listed.device_and_inode();
        Self {
            dev: dev as rustix::fs::Dev,
            ino,
        }
    }
}

/// The directory an operation's root entry was opened in, and its name there.
struct RootAt<'a> {
    dir: &'a File,
    name: &'a CStr,
}

/// Reopens the parent of `dir` through its "..", refusing it unless it is the
/// directory `expected` identifies: a directory moved elsewhere during the walk
/// has a different "..", and following it would delete outside the tree.
fn reopen_parent(dir: &Dir, expected: DirId) -> std::io::Result<Dir> {
    let parent = open_directory(dir.fd()?, "..")?;
    if DirId::of_dir(&parent)? != expected {
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
    Ok(Dir::new(open_directory_fd(parent, name)?)?)
}

/// `open_directory`, as the fd itself.
fn open_directory_fd(
    parent: impl AsFd,
    name: impl rustix::path::Arg,
) -> std::io::Result<std::os::fd::OwnedFd> {
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    Ok(openat(parent, name, flags, Mode::empty())?)
}

/// `open_directory` then `list_entries`. A `None` parent resolves `name`
/// against the current directory, like a path.
fn open_and_list(
    parent: Option<&Dir>,
    name: impl rustix::path::Arg,
) -> std::io::Result<(Dir, Entries)> {
    let parent = match parent {
        Some(parent) => parent.fd()?,
        None => CWD,
    };
    list_dir(open_directory(parent, name)?)
}

/// `list_entries` on `dir`, which it hands back with them.
fn list_dir(mut dir: Dir) -> std::io::Result<(Dir, Entries)> {
    let entries = list_entries(&mut dir)?;
    Ok((dir, entries))
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
///
/// Never checked for cancellation: the callers check between entries, and the
/// removal of a moved source, which a cancel must not stop part way, lists
/// through here too.
fn list_entries(dir: &mut Dir) -> std::io::Result<Entries> {
    let mut entries = Vec::new();
    while let Some(entry) = dir.read() {
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
    Ok(entries)
}

/// Renames `old_path` to `new_path`, failing atomically with `AlreadyExists` if
/// `new_path` is taken, unlike `fs::rename`, which replaces it. A check made
/// before the rename leaves a window in which a file created at `new_path` is
/// silently replaced; folding the check into the rename closes it.
///
/// Linux uses `renameat2(RENAME_NOREPLACE)` and macOS `renameatx_np`, both
/// through rustix's safe wrapper, since `unsafe` is denied crate-wide. Other
/// targets, and filesystems that reject the flag, fall back to a check and
/// `fs::rename`, and keep the narrow race.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_no_replace(old_path: &Path, new_path: &Path) -> std::io::Result<()> {
    use rustix::{
        fs::{CWD, RenameFlags, renameat_with},
        io::Errno,
    };

    // `CWD` as both dirfds: absolute paths ignore it and relative paths resolve
    // against the current directory, matching `fs::rename`.
    match renameat_with(CWD, old_path, CWD, new_path, RenameFlags::NOREPLACE) {
        Ok(()) => Ok(()),
        Err(Errno::NOSYS | Errno::INVAL | Errno::NOTSUP) => checked_rename(old_path, new_path),
        // Preserve the errno (e.g. XDEV -> CrossesDevices, EXIST ->
        // AlreadyExists) so callers can dispatch on `error.kind()`.
        Err(errno) => Err(errno.into()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn rename_no_replace(old_path: &Path, new_path: &Path) -> std::io::Result<()> {
    checked_rename(old_path, new_path)
}

/// `rename_no_replace` where the kernel cannot refuse a taken name itself.
fn checked_rename(old_path: &Path, new_path: &Path) -> std::io::Result<()> {
    if new_path.symlink_metadata().is_ok() {
        return Err(ErrorKind::AlreadyExists.into());
    }
    fs::rename(old_path, new_path)
}

/// An absolute, display-friendly rendering of `path` for the operations
/// notice. Lexical only (no filesystem access), so it works for destination
/// paths that do not exist yet; falls back to the original path if it cannot
/// be absolutized.
fn display_path(path: &Path) -> String {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    crate::visible_os(path.as_os_str()).into_owned()
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
fn resolve_entry(path: &Path) -> PathBuf {
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
fn is_link_to(link: &Path, entry: &Path) -> bool {
    link.symlink_metadata()
        .is_ok_and(|metadata| metadata.is_symlink())
        && link
            .canonicalize()
            .is_ok_and(|target| target == resolve_entry(entry))
}

/// How pasting an entry as `destination` would paste it onto itself. Such a
/// paste is refused, never offered as a collision: a granted overwrite clears
/// the destination before copying, which would remove the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum OntoItself {
    /// Both paths name one entry: the destination directory is the source's
    /// own, however either is spelled.
    SameEntry,
    /// Two names of one file (hard links, or one entry spelled two ways on a
    /// case-insensitive mount): replacing one with the other either deletes
    /// the file or does nothing, depending on how the names relate. `cp` and
    /// `mv` refuse both as the same file.
    SameFile,
    /// A symlink pasted over the entry it points at, which would replace the
    /// only copy of its data with a link to itself. `cp` and `mv` refuse it as
    /// the same file.
    LinksTo,
}

/// Whether pasting `source` as `destination` would paste it onto itself. The
/// paste queue and the task both ask this, so the prompt never offers to
/// replace what the task then refuses. Paths are compared resolved, so neither
/// a parent-dir segment (e.g. `/a/c/../b`) nor a symlinked directory can
/// disguise one as the other; a symlink in the last component is compared as
/// the link it is.
pub(super) fn onto_itself(source: &Path, destination: &Path) -> Option<OntoItself> {
    if resolve_entry(source) == resolve_entry(destination) {
        Some(OntoItself::SameEntry)
    } else if is_same_file(source, destination) {
        Some(OntoItself::SameFile)
    } else if is_link_to(source, destination) {
        Some(OntoItself::LinksTo)
    } else {
        None
    }
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

    // Compared resolved (see `onto_itself`), so a destination that reaches
    // into the source through a symlinked parent is still found below.
    let abs_old = resolve_entry(&old_path);
    let abs_new = resolve_entry(&new_path);

    if let Some(onto_itself) = onto_itself(&old_path, &new_path) {
        let error = match onto_itself {
            // Equal resolved paths mean the destination directory is the
            // source's own, so the message names the entry once.
            OntoItself::SameEntry => anyhow!(
                "Cannot {operation} {} into its own directory",
                compact(&old_path)
            ),
            OntoItself::SameFile => anyhow!(
                "Cannot {operation} {} to {}: they are the same file",
                compact(&old_path),
                compact(&new_path)
            ),
            OntoItself::LinksTo => anyhow!(
                "Cannot {operation} {}: it links to {}, the entry it would replace",
                compact(&old_path),
                compact(&new_path)
            ),
        };
        return Err(error.into());
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
    dir: impl AsFd,
    name: &CStr,
    new_path: &Path,
) -> bool {
    // A directory is never replaced, so only the skip choices apply to one,
    // exactly as at the top level.
    let is_directory = statat(dir, name, AtFlags::SYMLINK_NOFOLLOW)
        .is_ok_and(|stat| FileType::from_raw_mode(stat.st_mode) == FileType::Directory);
    let occupant = if is_directory {
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
        let name = c_name(old_path).ok_or_else(|| std::io::Error::from(ErrorKind::InvalidInput));
        match open_parent(old_path).and_then(|parent| open_source_file(&parent, &name?)) {
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
    fn display_path_spells_out_a_byte_that_is_not_utf8() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        // A lossy rendering would show U+FFFD, the same as for any other
        // invalid byte, or for a name holding U+FFFD itself.
        let path = Path::new(OsStr::from_bytes(b"/a/caf\xe9.txt"));

        assert_eq!("/a/caf\\xe9.txt", display_path(path));
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
        CopyContext::new(buffer, None, preserve_times, None, 0)
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

    /// The single alert a task refused before it started carried.
    fn refusal(result: Result<Task, Vec<Command>>) -> String {
        let Err(commands) = result else {
            panic!("the task should have been refused");
        };
        let [Command::AlertError(message)] = commands.as_slice() else {
            panic!("expected one alert, got {commands:?}");
        };
        message.clone()
    }

    /// The row seen was `sub/notes.txt`; `sub` is then swapped for a link to
    /// another directory holding a file of the same name. The path is resolved
    /// again when the task starts, and then names that file, which the user
    /// never saw.
    #[test_case("copy" ; "a copy")]
    #[test_case("move" ; "a move")]
    #[test_case("delete" ; "a delete")]
    fn a_task_refuses_a_source_whose_parent_was_swapped_since_it_was_listed(verb: &str) {
        let fx = TempDir::new("tasks_parent_swapped");
        let sub = fx.join("sub");
        let other = fx.join("other");
        let dest = fx.join("dest");
        for dir in [&sub, &other, &dest] {
            fs::create_dir(dir).unwrap();
        }
        fs::write(sub.join("notes.txt"), b"seen").unwrap();
        fs::write(other.join("notes.txt"), b"unseen").unwrap();
        let listed = PathInfo::try_from(sub.join("notes.txt").as_path()).unwrap();
        fs::rename(&sub, fx.join("sub.orig")).unwrap();
        std::os::unix::fs::symlink(&other, &sub).unwrap();
        let destination = PathInfo::try_from(dest.as_path()).unwrap();
        let task = match verb {
            "copy" => TaskCommand::Copy(listed, destination, false),
            "move" => TaskCommand::Move(listed, destination, false),
            _ => TaskCommand::Delete(listed),
        };

        let message = refusal(run_to_end(task));

        assert!(message.starts_with(&format!("Cannot {verb}")), "{message}");
        assert!(
            message.ends_with("it changed since it was listed"),
            "{message}"
        );
        assert_eq!(
            b"unseen".to_vec(),
            fs::read(other.join("notes.txt")).unwrap()
        );
        assert!(dest.join("notes.txt").symlink_metadata().is_err());
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

    /// A copy context whose collisions are settled by a standing choice, which
    /// is the only kind of answer that reaches a worker.
    fn answered_context<'a>(
        standing: ConflictChoice,
        buffer: &'a mut [u8],
        conflicts: &'a Conflicts,
    ) -> CopyContext<'a> {
        conflicts.answer(standing);
        CopyContext::new(buffer, Some(conflicts), false, None, 0)
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
        let mut context = CopyContext::new(&mut buffer, Some(&conflicts), false, None, 0);

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
        let mut context = CopyContext::new(&mut buffer, Some(&conflicts), false, None, 0);

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

    /// The identity of the entry `path` names now.
    fn id_of(path: &Path) -> DirId {
        DirId::of_stat(&statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW).unwrap())
    }

    /// A clean outcome of copying the entry `path` names now.
    fn copied(path: &Path) -> CopyOutcome {
        CopyOutcome {
            root: Some(id_of(path)),
            ..CopyOutcome::default()
        }
    }

    #[test]
    fn a_cross_device_move_removes_its_source_after_a_clean_copy() {
        let (_fx, src, rx, active) = moved("tasks_move_complete");

        finish_cross_device_move(active, copied(&src), &src, true);

        assert!(!src.exists());
        assert_eq!(None, finished_task(&rx).error_message());
    }

    /// Once the copy is complete a cancel can no longer stop the move: the
    /// cancel key checks the stage first, but one that reaches the token
    /// anyway must not leave the source part removed, split between two
    /// places.
    #[test]
    fn a_cross_device_move_removes_its_whole_source_even_when_cancelled() {
        let (_fx, src, _, _) = moved("tasks_move_cancelled");
        fs::create_dir(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("b.txt"), b"src").unwrap();
        let (tx, rx) = mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Move(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        token.cancel();

        finish_cross_device_move(active, copied(&src), &src, true);

        assert!(src.symlink_metadata().is_err());
        let task = finished_task(&rx);
        assert!(!task.is_cancelled());
        assert_eq!(None, task.error_message());
    }

    /// Something renames another directory onto the source's name once the
    /// copy is done. That one was never copied, so it is kept.
    #[test]
    fn a_cross_device_move_keeps_a_source_replaced_after_the_copy() {
        let (fx, src, rx, active) = moved("tasks_move_replaced");
        let outcome = copied(&src);
        fs::rename(&src, fx.join("copied")).unwrap();
        fs::create_dir(&src).unwrap();
        fs::write(src.join("other.txt"), b"other").unwrap();

        finish_cross_device_move(active, outcome, &src, true);

        assert!(src.join("other.txt").exists());
        let message = finished_task(&rx).error_message().expect("the move fails");
        assert!(
            message.ends_with("it was replaced after it was copied"),
            "{message}"
        );
    }

    #[test]
    fn a_cross_device_move_keeps_its_source_when_an_entry_was_skipped() {
        let (_fx, src, rx, active) = moved("tasks_move_skipped");

        finish_cross_device_move(
            active,
            CopyOutcome {
                skipped: 1,
                ..CopyOutcome::default()
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
                ..CopyOutcome::default()
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

    /// A move's file is created owner-only and gets the source's other bits
    /// once the copy stops; a copy's gets no more than the source has, which
    /// the umask trims. Either way a file still being written is readable by
    /// no one the source is not.
    #[test_case(true => 0o600 ; "a move is created owner only")]
    #[test_case(false => 0 ; "a copy is created with no bit the source lacks")]
    fn a_copied_file_is_never_more_readable_than_its_source_while_written(preserve: bool) -> u32 {
        let fx = TempDir::new("tasks_create_file_mode");
        let dir = open_parent(&fx.join("dst.txt")).unwrap();

        let _file = create_file_at(&dir, c"dst.txt", 0o100_640, preserve).unwrap();

        let mode = mode_of(&fx.join("dst.txt")) & 0o7777;
        if preserve { mode } else { mode & !0o640 }
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

    /// The fallback for a filesystem that rejects the no-replace flag, which
    /// `fs::rename` alone would turn into a silent replace.
    #[test]
    fn the_checked_rename_refuses_a_taken_name() {
        let fx = TempDir::new("tasks_checked_rename");
        let src = fx.join("a.txt");
        fs::write(&src, b"a").unwrap();
        let dst = fx.join("b.txt");
        fs::write(&dst, b"b").unwrap();

        let error = checked_rename(&src, &dst).unwrap_err();

        assert_eq!(ErrorKind::AlreadyExists, error.kind());
        assert_eq!(b"b".to_vec(), fs::read(&dst).unwrap());
        assert!(src.exists());
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

        assert!(remove_path(&root, id_of(&root), true, active, Removal::Delete).is_some());
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

    #[test]
    fn a_tree_of_small_files_sends_progress_per_share_of_the_total_not_per_file() {
        const FILES: usize = 1000;
        let fx = TempDir::new("tasks_copy_tree_progress");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        for i in 0..FILES {
            fs::write(src.join(i.to_string()), [7u8; 10]).unwrap();
        }
        let (tx, rx) = mpsc::channel();
        let active = copy_task(tx);

        let start = Instant::now();
        let (active, outcome) = copy_with_progress(
            &src,
            &fx.join("dst"),
            active,
            None,
            0,
            true,
            mode_of(&src),
            false,
            None,
            1024,
            1024,
        )
        .expect("not cancelled");
        let elapsed = start.elapsed();
        active.done();
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        let updates: Vec<Progress> = rx
            .try_iter()
            .filter_map(|command| match command {
                Command::Progress(task) if !task.is_terminal() => {
                    Some(task.combine_progress(&Progress::default()))
                }
                _ => None,
            })
            .collect();

        // `set_total` sends one, the first chunk another, and the floor admits
        // one more per interval elapsed. One per file would be a thousand.
        let bound = 2 + elapsed.as_millis() / PROGRESS_MIN_INTERVAL.as_millis();
        assert!(
            (updates.len() as u128) <= bound,
            "{} updates in {elapsed:?} for {FILES} files",
            updates.len()
        );
        // The share is of the tree's bytes, so every update reports that total,
        // and the first chunk counts toward it rather than filling the bar.
        let total = FILES as u64 * 10;
        assert!(
            updates.iter().all(|progress| progress.total == total),
            "{updates:?}"
        );
        let [first, chunk, ..] = updates.as_slice() else {
            panic!("expected the total and the first chunk, got {updates:?}");
        };
        assert_eq!(0, first.completed);
        assert_eq!(10, chunk.completed);
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
        remove_path(&entry, id_of(&entry), false, active, Removal::Delete)
            .expect("the entry should be removed")
            .done();

        assert!(entry.symlink_metadata().is_err());
        assert_eq!(
            b"keep".to_vec(),
            fs::read(outside.join("keep.txt")).unwrap()
        );
    }

    /// The entry a task was started for is renamed away and another put in
    /// its place before the removal opens it, which is the window between a
    /// task's check at the start and its worker. The removal compares what it
    /// opened and leaves the replacement alone.
    #[test_case(true  ; "a directory")]
    #[test_case(false ; "a file")]
    fn remove_path_refuses_an_entry_replaced_before_it_was_opened(is_directory: bool) {
        let fx = TempDir::new("tasks_delete_replaced");
        let entry = fx.join("entry");
        let replacement = fx.join("replacement");
        if is_directory {
            fs::create_dir(&entry).unwrap();
            fs::create_dir(&replacement).unwrap();
            fs::write(replacement.join("keep.txt"), b"keep").unwrap();
        } else {
            fs::write(&entry, b"listed").unwrap();
            fs::write(&replacement, b"keep").unwrap();
        }
        let expected = id_of(&entry);
        // Renamed away rather than removed, so the replacement cannot reuse
        // the inode.
        fs::rename(&entry, fx.join("renamed")).unwrap();
        fs::rename(&replacement, &entry).unwrap();
        let (tx, rx) = mpsc::channel();
        let (active, _, _) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );

        assert!(remove_path(&entry, expected, is_directory, active, Removal::Delete).is_none());

        let message = finished_task(&rx)
            .error_message()
            .expect("the delete fails");
        assert!(
            message.ends_with("it changed since it was listed"),
            "{message}"
        );
        let kept = if is_directory {
            entry.join("keep.txt")
        } else {
            entry.clone()
        };
        assert_eq!(b"keep".to_vec(), fs::read(kept).unwrap());
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

        let active = remove_path(&root, id_of(&root), true, active, Removal::Delete)
            .expect("the tree should be removed");
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

        assert!(remove_path(&root, id_of(&root), true, active, Removal::Delete).is_none());
        assert!(root.join("sub").join("f.txt").exists());
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

        let finished = remove_path(&root, id_of(&root), true, active, Removal::Delete);

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
        let expected = DirId::of_dir(&open_directory(CWD, &parent).unwrap()).unwrap();
        let child = open_directory(CWD, parent.join("child")).unwrap();

        let reopened = reopen_parent(&child, expected).unwrap();

        assert_eq!(expected, DirId::of_dir(&reopened).unwrap());
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
        let expected = DirId::of_dir(&open_directory(CWD, &parent).unwrap()).unwrap();
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

        assert!(remove_path(&root, id_of(&root), true, active, Removal::Delete).is_some());

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

    /// The permission bits the umask leaves of `0o777`. Probed rather than
    /// read, since reading the umask means setting it for every thread.
    fn umask_leaves(dir: &Path) -> u32 {
        use std::os::unix::fs::OpenOptionsExt;
        let probe = dir.join("umask_probe");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o777)
            .open(&probe)
            .unwrap();
        let leaves = mode_of(&probe) & 0o777;
        fs::remove_file(&probe).unwrap();
        leaves
    }

    /// Copies `src` to `dst` as a copy, or as a move's copy stage when
    /// `preserve`, returning the errors recorded.
    fn copy_one(src: &Path, dst: &Path, preserve: bool) -> Vec<String> {
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let is_directory = fs::symlink_metadata(src).unwrap().is_dir();
        assert!(copy_path(
            src,
            dst,
            &mut active,
            &mut errors,
            &mut context(preserve, &mut [0u8; 64]),
            is_directory,
            mode_of(src),
        ));
        active.done();
        errors
    }

    // A copy is `cp` without `-p`: the umask applies and the special bits go,
    // so a file copied where others can reach it is not a setuid program of
    // the user who copied it. A move is `mv`, which keeps the mode.
    #[test_case(0o4755, false ; "a copy drops setuid")]
    #[test_case(0o2755, false ; "a copy drops setgid")]
    #[test_case(0o777, false ; "a copy takes the umask")]
    #[test_case(0o4755, true ; "a move keeps setuid on the user's own file")]
    #[test_case(0o777, true ; "a move ignores the umask")]
    fn a_copied_file_keeps_the_special_bits_and_ignores_the_umask_only_when_moved(
        mode: u32,
        preserve: bool,
    ) {
        let fx = TempDir::new("tasks_file_mode");
        let src = fx.join("src");
        fs::write(&src, b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(&src, &fx.join("dst"), preserve);

        assert!(errors.is_empty(), "{errors:?}");
        let expected = if preserve {
            mode
        } else {
            mode & 0o777 & umask_leaves(fx.path())
        };
        assert_eq!(expected, mode_of(&fx.join("dst")) & 0o7777);
    }

    // A directory is created owner-writable so its children can be, then
    // given its mode: a copy's owner bits come back to what the source had.
    #[test_case(0o555, false ; "a copy of a read only directory")]
    #[test_case(0o1777, false ; "a copy drops the sticky bit and takes the umask")]
    #[test_case(0o555, true ; "a move of a read only directory")]
    #[test_case(0o1777, true ; "a move keeps the sticky bit")]
    fn a_copied_directory_ends_with_the_mode_of_its_kind_of_copy(mode: u32, preserve: bool) {
        let fx = TempDir::new("tasks_directory_mode");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("child"), b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(&src, &fx.join("dst"), preserve);

        assert!(errors.is_empty(), "{errors:?}");
        assert!(fx.join("dst").join("child").exists());
        let expected = if preserve {
            mode
        } else {
            mode & 0o777 & umask_leaves(fx.path())
        };
        assert_eq!(expected, mode_of(&fx.join("dst")) & 0o7777);
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// `mv` clears setuid and setgid when it cannot carry the ownership over,
    /// or a program would run as whoever moved it. Another owner cannot be
    /// arranged without root, so the rule is driven with a source that names
    /// one.
    #[test]
    fn a_move_drops_setuid_and_setgid_from_another_users_file() {
        let fx = TempDir::new("tasks_move_other_owner");
        let file = File::create(fx.join("dst")).unwrap();
        let stat = fstat(&file).unwrap();
        let source = Source {
            mode: 0o100_6755,
            uid: stat.st_uid.wrapping_add(1),
            gid: stat.st_gid,
        };

        apply_final_mode(&file, &source, &context(true, &mut [0u8; 1]));

        assert_eq!(0o755, mode_of(&fx.join("dst")) & 0o7777);
    }

    /// A move keeps the group bits, like `mv`, so it keeps the group they were
    /// granted to as well: the copy is otherwise created with whichever group
    /// the destination assigns, and a file readable by one group would become
    /// readable by another. Needs a second group to belong to, which
    /// `getgroups` reports on Linux.
    #[cfg(target_os = "linux")]
    #[test_case(false, 0o640 ; "a file")]
    #[test_case(true, 0o750 ; "a directory")]
    fn a_move_keeps_the_group_of_its_source(is_directory: bool, mode: u32) {
        use std::os::unix::fs::MetadataExt;

        let own = nix::unistd::getegid();
        let Some(other) = nix::unistd::getgroups()
            .unwrap()
            .into_iter()
            .find(|group| *group != own)
        else {
            eprintln!("skipped: the user running the tests belongs to one group only");
            return;
        };
        let fx = TempDir::new("tasks_move_group");
        let src = fx.join("src");
        if is_directory {
            fs::create_dir(&src).unwrap();
        } else {
            fs::write(&src, b"x").unwrap();
        }
        std::os::unix::fs::chown(&src, None, Some(other.as_raw())).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(mode)).unwrap();

        let errors = copy_one(&src, &fx.join("dst"), true);

        assert!(errors.is_empty(), "{errors:?}");
        let copied = fs::symlink_metadata(fx.join("dst")).unwrap();
        assert_eq!(other.as_raw(), copied.gid());
        assert_eq!(mode, copied.mode() & 0o7777);
    }

    /// Another user renames a directory the copy created and leaves a link to
    /// one of the victim's in its place. The copy continues in the directory it
    /// created, and neither writes into nor changes the mode of the victim's.
    #[test]
    fn a_destination_directory_swapped_for_a_symlink_is_not_written_through() {
        let fx = TempDir::new("tasks_destination_swapped");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("planted.txt"), b"x").unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o777)).unwrap();
        let victim = fx.join("victim");
        fs::create_dir(&victim).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o700)).unwrap();
        let dst = fx.join("dst");
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let mut buffer = [0u8; 64];
        // A move, whose final mode is the source's 0o777 whatever the umask.
        let mut context = context(true, &mut buffer);
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
        let source = statat(&src_parent, c"src", AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let mut paths = Paths {
            old: src.clone(),
            new: dst.clone(),
        };
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let level = enter_directory(&at, &paths, &mut errors, &mut context, &source)
            .expect("the directory should be entered");

        fs::rename(&dst, fx.join("moved")).unwrap();
        std::os::unix::fs::symlink(&victim, &dst).unwrap();
        assert!(copy_tree(
            level,
            &mut paths,
            &mut active,
            &mut errors,
            &mut context
        ));
        active.done();

        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(0o700, mode_of(&victim) & 0o7777);
        assert!(fs::read_dir(&victim).unwrap().next().is_none());
        assert!(fx.join("moved").join("planted.txt").exists());
        assert_eq!(0o777, mode_of(&fx.join("moved")) & 0o7777);
    }

    /// The source's own owner swaps a directory the copy has listed for a link
    /// to one outside the tree. The link is copied as a link, and nothing
    /// outside the tree is read.
    #[test]
    fn a_source_directory_swapped_for_a_symlink_is_copied_as_the_link() {
        let fx = TempDir::new("tasks_source_swapped");
        let src = fx.join("src");
        fs::create_dir_all(src.join("sub")).unwrap();
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"secret").unwrap();
        let dst = fx.join("dst");
        let (tx, _rx) = mpsc::channel();
        let mut active = copy_task(tx);
        let mut errors = Vec::new();
        let mut buffer = [0u8; 64];
        let mut context = context(false, &mut buffer);
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&dst).unwrap());
        let source = statat(&src_parent, c"src", AtFlags::SYMLINK_NOFOLLOW).unwrap();
        let mut paths = Paths {
            old: src.clone(),
            new: dst.clone(),
        };
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let level = enter_directory(&at, &paths, &mut errors, &mut context, &source)
            .expect("the directory should be entered");

        fs::rename(src.join("sub"), fx.join("sub.orig")).unwrap();
        std::os::unix::fs::symlink(&outside, src.join("sub")).unwrap();
        assert!(copy_tree(
            level,
            &mut paths,
            &mut active,
            &mut errors,
            &mut context
        ));
        active.done();

        assert!(errors.is_empty(), "{errors:?}");
        assert!(dst.join("sub").symlink_metadata().unwrap().is_symlink());
        assert_eq!(outside, fs::read_link(dst.join("sub")).unwrap());
    }

    /// A regular file swapped for something else between being listed and
    /// being opened. A FIFO would block the open, and with it the one worker
    /// every operation shares; a link to a device would be read without end.
    #[test_case("fifo" ; "a fifo")]
    #[test_case("link" ; "a symlink to a device")]
    #[test_case("file_link" ; "a symlink to a file outside the tree")]
    #[test_case("dir" ; "a directory")]
    fn a_source_file_that_is_no_longer_a_regular_file_is_refused_at_once(name: &'static str) {
        let fx = TempDir::new("tasks_source_not_regular");
        nix::unistd::mkfifo(
            &fx.join("fifo"),
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        )
        .unwrap();
        std::os::unix::fs::symlink("/dev/zero", fx.join("link")).unwrap();
        fs::write(fx.join("outside.txt"), b"secret").unwrap();
        std::os::unix::fs::symlink(fx.join("outside.txt"), fx.join("file_link")).unwrap();
        fs::create_dir(fx.join("dir")).unwrap();
        let path = fx.join(name);
        let (tx, rx) = mpsc::channel();

        // On a thread, so an open that blocks fails the test instead of
        // hanging it.
        thread::spawn(move || {
            let (task_tx, _task_rx) = mpsc::channel();
            let dst = path.with_file_name("dst");
            let prepared =
                prepare_destination(copy_task(task_tx), "copy", &path, &dst, false, 0o100_644);
            let _ = tx.send(prepared.is_none());
        });

        assert_eq!(Ok(true), rx.recv_timeout(Duration::from_secs(5)));
    }

    /// The copy's counterpart of `reopening_the_parent_of_a_moved_directory_is_refused`.
    #[test]
    fn a_copy_refuses_to_return_through_a_directory_that_was_moved() {
        let fx = TempDir::new("tasks_copy_reopen_moved");
        fs::create_dir_all(fx.join("parent").join("child")).unwrap();
        fs::create_dir(fx.join("elsewhere")).unwrap();
        let parent = open_directory_file(CWD, fx.join("parent")).unwrap();
        let expected = DirId::of(&parent).unwrap();
        let child = open_directory_file(&parent, "child").unwrap();
        assert_eq!(
            expected,
            DirId::of(reopen_parent_of(&child, expected).unwrap()).unwrap()
        );

        fs::rename(
            fx.join("parent").join("child"),
            fx.join("elsewhere").join("child"),
        )
        .unwrap();

        assert!(reopen_parent_of(&child, expected).is_err());
    }

    /// Pastes `old` into `dest` the way the worker does, from a selection made
    /// before `change` runs: a copy, or the cross-device arm of
    /// `run_move_task`, which removes what it copied. Returns the destination
    /// and the finished task.
    fn paste_after(
        old: &Path,
        dest: &Path,
        is_move: bool,
        change: impl FnOnce(),
    ) -> (PathBuf, Task) {
        let verb = if is_move { "move" } else { "copy" };
        let path = PathInfo::try_from(old).unwrap();
        let dest = PathInfo::try_from(dest).unwrap();
        let (old_path, new_path) = validate_paths(&path, &dest, verb, false).ok().unwrap();
        change();
        let (tx, rx) = mpsc::channel();
        let (active, source) = prepare_destination(
            copy_task(tx),
            verb,
            &old_path,
            &new_path,
            false,
            path.mode(),
        )
        .unwrap();
        if let Some((active, outcome)) = copy_with_progress(
            &old_path,
            &new_path,
            active,
            source,
            path.size,
            path.is_directory(),
            path.mode(),
            is_move,
            None,
            1024,
            1024,
        ) {
            if is_move {
                finish_cross_device_move(active, outcome, &old_path, path.is_directory());
            } else {
                finalize_copy(active, outcome.errors);
            }
        }
        (new_path, finished_task(&rx))
    }

    /// A FIFO selected for a move is a regular file by the time the worker
    /// runs. Recreating it as a FIFO and removing the file would lose its data.
    #[test]
    fn a_move_refuses_a_source_whose_type_changed_since_it_was_selected() {
        let fx = TempDir::new("tasks_move_type_changed");
        let old = fx.join("item");
        nix::unistd::mkfifo(&old, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
        fs::create_dir(fx.join("dest")).unwrap();

        let (new_path, task) = paste_after(&old, &fx.join("dest"), true, || {
            fs::remove_file(&old).unwrap();
            fs::write(&old, b"data").unwrap();
        });

        assert_eq!(b"data".as_slice(), fs::read(&old).unwrap());
        assert!(new_path.symlink_metadata().is_err());
        let message = task.error_message().expect("the move fails");
        assert!(
            message.ends_with("its type changed since it was selected"),
            "{message}"
        );
    }

    /// Another file is renamed over the selected name before the worker runs.
    /// It is copied with its own mode, never the selected file's: a copy of a
    /// 0600 file must not come out readable by others, and a move of one must
    /// not get the other file's setuid or execute bits.
    #[test_case(false ; "a copy")]
    #[test_case(true ; "a move")]
    fn a_file_renamed_over_the_selection_is_copied_with_its_own_mode(is_move: bool) {
        let fx = TempDir::new("tasks_renamed_over");
        let old = fx.join("notes");
        fs::write(&old, b"selected").unwrap();
        fs::set_permissions(&old, fs::Permissions::from_mode(0o755)).unwrap();
        let secret = fx.join("secret");
        fs::write(&secret, b"secret").unwrap();
        fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        fs::create_dir(fx.join("dest")).unwrap();

        let (new_path, task) = paste_after(&old, &fx.join("dest"), is_move, || {
            fs::rename(&secret, &old).unwrap();
        });

        assert_eq!(None, task.error_message());
        assert_eq!(b"secret".as_slice(), fs::read(&new_path).unwrap());
        assert_eq!(0o600, mode_of(&new_path) & 0o7777);
    }

    /// The destination directory is swapped for a link into the source after
    /// the paste was validated. The copy refuses to descend into the directory
    /// it created rather than copying its own output without end.
    #[test]
    fn a_copy_never_descends_into_a_directory_it_created() {
        let fx = TempDir::new("tasks_copy_into_itself");
        let src = fx.join("s");
        fs::create_dir_all(src.join("sub")).unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();

        let (_, task) = paste_after(&src, &dest, false, || {
            fs::remove_dir(&dest).unwrap();
            std::os::unix::fs::symlink(src.join("sub"), &dest).unwrap();
        });

        let message = task.error_message().expect("the copy reports the refusal");
        assert!(
            message.ends_with("it is inside the copy being made"),
            "{message}"
        );
        assert!(src.join("sub/s/sub").is_dir());
        assert!(!src.join("sub/s/sub/s").exists());
        assert!(src.join("sub/s/sub").read_dir().unwrap().next().is_none());
    }

    /// A directory the pre-scan listed is swapped for a link to one outside
    /// the tree before the scan reads it. The link is not followed.
    #[test]
    fn the_pre_scan_does_not_follow_a_directory_swapped_for_a_symlink() {
        let fx = TempDir::new("tasks_scan_swapped");
        let root = fx.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        let outside = fx.join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret.txt"), b"x").unwrap();
        let (tx, _rx) = mpsc::channel();
        let active = copy_task(tx);
        let mut seen = Vec::new();

        scan_tree(&active, &root, |_, name, _| {
            seen.push(name.to_owned());
            if name == c"sub" {
                fs::rename(root.join("sub"), fx.join("sub.orig")).unwrap();
                std::os::unix::fs::symlink(&outside, root.join("sub")).unwrap();
            }
        })
        .unwrap();

        assert_eq!(vec![c"sub".to_owned()], seen);
    }

    /// Another directory is renamed over one the copy listed, between the
    /// listing and the open. It is refused rather than copied under metadata
    /// that describes a different directory.
    #[test]
    fn a_directory_replaced_after_it_was_listed_is_refused() {
        let fx = TempDir::new("tasks_directory_replaced");
        let src = fx.join("src");
        fs::create_dir(&src).unwrap();
        let other = fx.join("other");
        fs::create_dir(&other).unwrap();
        let (src_parent, dst_parent) = (open_parent(&src).unwrap(), open_parent(&src).unwrap());
        let stat = statat(&src_parent, c"src", AtFlags::SYMLINK_NOFOLLOW).unwrap();
        fs::rename(&other, &src).unwrap();
        let (tx, _rx) = mpsc::channel();
        let active = copy_task(tx);
        let mut errors = Vec::new();
        let mut buffer = [0u8; 64];
        let mut context = context(false, &mut buffer);
        let at = At {
            src: &src_parent,
            dst: &dst_parent,
            src_name: c"src",
            dst_name: c"dst",
        };
        let paths = Paths {
            old: src.clone(),
            new: fx.join("dst"),
        };

        let level = enter_directory(&at, &paths, &mut errors, &mut context, &stat);
        active.done();

        assert!(level.is_none());
        assert_eq!(1, errors.len());
        assert!(
            errors[0].ends_with("it was replaced while being copied"),
            "{errors:?}"
        );
    }
}
