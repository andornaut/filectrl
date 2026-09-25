mod copy;
mod remove;
mod sys;
#[cfg(test)]
mod test_support;
mod validate;
mod walk;

use std::{
    fs,
    io::ErrorKind,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::AtomicBool,
        mpsc::{self, Sender},
    },
    thread,
    time::Duration,
};

use log::{info, warn};

pub(super) use self::validate::{is_same_file, onto_itself, rename_no_replace, restat};
use self::{
    copy::{CopyOutcome, CopySettings, copy_with_progress, prepare_destination},
    remove::{Removal, dir_total_entries, remove_path, replaced_after_copy},
    validate::{Renamed, display_path, rename_for_move, start_transfer},
};
use super::{
    conflicts::{
        Conflicts, changed_refusal, failed_transfer, not_replaced, raced_refusal,
        same_file_refusal, verb,
    },
    entry_id::Seen,
    path_info::{PathInfo, compact},
};
use crate::command::{
    Command,
    progress::{ActiveTask, CancellationToken, Task, TaskKind},
    result::CommandResult,
};

const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size
/// Shortest gap between two progress updates for one task. The percentage above
/// bounds them per unit of work, which for a fast copy is a hundred redraws
/// inside a second; this bounds them per unit of time.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

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
                job();
            }
        });
        tx
    });
    // The worker never exits, so the receiver outlives the process. Only a
    // test job that panics can end it (the test profile unwinds), and every
    // job after that would be dropped without a word.
    let sent = queue.send(Box::new(job));
    #[cfg(test)]
    sent.expect("the worker thread ended: an earlier job panicked");
    #[cfg(not(test))]
    let _ = sent;
}

/// Holds the shared worker until the returned sender is dropped, so work
/// queued meanwhile is validated before any of it runs.
#[cfg(test)]
pub(in crate::file_system) fn hold_worker() -> Sender<()> {
    let (release, gate) = mpsc::channel::<()>();
    queue_operation(move || {
        let _ = gate.recv();
    });
    release
}

/// Blocks until the task reporting on `rx` ends, and returns how it ended, so
/// the worker is done with what a test built for it before that is removed.
#[cfg(test)]
pub(in crate::file_system) fn await_end(rx: &mpsc::Receiver<Command>) -> Task {
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Command::Progress(task)) if task.is_terminal() => return task,
            Ok(_) => {}
            Err(error) => panic!("the task did not end: {error}"),
        }
    }
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

/// A file operation to run.
pub(in crate::file_system) enum TaskCommand {
    Copy(PasteJob),
    Delete(PathInfo),
    Move(PasteJob),
}

/// One source of a paste: `source` copied or moved into the directory
/// `dest`. `overwrite` is the queue's answer to a destination it saw taken:
/// the entry it may replace, as it saw it, or `None` when it saw the name
/// free. Only that entry is replaced: one that has taken its place since, or
/// the same one written since, is refused (`conflicts::changed_refusal`), and
/// a name taken after the queue saw it free, at the top level or anywhere
/// inside a directory the copy creates, is never replaced (`paste`'s standing
/// "skip all" skips it).
pub(in crate::file_system) struct PasteJob {
    pub(in crate::file_system) conflicts: Conflicts,
    pub(in crate::file_system) overwrite: Option<Seen>,
    pub(in crate::file_system) dest: PathInfo,
    pub(in crate::file_system) source: PathInfo,
}

impl TaskCommand {
    pub(in crate::file_system) fn run(self, tx: Sender<Command>) -> TaskRunResult {
        match self {
            TaskCommand::Copy(job) => run_paste_task(false, tx, job),
            TaskCommand::Delete(path) => run_delete_task(tx, &path),
            TaskCommand::Move(job) => run_paste_task(true, tx, job),
        }
    }
}

/// Starts one source of a paste, a move when `is_move` and a copy otherwise:
/// validated and registered here, then run on the worker.
fn run_paste_task(is_move: bool, tx: Sender<Command>, job: PasteJob) -> TaskRunResult {
    let PasteJob {
        conflicts,
        overwrite,
        dest,
        source,
    } = job;
    let (path, old_path, new_path, kind) =
        match start_transfer(is_move, &dest, overwrite.is_some(), &source) {
            Ok(started) => started,
            Err(result) => return TaskRunResult::failed(result),
        };

    // Fail a copy before the task is registered, so an unreadable directory
    // creates no progress notice. The recursive size walk still runs off the
    // UI thread. A symlink has `is_directory == false` even when it points at
    // a directory, so it skips this and is recreated as a link by
    // `copy_symlink`. A move is a rename first, which needs no listing.
    if !is_move
        && path.is_directory()
        && let Err(error) = fs::read_dir(&old_path)
    {
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
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        let Some(active) = check_cancelled(active) else {
            return;
        };
        let settings = CopySettings {
            is_move,
            conflicts: &conflicts,
        };
        if is_move {
            move_to(settings, overwrite, active, &old_path, &new_path, &path);
        } else {
            copy_to(settings, overwrite, active, &old_path, &new_path, &path);
        }
    });

    TaskRunResult::started(&initial, token, uncancellable)
}

