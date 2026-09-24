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
    validate::{SameFile, display_path, rename_for_move, settle_raced_rename, start_transfer},
};
use super::{
    conflicts::Conflicts,
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
    pub fn run(self, tx: Sender<Command>, conflicts: Option<&Conflicts>) -> TaskRunResult {
        match self {
            TaskCommand::Copy(path, dir, overwrite) => {
                run_copy_task(tx, &path, &dir, overwrite, conflicts)
            }
            TaskCommand::Delete(path) => run_delete_task(tx, &path),
            TaskCommand::Move(path, dir, overwrite) => {
                run_move_task(tx, &path, &dir, overwrite, conflicts)
            }
        }
    }
}

fn run_copy_task(
    tx: Sender<Command>,
    path: &PathInfo,
    dir: &PathInfo,
    overwrite: bool,
    conflicts: Option<&Conflicts>,
) -> TaskRunResult {
    let conflicts = conflicts.cloned();
    let (path, old_path, new_path, kind) =
        match start_transfer("copy", TaskKind::Copy, dir, overwrite, path) {
            Ok(started) => started,
            Err(result) => return TaskRunResult::failed(result),
        };

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
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        if let Some(active) = check_cancelled(active) {
            copy_to(
                active,
                &old_path,
                &new_path,
                &path,
                overwrite,
                conflicts.as_ref(),
            );
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
) -> TaskRunResult {
    let conflicts = conflicts.cloned();
    let (path, old_path, new_path, kind) =
        match start_transfer("move", TaskKind::Move, dir, overwrite, path) {
            Ok(started) => started,
            Err(result) => return TaskRunResult::failed(result),
        };
    let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let is_directory = path.is_directory();
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        let Some(mut active) = check_cancelled(active) else {
            return;
        };
        let renamed = rename_for_move(&old_path, &new_path, overwrite).or_else(|error| {
            if error.kind() == ErrorKind::AlreadyExists {
                settle_raced_rename(conflicts.as_ref(), &old_path, &new_path, is_directory)
                    .unwrap_or(Err(error))
            } else {
                Err(error)
            }
        });
        match renamed {
            Ok(()) => {
                active.increment(path.size);
                active.done();
            }
            Err(error) => match error.kind() {
                // If the file is on a different device/mount-point, we must copy-then-delete it instead
                ErrorKind::CrossesDevices => move_across_devices(
                    active,
                    &old_path,
                    &new_path,
                    &path,
                    overwrite,
                    conflicts.as_ref(),
                ),
                _ if SameFile::is(&error) => active.error(format!(
                    "Cannot move {}: {} is the same file",
                    compact(&old_path),
                    compact(&new_path)
                )),
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

/// The worker side of a copy whose paths are validated: clears a granted
/// overwrite, copies `path`, and reports how it went.
fn copy_to(
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
    overwrite: bool,
    conflicts: Option<&Conflicts>,
) {
    let Some((active, source)) =
        prepare_destination(active, "copy", old_path, new_path, overwrite, path.mode())
    else {
        return;
    };
    let settings = CopySettings {
        // Like `cp`, which does not preserve timestamps without `-p`.
        preserve: false,
        conflicts,
    };
    if let Some((active, outcome)) =
        copy_with_progress(old_path, new_path, active, source, path, settings)
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
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
    overwrite: bool,
    conflicts: Option<&Conflicts>,
) {
    // There is no rename to replace the destination here, and the copy opens
    // it with `create_new`, so a granted overwrite has to clear it first.
    let Some((active, source)) =
        prepare_destination(active, "move", old_path, new_path, overwrite, path.mode())
    else {
        return;
    };
    let settings = CopySettings {
        // A same-device move is a rename, which keeps the timestamps; the copy
        // fallback has to put them back so the result does not depend on which
        // mount the destination happens to be on.
        preserve: true,
        conflicts,
    };
    let Some((active, outcome)) =
        copy_with_progress(old_path, new_path, active, source, path, settings)
    else {
        return;
    };
    finish_cross_device_move(active, outcome, old_path, path.is_directory());
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
        finalize(active, outcome.errors);
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
/// entries are not failures and do not appear. Every error is also logged.
fn finalize(active: ActiveTask, errors: Vec<String>) {
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
            copy_task, destination, finished_task, id_of, paste_after, paste_over, run_to_end,
        },
        *,
    };
    use crate::{
        command::progress::{Progress, Transfer},
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

    /// The whole arm `run_move_task` falls back to when the rename crosses
    /// devices, from the copy through the removal.
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
            paste_over(&old, &dest, true)
        } else {
            paste_after(&old, &dest, true, || {})
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

        let (new_path, task) = paste_over(&old, &dest, false);

        assert_eq!(None, task.error_message());
        assert_eq!(b"src".as_slice(), fs::read(&new_path).unwrap());
        assert_eq!(b"src".as_slice(), fs::read(&old).unwrap());
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

    /// Starts two deletes on the shared worker, both registered before either
    /// runs, as a batch delete of marks can be, and waits for the second. The
    /// worker is held until both are registered.
    fn delete_both(first: &Path, second: &Path) -> Task {
        let (release, gate) = mpsc::channel::<()>();
        queue_operation(move || {
            let _ = gate.recv();
        });
        let (first_tx, _first_rx) = mpsc::channel();
        let (second_tx, second_rx) = mpsc::channel();
        let first = PathInfo::try_from(first).unwrap();
        let second = PathInfo::try_from(second).unwrap();
        let started = TaskCommand::Delete(first).run(first_tx, None);
        assert!(started.cancel_info.is_some());
        let started = TaskCommand::Delete(second).run(second_tx, None);
        assert!(started.cancel_info.is_some());
        drop(release);
        loop {
            match second_rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Command::Progress(task)) if task.is_terminal() => return task,
                Ok(_) => {}
                Err(error) => panic!("the delete did not finish: {error}"),
            }
        }
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