/// The worker side of a move whose paths are validated: a rename, replacing
/// the entry `overwrite` names if it still holds the destination, or a copy
/// and removal when the rename crosses devices (`move_across_devices`). A name
/// taken since the paste saw it free is never replaced: skipped under a
/// standing "skip all", which leaves the source where it is, and refused
/// otherwise; so is one whose entry changed since.
fn move_to(
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    mut active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
) {
    match rename_for_move(overwrite, old_path, new_path) {
        Renamed::Moved => {
            active.increment(path.size);
            active.done();
        }
        Renamed::Changed => active.error(changed_refusal(true, old_path, new_path)),
        Renamed::SameFile => active.error(same_file_refusal(verb(true), old_path, new_path)),
        Renamed::Failed { error, kept } => match error.kind() {
            ErrorKind::AlreadyExists if settings.conflicts.skips_raced() => active.done(),
            ErrorKind::AlreadyExists => active.error(raced_refusal(true, old_path, new_path)),
            // A rename cannot cross devices, so the move falls back to a copy
            // and a removal of the source.
            ErrorKind::CrossesDevices => {
                move_across_devices(settings, overwrite, active, old_path, new_path, path);
            }
            // The rename replaces atomically or not at all, so a replacing one
            // that fails leaves what it was to replace.
            _ if kept => active
                .error(failed_transfer(true, old_path, new_path, &error) + &not_replaced(new_path)),
            _ => active.error(failed_transfer(true, old_path, new_path, &error)),
        },
    }
}

fn run_delete_task(tx: Sender<Command>, path: &PathInfo) -> TaskRunResult {
    let path = match restat(path, "delete") {
        Ok(fresh) => fresh,
        // Already gone, like `rm -f`: a delete earlier in the same batch may
        // have removed it, or the directory it was in, before this one starts.
        Err(_)
            if path
                .path
                .symlink_metadata()
                .is_err_and(|error| error.kind() == ErrorKind::NotFound) =>
        {
            return TaskRunResult::failed(CommandResult::Handled);
        }
        Err(error) => return TaskRunResult::failed(error.into()),
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
        if let Some((active, errors)) = remove_path(&path, is_directory, active, Removal::Delete) {
            finalize(active, errors);
        }
    });

    TaskRunResult::started(&initial, token, uncancellable)
}

/// The worker side of a copy whose paths are validated: copies `path`, over
/// the entry a granted overwrite names if it still holds the name, and reports
/// how it went.
fn copy_to(
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
) {
    if let Some((active, outcome)) =
        copy_stage(settings, overwrite, active, old_path, new_path, path)
    {
        // A skipped entry needs no mention here: nothing is left behind by a
        // copy that did not make it, and the standing "skip all" that settled
        // it was the user's own answer.
        finalize(active, outcome.errors);
    }
}

/// A move whose rename crossed devices: copies `path`, then removes the
/// source (`finish_cross_device_move`).
fn move_across_devices(
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
) {
    if let Some((active, outcome)) =
        copy_stage(settings, overwrite, active, old_path, new_path, path)
    {
        finish_cross_device_move(active, outcome, old_path, path.is_directory());
    }
}

/// What a copy and the copy a move across devices (`settings.is_move`) share:
/// checks the destination and opens the source (`prepare_destination`), then
/// copies `path`. `None` when the task was finalized on the way.
fn copy_stage(
    settings: CopySettings<'_>,
    overwrite: Option<Seen>,
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
) -> Option<(ActiveTask, CopyOutcome)> {
    let (active, prepared) =
        prepare_destination(active, settings, overwrite, old_path, new_path, path.mode())?;
    copy_with_progress(settings, prepared, path, active, old_path, new_path)
}

/// Finishes a cross-device move once the copy stage is done, removing the
/// source only when the destination holds every entry of it, like `mv`.
///
/// A failed entry keeps the whole source, including the entries that copied,
/// and the partial destination, which the message says when the destination
/// received anything. A skipped one does the same: it is no more at the
/// destination than a failed one, so removing the source would delete what
/// nothing else holds. A skipped top-level entry copied nothing, which leaves
/// the source where it is with nothing to report, as a rename that skips does.
fn finish_cross_device_move(
    active: ActiveTask,
    outcome: CopyOutcome,
    old_path: &Path,
    is_directory: bool,
) {
    if let Some(summary) = summarize(outcome.errors) {
        if outcome.wrote {
            active.error(format!(
                "{summary}; the original {} was kept",
                compact(old_path)
            ));
        } else {
            active.error(summary);
        }
        return;
    }
    if outcome.top_skipped {
        active.done();
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
        active.error(replaced_after_copy(old_path));
        return;
    };
    // Like `mv`, an entry written into the source while it was being copied is
    // removed with the rest, and one that cannot be removed is reported while
    // the rest still are.
    let removal = Removal::MovedSource(root);
    if let Some((active, errors)) = remove_path(old_path, is_directory, active, removal) {
        finalize(active, errors);
    }
}

/// Finalizes a copy, move or delete the way coreutils does: success when no
/// per-entry error was recorded, otherwise one alert summarizing them. Skipped
/// entries are not failures and do not appear.
fn finalize(active: ActiveTask, errors: Vec<String>) {
    match summarize(errors) {
        Some(summary) => active.error(summary),
        None => active.done(),
    }
}

/// One line for the errors a task recorded, the first with a count of the
/// rest, or `None` when there are none. Every error is logged.
fn summarize(errors: Vec<String>) -> Option<String> {
    for error in &errors {
        warn!("{error}");
    }
    let more = errors.len().checked_sub(1)?;
    let first = errors.into_iter().next()?;
    Some(if more == 0 {
        first
    } else {
        format!("{first} (and {more} more)")
    })
}

/// Finalizes a task cancelled part way through. The task shows only that it
/// was cancelled, so each error recorded before the cancel is logged, as
/// `finalize` logs them.
fn cancel_logging(errors: &[String], active: ActiveTask) {
    for error in errors {
        warn!("{error}");
    }
    active.cancelled();
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

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File},
        os::unix::fs::PermissionsExt,
        path::PathBuf,
    };

    use test_case::test_case;

    use super::{
        test_support::{
            KINDS, Kind, STANDING, answered, assert_made, copy_task, every_case, finished_task,
            id_of, make, other_device, paste_after, paste_over, run_to_end, seen, staging_left,
        },
        *,
    };
    use crate::{
        command::{
            ConflictChoice,
            progress::{Progress, Transfer},
        },
        file_system::entry_id::{EntryId, records_birth_time},
        test_support::TempDir,
    };

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
        let task = run_to_end(TaskCommand::Copy(PasteJob {
            conflicts: Conflicts::default(),
            overwrite: None,
            dest: PathInfo::try_from(dst.as_path()).unwrap(),
            source: PathInfo::try_from(src.as_path()).unwrap(),
        }))
        .expect("the copy should start");

        assert_eq!(None, task.error_message());
        assert_ne!(
            old,
            fs::metadata(dst.join("a.txt")).unwrap().modified().unwrap()
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

        let result = run_to_end(TaskCommand::Copy(PasteJob {
            conflicts: Conflicts::default(),
            overwrite: None,
            dest: PathInfo::try_from(dst.as_path()).unwrap(),
            source: PathInfo::try_from(src.as_path()).unwrap(),
        }));
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
            assert_eq!(
                &format!(
                    "Failed to read directory {}: Permission denied (os error 13)",
                    compact(&src)
                ),
                message
            );
            assert!(!dst.join("locked").exists());
        } else {
            eprintln!("skipped: a mode-000 directory can be listed here");
            assert_eq!(
                None,
                result.expect("a readable copy starts").error_message()
            );
        }
    }

    /// A move is a rename first, which needs no listing, so a directory that
    /// cannot be listed still moves within its filesystem. Write access stays:
    /// moving a directory to another parent rewrites its `..`.
    #[test]
    fn a_move_of_an_unreadable_directory_is_not_refused_up_front() {
        let fx = TempDir::new("tasks_move_unreadable");
        let src = fx.join("locked");
        fs::create_dir(&src).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o300)).unwrap();
        let dst = fx.join("dst");
        fs::create_dir(&dst).unwrap();

        let result = run_to_end(TaskCommand::Move(PasteJob {
            conflicts: Conflicts::default(),
            overwrite: None,
            dest: PathInfo::try_from(dst.as_path()).unwrap(),
            source: PathInfo::try_from(src.as_path()).unwrap(),
        }));
        let moved = dst.join("locked");
        let _ = fs::set_permissions(&moved, fs::Permissions::from_mode(0o755));
        let _ = fs::set_permissions(&src, fs::Permissions::from_mode(0o755));

        assert_eq!(None, result.expect("the move should start").error_message());
        assert!(moved.is_dir());
        assert!(!src.exists());
    }

    // ── a paste replaces only what the queue saw ─────────────────────────────

    /// How a job of a paste reaches the worker.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Op {
        Copy,
        /// A move whose rename stays on one device.
        Move,
        /// A move whose rename crossed devices, which `move_to` hands to
        /// `move_across_devices`. Queued straight to that function here, so it
        /// runs on any machine; the tests named `..._across_devices_...` reach
        /// it through `move_to` where a second filesystem exists.
        Across,
    }

    const OPS: [Op; 3] = [Op::Copy, Op::Move, Op::Across];

    /// Validates and queues `source` into `dest` as a job of the paste behind
    /// `conflicts`, with the replacement of `overwrite` granted, the way the
    /// paste queue does. `Err` carries the alerts of a job refused before it
    /// was queued.
    fn start(
        conflicts: &Conflicts,
        op: Op,
        overwrite: Option<Seen>,
        dest: &Path,
        source: &Path,
    ) -> Result<mpsc::Receiver<Command>, Vec<Command>> {
        start_cancellable(conflicts, op, overwrite, dest, source).map(|(rx, _)| rx)
    }

    /// `start`, with the token that cancels the job.
    fn start_cancellable(
        conflicts: &Conflicts,
        op: Op,
        overwrite: Option<Seen>,
        dest: &Path,
        source: &Path,
    ) -> Result<(mpsc::Receiver<Command>, CancellationToken), Vec<Command>> {
        let (tx, rx) = mpsc::channel();
        let path = PathInfo::try_from(source).unwrap();
        let dir = PathInfo::try_from(dest).unwrap();
        let job = PasteJob {
            conflicts: conflicts.clone(),
            overwrite,
            dest: dir.clone(),
            source: path.clone(),
        };
        let result = match op {
            Op::Copy => TaskCommand::Copy(job).run(tx),
            Op::Move => TaskCommand::Move(job).run(tx),
            Op::Across => {
                let (path, old_path, new_path, kind) =
                    match start_transfer(true, &dir, overwrite.is_some(), &path) {
                        Ok(started) => started,
                        Err(result) => return Err(result.into_commands()),
                    };
                let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
                let uncancellable = active.uncancellable_handle();
                let conflicts = conflicts.clone();
                queue_operation(move || {
                    let settings = CopySettings {
                        is_move: true,
                        conflicts: &conflicts,
                    };
                    move_across_devices(settings, overwrite, active, &old_path, &new_path, &path);
                });
                TaskRunResult::started(&initial, token, uncancellable)
            }
        };
        match result.cancel_info {
            Some(info) => Ok((rx, info.token)),
            None => Err(result.command_result.into_commands()),
        }
    }

    /// The refusal of `source`, which found `new` taken after the queue saw
    /// it free.
    fn raced(is_move: bool, source: &Path, new: &Path) -> String {
        let verb = if is_move { "move" } else { "copy" };
        format!(
            "Cannot {verb} {} to {}: another entry took that name after the paste checked it",
            compact(source),
            compact(new)
        )
    }

    /// The refusal of `source`, whose granted replacement found another entry
    /// at `new`.
    fn changed(is_move: bool, source: &Path, new: &Path) -> String {
        let verb = if is_move { "move" } else { "copy" };
        format!(
            "Cannot {verb} {} to {}: the entry there changed after the paste checked it",
            compact(source),
            compact(new)
        )
    }

    /// Every way a source of a paste can find its destination name taken
    /// after the queue saw it free (validated while free, taken before its job
    /// runs), for every operation, every kind of source and of occupant, and
    /// every standing answer: the occupant is never replaced, the source stays
    /// where it was, and the outcome is exact: skipped under "skip all" with
    /// nothing to report, refused otherwise, "overwrite all" included.
    #[test]
    fn a_name_taken_after_the_paste_saw_it_free_is_never_replaced() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| KINDS.into_iter().map(move |source| (op, source)))
            .flat_map(|(op, source)| {
                KINDS
                    .into_iter()
                    .map(move |occupant| (op, source, occupant))
            })
            .flat_map(|(op, source, occupant)| {
                STANDING
                    .into_iter()
                    .map(move |standing| (op, source, occupant, standing))
            })
            .collect();
        assert_eq!(144, cases.len());

        every_case(cases, |&(op, source_kind, occupant, standing)| {
            let case = format!("{op:?}, {source_kind:?} onto {occupant:?}, {standing:?}");
            let fx = TempDir::new("tasks_raced");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            make(source_kind, "source", &old);
            let conflicts = answered(standing);

            let gate = hold_worker();
            let job = start(&conflicts, op, None, &dest, &old).expect("validated while free");
            make(occupant, "occupant", &new);
            let id = EntryId::of_path(&new);
            drop(gate);
            let task = await_end(&job);

            // Nothing reached the destination, so a move across devices has
            // nothing to say about the source it kept.
            let expected = match standing {
                Some(ConflictChoice::SkipAll) => None,
                _ => Some(raced(op != Op::Copy, &old, &new)),
            };
            assert_eq!(expected, task.error_message(), "{case}");
            assert_made(&case, occupant, "occupant", id, &new);
            assert_made(&case, source_kind, "source", None, &old);
        });
    }

    /// A move that really crosses devices, through `move_to`, whose
    /// `CrossesDevices` arm has to hand the paste on for "skip all" to reach
    /// the copy. Needs a writable directory on another filesystem than the
    /// temporary one; skipped where there is none.
    #[test]
    fn a_name_taken_across_devices_is_never_replaced() {
        every_case(STANDING, |&standing| {
            let case = format!("{standing:?}");
            let fx = TempDir::new("tasks_raced_across");
            let Some((_removed, dest)) = other_device(&fx) else {
                eprintln!("skipped: no writable directory on another filesystem");
                return;
            };
            let (old, new) = (fx.join("one"), dest.join("one"));
            make(Kind::File, "source", &old);
            let conflicts = answered(standing);

            let gate = hold_worker();
            let job = start(&conflicts, Op::Move, None, &dest, &old).expect("validated while free");
            make(Kind::File, "occupant", &new);
            let id = EntryId::of_path(&new);
            drop(gate);

            let expected = match standing {
                Some(ConflictChoice::SkipAll) => None,
                _ => Some(raced(true, &old, &new)),
            };
            assert_eq!(expected, await_end(&job).error_message(), "{case}");
            assert_made(&case, Kind::File, "occupant", id, &new);
            assert_made(&case, Kind::File, "source", None, &old);
        });
    }

    /// Two pastes queued at once: the later one validated its source while the
    /// name was free, so the earlier one's entry is a name taken since, and
    /// its standing "overwrite all" does not reach it. Replacing it would lose
    /// the only copy of what the earlier paste cut.
    #[test]
    fn a_later_paste_never_replaces_what_an_earlier_one_left() {
        let fx = TempDir::new("tasks_two_pastes");
        let (x, y, dest) = (fx.join("x"), fx.join("y"), fx.join("dest"));
        for dir in [&x, &y, &dest] {
            fs::create_dir(dir).unwrap();
        }
        fs::write(x.join("a"), b"x").unwrap();
        fs::write(y.join("a"), b"y").unwrap();
        let (first, second) = (
            Conflicts::default(),
            answered(Some(ConflictChoice::OverwriteAll)),
        );

        let gate = hold_worker();
        let first_job = start(&first, Op::Move, None, &dest, &x.join("a")).unwrap();
        let second_job = start(&second, Op::Move, None, &dest, &y.join("a")).unwrap();
        drop(gate);

        assert_eq!(None, await_end(&first_job).error_message());
        assert_eq!(
            Some(raced(true, &y.join("a"), &dest.join("a"))),
            await_end(&second_job).error_message()
        );
        assert_eq!(b"x".to_vec(), fs::read(dest.join("a")).unwrap());
        assert_eq!(b"y".to_vec(), fs::read(y.join("a")).unwrap());
        assert!(!x.join("a").exists());
    }

    /// What holds a name the queue was told to replace, by the time the job
    /// runs.
    #[derive(Clone, Copy, Debug)]
    enum Since {
        /// The entry the queue saw, untouched.
        Unchanged,
        /// Nothing: the entry was removed.
        Removed,
        /// Another entry of this kind, in the one the queue saw's place.
        Replaced(Kind),
        /// The entry the queue saw, written with as many bytes as before.
        Written,
    }

    const SINCE: [Since; 7] = [
        Since::Unchanged,
        Since::Removed,
        Since::Written,
        Since::Replaced(Kind::File),
        Since::Replaced(Kind::Symlink),
        Since::Replaced(Kind::Fifo),
        Since::Replaced(Kind::Directory),
    ];

    /// Puts what `since` describes at `new`, which holds a file, keeping the
    /// file under another name in `fx` so a new entry cannot reuse its inode
    /// number. Returns the identity of what is there.
    fn change(fx: &TempDir, since: Since, new: &Path) -> Option<EntryId> {
        match since {
            Since::Unchanged => {}
            Since::Removed => fs::remove_file(new).unwrap(),
            Since::Replaced(kind) => {
                fs::rename(new, fx.join("kept")).unwrap();
                make(kind, "since", new);
            }
            Since::Written => {
                // Past any timestamp granularity the filesystem might round to.
                std::thread::sleep(Duration::from_millis(20));
                fs::write(new, b"SEEN").unwrap();
            }
        }
        EntryId::of_path(new)
    }

    /// A granted overwrite replaces the entry the queue saw and nothing else,
    /// by every operation: an entry that took its place since is refused and
    /// left alone, and a name found free is written as if it had been free.
    #[test]
    fn a_granted_overwrite_replaces_only_the_entry_the_queue_saw() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| SINCE.into_iter().map(move |since| (op, since)))
            .collect();

        every_case(cases, |&(op, since)| {
            let case = format!("{op:?}, {since:?}");
            let fx = TempDir::new("tasks_granted");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();
            let granted = seen(&new);

            let gate = hold_worker();
            let job = start(&Conflicts::default(), op, granted, &dest, &old).unwrap();
            let id = change(&fx, since, &new);
            drop(gate);
            let task = await_end(&job);

            match since {
                Since::Unchanged | Since::Removed => {
                    assert_eq!(None, task.error_message(), "{case}");
                    assert_eq!(b"source".to_vec(), fs::read(&new).unwrap(), "{case}");
                    assert_eq!(op == Op::Copy, old.exists(), "{case}");
                }
                Since::Replaced(kind) => {
                    assert_eq!(
                        Some(changed(op != Op::Copy, &old, &new)),
                        task.error_message(),
                        "{case}"
                    );
                    assert_made(&case, kind, "since", id, &new);
                    assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
                }
                Since::Written => {
                    assert_eq!(
                        Some(changed(op != Op::Copy, &old, &new)),
                        task.error_message(),
                        "{case}"
                    );
                    assert_eq!(b"SEEN".to_vec(), fs::read(&new).unwrap(), "{case}");
                    assert_eq!(id, EntryId::of_path(&new), "{case}");
                    assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
                }
            }
            assert_eq!(Vec::<String>::new(), staging_left(&dest), "{case}");
        });
    }

    /// A granted overwrite whose source is gone by the time it runs fails,
    /// and says the entry it was to replace was left exactly when that entry
    /// is still there: not when it was removed too, where nothing granted is
    /// left to mention. A file source fails where it is opened, before the
    /// copy; a symlink where the copy reads it.
    #[test]
    fn a_granted_overwrite_whose_source_is_gone_says_whether_it_left_the_entry() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| [Kind::File, Kind::Symlink].map(move |kind| (op, kind)))
            .flat_map(|(op, kind)| [false, true].map(move |gone| (op, kind, gone)))
            .collect();

        every_case(cases, |&(op, kind, occupant_gone)| {
            let case = format!("{op:?}, {kind:?}, occupant gone: {occupant_gone}");
            let fx = TempDir::new("tasks_granted_source_gone");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            make(kind, "source", &old);
            fs::write(&new, b"seen").unwrap();
            let granted = seen(&new);

            let gate = hold_worker();
            let job = start(&Conflicts::default(), op, granted, &dest, &old).unwrap();
            fs::remove_file(&old).unwrap();
            if occupant_gone {
                fs::remove_file(&new).unwrap();
            }
            drop(gate);
            let task = await_end(&job);

            let verb = if op == Op::Copy { "copy" } else { "move" };
            let failed = format!(
                "Failed to {verb} {} to {}: No such file or directory (os error 2)",
                compact(&old),
                compact(&new)
            );
            let expected = if occupant_gone {
                failed
            } else {
                format!("{failed}; {} was not replaced", compact(&new))
            };
            assert_eq!(Some(expected), task.error_message(), "{case}");
            if !occupant_gone {
                assert_eq!(b"seen".to_vec(), fs::read(&new).unwrap(), "{case}");
            }
            assert_eq!(Vec::<String>::new(), staging_left(&dest), "{case}");
        });
    }

    /// Every kind of entry that can replace another does, staged and landed
    /// whole: a file, a symlink and a FIFO, by every operation.
    #[test]
    fn a_granted_overwrite_replaces_with_every_kind_of_source() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| [Kind::File, Kind::Symlink, Kind::Fifo].map(move |kind| (op, kind)))
            .collect();

        every_case(cases, |&(op, kind)| {
            let case = format!("{op:?}, {kind:?}");
            let fx = TempDir::new("tasks_granted_kinds");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            make(kind, "source", &old);
            fs::write(&new, b"seen").unwrap();
            let granted = seen(&new);

            let job = start(&Conflicts::default(), op, granted, &dest, &old).unwrap();
            let task = await_end(&job);

            assert_eq!(None, task.error_message(), "{case}");
            assert_made(&case, kind, "source", None, &new);
            assert_eq!(op == Op::Copy, old.symlink_metadata().is_ok(), "{case}");
            assert_eq!(Vec::<String>::new(), staging_left(&dest), "{case}");
        });
    }

    /// A granted overwrite of a symlink replaces the link, never the file it
    /// points at, by every operation.
    #[test]
    fn a_granted_overwrite_of_a_symlink_replaces_the_link() {
        every_case(OPS, |&op| {
            let case = format!("{op:?}");
            let fx = TempDir::new("tasks_granted_link");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(dest.join("target"), b"target").unwrap();
            std::os::unix::fs::symlink("target", &new).unwrap();
            let granted = seen(&new);

            let job = start(&Conflicts::default(), op, granted, &dest, &old).unwrap();
            let task = await_end(&job);

            assert_eq!(None, task.error_message(), "{case}");
            assert!(!fs::symlink_metadata(&new).unwrap().is_symlink(), "{case}");
            assert_eq!(b"source".to_vec(), fs::read(&new).unwrap(), "{case}");
            assert_eq!(
                b"target".to_vec(),
                fs::read(dest.join("target")).unwrap(),
                "{case}"
            );
        });
    }

    /// Two names of one file, both granted: replacing the first changes the
    /// file's change time but not when it was created, so the second is
    /// still the entry the queue saw and is replaced too. Where the
    /// filesystem records no birth time the second is refused instead, which
    /// loses nothing; the test says so.
    #[test]
    fn two_granted_links_of_one_file_are_both_replaced() {
        every_case(OPS, |&op| {
            let case = format!("{op:?}");
            let fx = TempDir::new("tasks_granted_links");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            fs::write(src.join("a"), b"new a").unwrap();
            fs::write(src.join("b"), b"new b").unwrap();
            fs::write(dest.join("a"), b"shared").unwrap();
            fs::hard_link(dest.join("a"), dest.join("b")).unwrap();
            let (granted_a, granted_b) = (seen(&dest.join("a")), seen(&dest.join("b")));
            let conflicts = Conflicts::default();

            let gate = hold_worker();
            let first = start(&conflicts, op, granted_a, &dest, &src.join("a")).unwrap();
            let second = start(&conflicts, op, granted_b, &dest, &src.join("b")).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drop(gate);
            assert_eq!(None, await_end(&first).error_message(), "{case}");
            let second = await_end(&second);

            assert_eq!(
                b"new a".to_vec(),
                fs::read(dest.join("a")).unwrap(),
                "{case}"
            );
            if records_birth_time(&dest) {
                assert_eq!(None, second.error_message(), "{case}");
                assert_eq!(
                    b"new b".to_vec(),
                    fs::read(dest.join("b")).unwrap(),
                    "{case}"
                );
            } else {
                // The change time stands in, which replacing the other link
                // moved, so the second is refused and its entry kept.
                assert_eq!(
                    Some(changed(op != Op::Copy, &src.join("b"), &dest.join("b"))),
                    second.error_message(),
                    "{case}"
                );
                assert_eq!(
                    b"new a".to_vec(),
                    fs::read(dest.join("b")).unwrap(),
                    "{case}"
                );
            }
        });
    }

    /// The same through `move_to` onto another filesystem, where the rename's
    /// own check runs first and the copy then replaces the entry once it is
    /// whole.
    /// Skipped where there is no second filesystem.
    #[test]
    fn a_granted_overwrite_across_devices_replaces_only_the_entry_the_queue_saw() {
        every_case(SINCE, |&since| {
            let case = format!("{since:?}");
            let fx = TempDir::new("tasks_granted_across");
            let Some((_removed, dest)) = other_device(&fx) else {
                eprintln!("skipped: no writable directory on another filesystem");
                return;
            };
            let (old, new) = (fx.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();
            let granted = seen(&new);

            let gate = hold_worker();
            let job = start(&Conflicts::default(), Op::Move, granted, &dest, &old).unwrap();
            // Kept on the destination's filesystem, as a rename there would
            // leave it.
            let id = match since {
                Since::Replaced(kind) => {
                    fs::rename(&new, dest.join("kept")).unwrap();
                    make(kind, "since", &new);
                    EntryId::of_path(&new)
                }
                _ => change(&fx, since, &new),
            };
            drop(gate);
            let task = await_end(&job);

            match since {
                Since::Unchanged | Since::Removed => {
                    assert_eq!(None, task.error_message(), "{case}");
                    assert_eq!(b"source".to_vec(), fs::read(&new).unwrap(), "{case}");
                    assert!(!old.exists(), "{case}");
                }
                Since::Replaced(kind) => {
                    assert_eq!(
                        Some(changed(true, &old, &new)),
                        task.error_message(),
                        "{case}"
                    );
                    assert_made(&case, kind, "since", id, &new);
                    assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
                }
                Since::Written => {
                    assert_eq!(
                        Some(changed(true, &old, &new)),
                        task.error_message(),
                        "{case}"
                    );
                    assert_eq!(b"SEEN".to_vec(), fs::read(&new).unwrap(), "{case}");
                    assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
                }
            }
            assert_eq!(Vec::<String>::new(), staging_left(&dest), "{case}");
        });
    }

    /// A granted overwrite cancelled while its job waits in the queue does
    /// nothing: the entry it was granted for and the source stay as they
    /// were, through the jobs' own closures.
    #[test]
    fn a_granted_overwrite_cancelled_while_queued_does_nothing() {
        every_case([Op::Copy, Op::Move], |&op| {
            let case = format!("{op:?}");
            let fx = TempDir::new("tasks_granted_cancelled");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();
            let granted = seen(&new);

            let gate = hold_worker();
            let (job, token) =
                start_cancellable(&Conflicts::default(), op, granted, &dest, &old).unwrap();
            token.cancel();
            drop(gate);
            let task = await_end(&job);

            assert!(task.is_cancelled(), "{case}: {:?}", task.error_message());
            assert_eq!(b"seen".to_vec(), fs::read(&new).unwrap(), "{case}");
            assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
            assert_eq!(Vec::<String>::new(), staging_left(&dest), "{case}");
        });
    }

    /// Two pastes both told to replace the same entry: the first does, and
    /// the second finds the first's entry in its place, which it was never
    /// told to replace. For a cut, that entry is the only copy of the source.
    #[test]
    fn a_later_paste_granted_the_same_overwrite_never_replaces_the_earlier_one() {
        every_case(OPS, |&op| {
            let case = format!("{op:?}");
            let fx = TempDir::new("tasks_two_granted");
            let (x, y, dest) = (fx.join("x"), fx.join("y"), fx.join("dest"));
            for dir in [&x, &y, &dest] {
                fs::create_dir(dir).unwrap();
            }
            fs::write(x.join("a"), b"x").unwrap();
            fs::write(y.join("a"), b"y").unwrap();
            fs::write(dest.join("a"), b"there").unwrap();
            let granted = seen(&dest.join("a"));

            let gate = hold_worker();
            let first = start(&Conflicts::default(), op, granted, &dest, &x.join("a")).unwrap();
            let second = start(&Conflicts::default(), op, granted, &dest, &y.join("a")).unwrap();
            drop(gate);

            assert_eq!(None, await_end(&first).error_message(), "{case}");
            assert_eq!(
                Some(changed(op != Op::Copy, &y.join("a"), &dest.join("a"))),
                await_end(&second).error_message(),
                "{case}"
            );
            assert_eq!(b"x".to_vec(), fs::read(dest.join("a")).unwrap(), "{case}");
            assert_eq!(b"y".to_vec(), fs::read(y.join("a")).unwrap(), "{case}");
        });
    }

    /// A granted overwrite that cannot write its replacement beside the entry
    /// it was granted for reports it and leaves both in place. Probed rather
    /// than skipped for root, who can write to a read-only directory.
    #[test]
    fn a_granted_overwrite_that_cannot_write_beside_the_entry_reports_it() {
        every_case([Op::Copy, Op::Across], |&op| {
            let case = format!("{op:?}");
            let fx = TempDir::new("tasks_granted_locked");
            let (src, dest) = (fx.join("src"), fx.join("dest"));
            fs::create_dir(&src).unwrap();
            fs::create_dir(&dest).unwrap();
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();
            let granted = seen(&new);

            let gate = hold_worker();
            let job = start(&Conflicts::default(), op, granted, &dest, &old).unwrap();
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
            let locked = fs::write(dest.join("probe"), b"").is_err();
            drop(gate);
            let task = await_end(&job);
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

            if locked {
                let verb = if op == Op::Copy { "copy" } else { "move" };
                assert_eq!(
                    Some(format!(
                        "Failed to {verb} {} to {}: Permission denied (os error 13); {} was not \
                         replaced",
                        compact(&old),
                        compact(&new),
                        compact(&new)
                    )),
                    task.error_message(),
                    "{case}"
                );
                assert_eq!(b"seen".to_vec(), fs::read(&new).unwrap(), "{case}");
                assert!(old.exists(), "{case}");
            } else {
                eprintln!("skipped: a directory without write permission can be written here");
                assert_eq!(None, task.error_message(), "{case}");
            }
        });
    }

    /// A granted overwrite whose destination became another link to the
    /// source before its job ran: `rename(2)` onto it would do nothing and
    /// report success, so the move is refused and both names stay.
    #[test]
    fn a_granted_move_onto_a_link_to_its_source_is_refused_as_the_same_file() {
        let fx = TempDir::new("tasks_granted_same_file");
        let (src, dest) = (fx.join("src"), fx.join("dest"));
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dest).unwrap();
        let (old, new) = (src.join("one"), dest.join("one"));
        fs::write(&old, b"source").unwrap();
        fs::write(&new, b"seen").unwrap();

        let gate = hold_worker();
        let job = start(&Conflicts::default(), Op::Move, seen(&new), &dest, &old).unwrap();
        fs::remove_file(&new).unwrap();
        fs::hard_link(&old, &new).unwrap();
        drop(gate);
        let task = await_end(&job);

        assert_eq!(
            Some(format!(
                "Cannot move {} to {}: they are the same file",
                compact(&old),
                compact(&new)
            )),
            task.error_message()
        );
        assert!(old.exists());
        assert!(new.exists());
    }

    /// A granted move whose rename fails leaves what it was to replace, which
    /// the message says: the rename replaces atomically or not at all.
    #[test]
    fn a_granted_move_that_fails_says_what_it_left() {
        let fx = TempDir::new("tasks_granted_move_fails");
        let (src, dest) = (fx.join("src"), fx.join("dest"));
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dest).unwrap();
        let (old, new) = (src.join("one"), dest.join("one"));
        fs::write(&old, b"source").unwrap();
        fs::write(&new, b"seen").unwrap();

        let gate = hold_worker();
        let job = start(&Conflicts::default(), Op::Move, seen(&new), &dest, &old).unwrap();
        fs::remove_file(&old).unwrap();
        drop(gate);
        let task = await_end(&job);

        assert_eq!(
            Some(format!(
                "Failed to move {} to {}: No such file or directory (os error 2); {} was not \
                 replaced",
                compact(&old),
                compact(&new),
                compact(&new)
            )),
            task.error_message()
        );
        assert_eq!(b"seen".to_vec(), fs::read(&new).unwrap());
    }

    /// Jobs run one at a time, in the order they were queued.
    #[test]
    fn a_job_starts_after_the_one_queued_before_it_has_finished() {
        let fx = TempDir::new("tasks_serial");
        let marker = fx.join("marker");
        let (seen_tx, seen_rx) = mpsc::channel();
        let gate = hold_worker();
        let written = marker.clone();
        queue_operation(move || {
            std::thread::sleep(Duration::from_millis(50));
            let _ = fs::write(&written, b"done");
        });
        queue_operation(move || {
            let _ = seen_tx.send(marker.exists());
        });
        drop(gate);

        assert_eq!(Ok(true), seen_rx.recv_timeout(Duration::from_secs(5)));
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

    /// The whole fallback `move_to` takes when the rename crosses devices,
    /// from the copy through the removal.
    #[test_case(false ; "into a free name")]
    #[test_case(true ; "over a file the paste was allowed to replace")]
    fn a_move_across_devices_leaves_only_the_destination(occupied: bool) {
        let fx = TempDir::new("tasks_move_across");
        let old = fx.join("a.txt");
        fs::write(&old, b"src").unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        if occupied {
            fs::write(dest.join("a.txt"), b"dest").unwrap();
        }

        let (new_path, task) = if occupied {
            paste_over(true, &dest, &old)
        } else {
            paste_after(true, &dest, &old, || {})
        };

        assert_eq!(None, task.error_message());
        assert_eq!(b"src".as_slice(), fs::read(&new_path).unwrap());
        assert!(old.symlink_metadata().is_err());
    }

    #[test]
    fn a_copy_over_a_file_the_paste_was_allowed_to_replace_keeps_its_source() {
        let fx = TempDir::new("tasks_copy_over");
        let old = fx.join("a.txt");
        fs::write(&old, b"src").unwrap();
        let dest = fx.join("dest");
        fs::create_dir(&dest).unwrap();
        fs::write(dest.join("a.txt"), b"dest").unwrap();

        let (new_path, task) = paste_over(false, &dest, &old);

        assert_eq!(None, task.error_message());
        assert_eq!(b"src".as_slice(), fs::read(&new_path).unwrap());
        assert_eq!(b"src".as_slice(), fs::read(&old).unwrap());
    }

    /// The skipped entries never reached the destination, so removing the
    /// source would delete the only copy of them. A skip is not an error, so
    /// the emptiness of `errors` must not be what decides this.
    #[test_case(1, "Skipped 1 entry" ; "one")]
    #[test_case(2, "Skipped 2 entries" ; "several")]
    fn a_cross_device_move_keeps_its_source_when_an_entry_below_it_was_skipped(
        skipped: usize,
        reported: &str,
    ) {
        let (_fx, src, rx, active) = moved("tasks_move_skipped");

        finish_cross_device_move(
            active,
            CopyOutcome {
                skipped,
                ..CopyOutcome::default()
            },
            &src,
            true,
        );

        assert!(src.join("a.txt").exists());
        assert_eq!(
            Some(format!(
                "{reported}, so the original {} was kept",
                compact(&src)
            )),
            finished_task(&rx).error_message()
        );
    }

    /// A skipped top-level entry copied nothing, so the move ends as a rename
    /// that skips does: done, with the source where it was.
    #[test]
    fn a_cross_device_move_whose_entry_was_skipped_keeps_its_source_quietly() {
        let (_fx, src, rx, active) = moved("tasks_move_top_skipped");

        finish_cross_device_move(
            active,
            CopyOutcome {
                skipped: 1,
                top_skipped: true,
                ..CopyOutcome::default()
            },
            &src,
            true,
        );

        assert!(src.join("a.txt").exists());
        assert_eq!(None, finished_task(&rx).error_message());
    }

    /// A move across devices that failed keeps its source, and says so when
    /// the destination received part of it; when nothing reached it, the
    /// failure is the whole story.
    #[test_case(true ; "with part of it at the destination")]
    #[test_case(false ; "with nothing at the destination")]
    fn a_cross_device_move_keeps_its_source_when_an_entry_failed(wrote: bool) {
        let (_fx, src, rx, active) = moved("tasks_move_failed");

        finish_cross_device_move(
            active,
            CopyOutcome {
                errors: vec!["a.txt could not be written".to_string()],
                wrote,
                ..CopyOutcome::default()
            },
            &src,
            true,
        );

        assert!(src.join("a.txt").exists());
        let expected = if wrote {
            format!(
                "a.txt could not be written; the original {} was kept",
                compact(&src)
            )
        } else {
            "a.txt could not be written".to_string()
        };
        assert_eq!(Some(expected), finished_task(&rx).error_message());
    }

    #[test]
    fn several_failed_entries_are_reported_as_the_first_and_a_count_of_the_rest() {
        let (tx, rx) = mpsc::channel();

        finalize(
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

    /// The queue can hold a task for as long as the operations ahead of it
    /// take, so a cancel has to be honored before anything is written.
    #[test]
    fn a_cancel_that_lands_while_queued_stops_the_task() {
        let (tx, rx) = mpsc::channel();
        let (active, _, token) = ActiveTask::new(
            tx,
            TaskKind::Copy(Transfer {
                source: String::new(),
                destination: String::new(),
            }),
            1,
        );
        token.cancel();

        assert!(check_cancelled(active).is_none());
        assert!(finished_task(&rx).is_cancelled());
    }

    /// Starts two deletes on the shared worker, both registered before either
    /// runs, as a batch delete of marks can be, and waits for the second. The
    /// worker is held until both are registered.
    fn delete_both(first: &Path, second: &Path) -> Task {
        let release = hold_worker();
        let (first_tx, _first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let first = PathInfo::try_from(first).unwrap();
        let second = PathInfo::try_from(second).unwrap();
        let started = TaskCommand::Delete(first).run(first_tx);
        assert!(started.cancel_info.is_some());
        let started = TaskCommand::Delete(second).run(second_tx);
        assert!(started.cancel_info.is_some());
        drop(release);
        await_end(&second_rx)
    }

    #[test]
    fn deleting_an_entry_inside_a_directory_deleted_first_succeeds() {
        let fx = TempDir::new("tasks_delete_nested_mark");
        let dir = fx.join("a");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("b"), b"x").unwrap();

        // Like `rm -rf a a/b`: the second finds its directory gone, which is
        // what it asked for.
        let task = delete_both(&dir, &dir.join("b"));

        assert!(!dir.exists());
        assert_eq!(None, task.error_message());
    }

    #[test_case(false ; "a file")]
    #[test_case(true  ; "a directory")]
    fn deleting_an_entry_already_deleted_succeeds(is_directory: bool) {
        let fx = TempDir::new("tasks_delete_twice");
        let path = fx.join("doomed");
        if is_directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, b"x").unwrap();
        }

        // Its directory is still there, so the second reaches the unlink, or
        // the open, of the entry itself.
        let task = delete_both(&path, &path);

        assert!(!path.exists());
        assert_eq!(None, task.error_message());
    }

    #[test]
    fn a_delete_of_an_entry_gone_before_it_starts_is_not_an_error() {
        let fx = TempDir::new("tasks_delete_gone");
        let path = fx.join("doomed");
        fs::write(&path, b"x").unwrap();
        let listed = PathInfo::try_from(path.as_path()).unwrap();
        fs::remove_file(&path).unwrap();

        // An earlier delete of the batch already ran, so there is nothing to
        // start and nothing to report.
        let result = run_to_end(TaskCommand::Delete(listed));

        assert!(matches!(result, Err(commands) if commands.is_empty()));
    }
}
