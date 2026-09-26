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

use self::{
    copy::{CopyOutcome, copy_with_progress},
    remove::{Removal, dir_total_entries, remove_path},
    validate::{display_path, start_transfer},
};
pub(super) use self::{
    sys::set_mode_at,
    validate::{is_same_file, onto_itself, rename_no_replace, restat},
};
use super::{
    conflicts::failed_transfer,
    path_info::{PathInfo, compact},
};
use crate::command::{
    Command,
    progress::{ActiveTask, CancellationToken, Task, TaskKind},
    result::CommandResult,
};

const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size
/// Shortest gap between two progress updates for one task. The percentage bounds updates per unit
/// of work; this bounds them per unit of time.
const PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(100);

type Job = Box<dyn FnOnce() + Send>;

/// Runs file-operation jobs one at a time, in queue order, on a single background thread started on
/// first use. Each task validates and registers itself on the calling thread, so a batch reports
/// failed sources before any starts.
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
    // The worker never exits; only a panicking test job (the test profile unwinds) can end it.
    let sent = queue.send(Box::new(job));
    #[cfg(test)]
    sent.expect("the worker thread ended: an earlier job panicked");
    #[cfg(not(test))]
    let _ = sent;
}

/// Holds the shared worker until the returned sender is dropped, so work queued meanwhile is
/// validated before any of it runs.
#[cfg(test)]
pub(in crate::file_system) fn hold_worker() -> Sender<()> {
    let (release, gate) = mpsc::channel::<()>();
    queue_operation(move || {
        let _ = gate.recv();
    });
    release
}

/// Blocks until the task reporting on `rx` ends and returns how it ended.
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
    /// Set when the task can no longer be cancelled; the cancel stack drops such entries.
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

    /// The initial progress snapshot was already sent before the worker was spawned, so no command
    /// is returned.
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

pub(in crate::file_system) enum TaskCommand {
    Delete(PathInfo),
    Paste(Box<PasteJob>),
}

/// One source of a paste: `source` moved (`is_move`) or copied into `dest`. When `overwrite`, it
/// replaces whatever holds its name when the worker runs, like `cp -f` and `mv -f`; otherwise a
/// taken name fails the task.
pub(in crate::file_system) struct PasteJob {
    pub(in crate::file_system) is_move: bool,
    pub(in crate::file_system) overwrite: bool,
    pub(in crate::file_system) dest: PathInfo,
    pub(in crate::file_system) source: PathInfo,
}

impl TaskCommand {
    pub(in crate::file_system) fn paste(job: PasteJob) -> Self {
        Self::Paste(Box::new(job))
    }

    pub(in crate::file_system) fn run(self, tx: Sender<Command>) -> TaskRunResult {
        match self {
            TaskCommand::Delete(path) => run_delete_task(tx, &path),
            TaskCommand::Paste(job) => run_paste_task(tx, *job),
        }
    }
}

/// Starts one source of a paste: validated and registered here, then run on the worker.
fn run_paste_task(tx: Sender<Command>, job: PasteJob) -> TaskRunResult {
    let PasteJob {
        is_move,
        overwrite,
        dest,
        source,
    } = job;
    let (path, old_path, new_path, kind) = match start_transfer(is_move, &dest, overwrite, &source)
    {
        Ok(started) => started,
        Err(result) => return TaskRunResult::failed(result),
    };

    // A directory's real total is scanned in the worker and applied via `active.set_total`.
    let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
    active.send_progress();
    let uncancellable = active.uncancellable_handle();

    queue_operation(move || {
        let Some(active) = check_cancelled(active) else {
            return;
        };
        if is_move {
            move_to(overwrite, active, &old_path, &new_path, &path);
        } else {
            copy_to(overwrite, active, &old_path, &new_path, &path);
        }
    });

    TaskRunResult::started(&initial, token, uncancellable)
}

/// The worker side of a validated move: a rename (over what holds the name when `overwrite`), or a
/// copy and removal across devices (`move_across_devices`).
fn move_to(
    overwrite: bool,
    mut active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
) {
    let renamed = if overwrite {
        fs::rename(old_path, new_path)
    } else {
        rename_no_replace(old_path, new_path)
    };
    match renamed {
        Ok(()) => {
            active.increment(path.size);
            active.done();
        }
        Err(error) if error.kind() == ErrorKind::CrossesDevices => {
            move_across_devices(overwrite, active, old_path, new_path, path);
        }
        Err(error) => active.error(failed_transfer(true, old_path, new_path, &error)),
    }
}

fn run_delete_task(tx: Sender<Command>, path: &PathInfo) -> TaskRunResult {
    let path = match restat(path, "delete") {
        Ok(fresh) => fresh,
        // Already gone, like `rm -f`: an earlier delete of the batch may have removed it.
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
    // Delete progress counts entries, not bytes. A directory's total is scanned in the worker.
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

/// The worker side of a validated copy.
fn copy_to(overwrite: bool, active: ActiveTask, old_path: &Path, new_path: &Path, path: &PathInfo) {
    if let Some((active, outcome)) =
        copy_with_progress(false, overwrite, path, active, old_path, new_path)
    {
        finalize(active, outcome.errors);
    }
}

/// A move whose rename crossed devices: copies `path`, then removes the source.
fn move_across_devices(
    overwrite: bool,
    active: ActiveTask,
    old_path: &Path,
    new_path: &Path,
    path: &PathInfo,
) {
    if let Some((active, outcome)) =
        copy_with_progress(true, overwrite, path, active, old_path, new_path)
    {
        finish_cross_device_move(active, outcome, old_path, new_path, path.is_directory());
    }
}

/// Finishes a cross-device move like `mv`: the source is removed only after a copy with no
/// errors. Otherwise the whole source and the partial destination are kept.
fn finish_cross_device_move(
    active: ActiveTask,
    outcome: CopyOutcome,
    old_path: &Path,
    new_path: &Path,
    is_directory: bool,
) {
    if let Some(summary) = summarize(outcome.errors) {
        active.error(summary);
        return;
    }
    // The copy is complete, so removing the source is the only way to finish; a cancel no longer
    // applies.
    active.set_uncancellable();
    if let Some((active, errors)) =
        remove_path(old_path, is_directory, active, Removal::MovedSource)
    {
        // The destination is whole; only the cleanup failed.
        match summarize(errors) {
            Some(summary) => active.error(format!(
                "Failed to remove the original {} once it was moved to {}: {summary}",
                compact(old_path),
                compact(new_path)
            )),
            None => active.done(),
        }
    }
}

/// Finalizes a copy, move or delete like coreutils: success when no per-entry error was recorded,
/// otherwise one alert summarizing them.
fn finalize(active: ActiveTask, errors: Vec<String>) {
    match summarize(errors) {
        Some(summary) => active.error(summary),
        None => active.done(),
    }
}

/// One line for the errors a task recorded, the first with a count of the rest. Every error is
/// logged.
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

/// Finalizes a cancelled task, logging each error recorded before the cancel.
fn cancel_logging(errors: &[String], active: ActiveTask) {
    for error in errors {
        warn!("{error}");
    }
    active.cancelled();
}

/// Honors a cancel that landed while the task was queued. `None` when the task was finalized here.
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
            Kind, assert_made, copy_task, every_case, finished_task, make, paste_after, paste_over,
            run_to_end, src_and_dest,
        },
        *,
    };
    use crate::{command::progress::Transfer, test_support::TempDir};

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

        // `cp` does not preserve timestamps without `-p`.
        let task = run_to_end(TaskCommand::paste(PasteJob {
            is_move: false,
            overwrite: false,
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

    /// Like `cp -R`, the directory is made and the failure to list the source is reported.
    #[test]
    fn a_copy_of_an_unreadable_directory_reports_it() {
        let fx = TempDir::new("tasks_copy_unreadable");
        let src = fx.join("locked");
        fs::create_dir(&src).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o000)).unwrap();
        // Root lists a mode-000 directory anyway; probe rather than inspect the euid.
        let is_unreadable = fs::read_dir(&src).is_err();
        let dst = fx.join("dst");
        fs::create_dir(&dst).unwrap();

        let task = run_to_end(TaskCommand::paste(PasteJob {
            is_move: false,
            overwrite: false,
            dest: PathInfo::try_from(dst.as_path()).unwrap(),
            source: PathInfo::try_from(src.as_path()).unwrap(),
        }))
        .expect("the copy should start");
        fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).unwrap();
        // The copy takes the source's mode; restore it so the fixture can be removed.
        let _ = fs::set_permissions(dst.join("locked"), fs::Permissions::from_mode(0o755));

        if is_unreadable {
            assert_eq!(
                Some(format!(
                    "Failed to read directory {}: Permission denied (os error 13)",
                    compact(&src)
                )),
                task.error_message()
            );
        } else {
            eprintln!("skipped: a mode-000 directory can be listed here");
            assert_eq!(None, task.error_message());
        }
    }

    /// Write access stays: moving a directory to another parent rewrites its `..`.
    #[test]
    fn a_move_of_an_unreadable_directory_is_not_refused_up_front() {
        let fx = TempDir::new("tasks_move_unreadable");
        let src = fx.join("locked");
        fs::create_dir(&src).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o300)).unwrap();
        let dst = fx.join("dst");
        fs::create_dir(&dst).unwrap();

        let result = run_to_end(TaskCommand::paste(PasteJob {
            is_move: true,
            overwrite: false,
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

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum Op {
        Copy,
        Move,
        /// A move whose rename crossed devices, queued straight to `move_across_devices` so it runs
        /// on any machine.
        Across,
    }

    const OPS: [Op; 3] = [Op::Copy, Op::Move, Op::Across];

    /// Validates and queues `source` into `dest` as the paste queue does. `Err` carries the alerts
    /// of a job refused before it was queued.
    fn start(
        op: Op,
        overwrite: bool,
        dest: &Path,
        source: &Path,
    ) -> Result<mpsc::Receiver<Command>, Vec<Command>> {
        start_cancellable(op, overwrite, dest, source).map(|(rx, _)| rx)
    }

    fn start_cancellable(
        op: Op,
        overwrite: bool,
        dest: &Path,
        source: &Path,
    ) -> Result<(mpsc::Receiver<Command>, CancellationToken), Vec<Command>> {
        let (tx, rx) = mpsc::channel();
        let path = PathInfo::try_from(source).unwrap();
        let dir = PathInfo::try_from(dest).unwrap();
        let result = match op {
            Op::Copy | Op::Move => TaskCommand::paste(PasteJob {
                is_move: op == Op::Move,
                overwrite,
                dest: dir,
                source: path,
            })
            .run(tx),
            Op::Across => {
                let (path, old_path, new_path, kind) =
                    match start_transfer(true, &dir, overwrite, &path) {
                        Ok(started) => started,
                        Err(result) => return Err(result.into_commands()),
                    };
                let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
                let uncancellable = active.uncancellable_handle();
                queue_operation(move || {
                    move_across_devices(overwrite, active, &old_path, &new_path, &path);
                });
                TaskRunResult::started(&initial, token, uncancellable)
            }
        };
        match result.cancel_info {
            Some(info) => Ok((rx, info.token)),
            None => Err(result.command_result.into_commands()),
        }
    }

    /// The names in `dir`, sorted: a replacement leaves no temporary name behind.
    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Without a granted overwrite, a name taken after the paste saw it free fails the task and
    /// the entry there is kept, like `cp -n`.
    #[test]
    fn a_name_taken_after_the_paste_saw_it_free_is_not_replaced() {
        const KINDS: [Kind; 4] = [Kind::File, Kind::Symlink, Kind::Fifo, Kind::Directory];
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| KINDS.map(move |source| (op, source)))
            .flat_map(|(op, source)| KINDS.map(move |occupant| (op, source, occupant)))
            .collect();

        every_case(cases, |&(op, source_kind, occupant)| {
            let case = format!("{op:?}, {source_kind:?} onto {occupant:?}");
            let (_fx, src, dest) = src_and_dest("tasks_raced");
            let (old, new) = (src.join("one"), dest.join("one"));
            make(source_kind, "source", &old);

            let gate = hold_worker();
            let job = start(op, false, &dest, &old).expect("validated while free");
            make(occupant, "occupant", &new);
            drop(gate);
            let task = await_end(&job);

            let message = task.error_message().expect("the task fails");
            assert!(message.contains("File exists"), "{case}: {message}");
            assert_made(&case, occupant, "occupant", &new);
            assert_made(&case, source_kind, "source", &old);
        });
    }

    /// The later paste validated while the name was free, so it is not allowed to replace.
    #[test]
    fn a_later_paste_does_not_replace_what_an_earlier_one_left() {
        let fx = TempDir::new("tasks_two_pastes");
        let (x, y, dest) = (fx.join("x"), fx.join("y"), fx.join("dest"));
        for dir in [&x, &y, &dest] {
            fs::create_dir(dir).unwrap();
        }
        fs::write(x.join("a"), b"x").unwrap();
        fs::write(y.join("a"), b"y").unwrap();

        let gate = hold_worker();
        let first_job = start(Op::Move, false, &dest, &x.join("a")).unwrap();
        let second_job = start(Op::Move, false, &dest, &y.join("a")).unwrap();
        drop(gate);

        assert_eq!(None, await_end(&first_job).error_message());
        assert_eq!(
            Some(format!(
                "Failed to move {} to {}: File exists (os error 17)",
                compact(&y.join("a")),
                compact(&dest.join("a"))
            )),
            await_end(&second_job).error_message()
        );
        assert_eq!(b"x".to_vec(), fs::read(dest.join("a")).unwrap());
        assert_eq!(b"y".to_vec(), fs::read(y.join("a")).unwrap());
        assert!(!x.join("a").exists());
    }

    /// What holds a name the queue was told to replace, by the time the job runs.
    #[derive(Clone, Copy, Debug)]
    enum Since {
        Unchanged,
        Removed,
        /// Another entry of this kind, in the seen entry's place.
        Replaced(Kind),
    }

    const SINCE: [Since; 6] = [
        Since::Unchanged,
        Since::Removed,
        Since::Replaced(Kind::File),
        Since::Replaced(Kind::Symlink),
        Since::Replaced(Kind::Fifo),
        Since::Replaced(Kind::Directory),
    ];

    /// Like `cp -f` and `mv -f`: the overwrite is for the name, so whatever non-directory holds it
    /// when the job runs is replaced. A directory never is.
    #[test]
    fn a_granted_overwrite_replaces_whatever_holds_the_name() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| SINCE.into_iter().map(move |since| (op, since)))
            .collect();

        every_case(cases, |&(op, since)| {
            let case = format!("{op:?}, {since:?}");
            let (_fx, src, dest) = src_and_dest("tasks_granted");
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();

            let gate = hold_worker();
            let job = start(op, true, &dest, &old).unwrap();
            match since {
                Since::Unchanged => {}
                Since::Removed => fs::remove_file(&new).unwrap(),
                Since::Replaced(kind) => {
                    fs::remove_file(&new).unwrap();
                    make(kind, "since", &new);
                }
            }
            drop(gate);
            let task = await_end(&job);

            if let Since::Replaced(Kind::Directory) = since {
                let message = task.error_message().expect("a directory is never replaced");
                assert!(message.contains("Is a directory"), "{case}: {message}");
                assert_made(&case, Kind::Directory, "since", &new);
                assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
            } else {
                assert_eq!(None, task.error_message(), "{case}");
                assert_eq!(b"source".to_vec(), fs::read(&new).unwrap(), "{case}");
                assert_eq!(op == Op::Copy, old.exists(), "{case}");
            }
            assert_eq!(vec!["one"], names_in(&dest), "{case}");
        });
    }

    #[test]
    fn a_granted_overwrite_whose_source_is_gone_leaves_the_entry() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| [Kind::File, Kind::Symlink].map(move |kind| (op, kind)))
            .collect();

        every_case(cases, |&(op, kind)| {
            let case = format!("{op:?}, {kind:?}");
            let (_fx, src, dest) = src_and_dest("tasks_granted_source_gone");
            let (old, new) = (src.join("one"), dest.join("one"));
            make(kind, "source", &old);
            fs::write(&new, b"seen").unwrap();

            let gate = hold_worker();
            let job = start(op, true, &dest, &old).unwrap();
            fs::remove_file(&old).unwrap();
            drop(gate);
            let task = await_end(&job);

            let verb = if op == Op::Copy { "copy" } else { "move" };
            assert_eq!(
                Some(format!(
                    "Failed to {verb} {} to {}: No such file or directory (os error 2)",
                    compact(&old),
                    compact(&new)
                )),
                task.error_message(),
                "{case}"
            );
            assert_eq!(b"seen".to_vec(), fs::read(&new).unwrap(), "{case}");
            assert_eq!(vec!["one"], names_in(&dest), "{case}");
        });
    }

    #[test]
    fn a_granted_overwrite_replaces_with_every_kind_of_source() {
        let cases: Vec<_> = OPS
            .into_iter()
            .flat_map(|op| [Kind::File, Kind::Symlink, Kind::Fifo].map(move |kind| (op, kind)))
            .collect();

        every_case(cases, |&(op, kind)| {
            let case = format!("{op:?}, {kind:?}");
            let (_fx, src, dest) = src_and_dest("tasks_granted_kinds");
            let (old, new) = (src.join("one"), dest.join("one"));
            make(kind, "source", &old);
            fs::write(&new, b"seen").unwrap();

            let job = start(op, true, &dest, &old).unwrap();
            let task = await_end(&job);

            assert_eq!(None, task.error_message(), "{case}");
            assert_made(&case, kind, "source", &new);
            assert_eq!(op == Op::Copy, old.symlink_metadata().is_ok(), "{case}");
            assert_eq!(vec!["one"], names_in(&dest), "{case}");
        });
    }

    #[test]
    fn a_granted_overwrite_of_a_symlink_replaces_the_link() {
        every_case(OPS, |&op| {
            let case = format!("{op:?}");
            let (_fx, src, dest) = src_and_dest("tasks_granted_link");
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(dest.join("target"), b"target").unwrap();
            std::os::unix::fs::symlink("target", &new).unwrap();

            let job = start(op, true, &dest, &old).unwrap();
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

    #[test]
    fn a_granted_overwrite_cancelled_while_queued_does_nothing() {
        every_case([Op::Copy, Op::Move], |&op| {
            let case = format!("{op:?}");
            let (_fx, src, dest) = src_and_dest("tasks_granted_cancelled");
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();

            let gate = hold_worker();
            let (job, token) = start_cancellable(op, true, &dest, &old).unwrap();
            token.cancel();
            drop(gate);
            let task = await_end(&job);

            assert!(task.is_cancelled(), "{case}: {:?}", task.error_message());
            assert_eq!(b"seen".to_vec(), fs::read(&new).unwrap(), "{case}");
            assert_eq!(b"source".to_vec(), fs::read(&old).unwrap(), "{case}");
            assert_eq!(vec!["one"], names_in(&dest), "{case}");
        });
    }

    /// Probed rather than skipped for root, who can write to a read-only directory.
    #[test]
    fn a_granted_overwrite_that_cannot_write_beside_the_entry_reports_it() {
        every_case([Op::Copy, Op::Across], |&op| {
            let case = format!("{op:?}");
            let (_fx, src, dest) = src_and_dest("tasks_granted_locked");
            let (old, new) = (src.join("one"), dest.join("one"));
            fs::write(&old, b"source").unwrap();
            fs::write(&new, b"seen").unwrap();

            let gate = hold_worker();
            let job = start(op, true, &dest, &old).unwrap();
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).unwrap();
            let locked = fs::write(dest.join("probe"), b"").is_err();
            drop(gate);
            let task = await_end(&job);
            fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).unwrap();

            if locked {
                let verb = if op == Op::Copy { "copy" } else { "move" };
                assert_eq!(
                    Some(format!(
                        "Failed to {verb} {} to {}: Permission denied (os error 13)",
                        compact(&old),
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
        // root, a.txt, sub, sub/b.txt
        assert_eq!(4, task.progress().total);
    }

    fn moved(label: &str) -> (TempDir, PathBuf, mpsc::Receiver<Command>, ActiveTask) {
        let fx = TempDir::new(label);
        let src = fx.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("a.txt"), b"src").unwrap();
        let (tx, rx) = mpsc::channel();
        let active = copy_task(tx);
        (fx, src, rx, active)
    }

    #[test]
    fn a_cross_device_move_removes_its_source_after_a_clean_copy() {
        let (_fx, src, rx, active) = moved("tasks_move_complete");

        finish_cross_device_move(active, CopyOutcome::default(), &src, &src, true);

        assert!(!src.exists());
        assert_eq!(None, finished_task(&rx).error_message());
    }

    #[test]
    fn a_cross_device_move_that_cannot_remove_its_source_says_it_was_moved() {
        let (fx, src, rx, active) = moved("tasks_move_unremovable");
        fs::create_dir(src.join("sub")).unwrap();
        fs::write(src.join("sub").join("b.txt"), b"src").unwrap();
        fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o555)).unwrap();
        let locked = fs::write(src.join("sub").join("probe"), b"").is_err();
        let dest = fx.join("dest");

        finish_cross_device_move(active, CopyOutcome::default(), &src, &dest, true);

        fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o755)).unwrap();
        if !locked {
            eprintln!("not exercised: a read-only directory is writable here");
            return;
        }
        assert_eq!(
            Some(format!(
                "Failed to remove the original {} once it was moved to {}: Failed to delete \
                 {}: Permission denied (os error 13)",
                compact(&src),
                compact(&dest),
                compact(&src.join("sub").join("b.txt"))
            )),
            finished_task(&rx).error_message()
        );
    }

    /// The cancel key checks the stage first, but one that reaches the token anyway must not leave
    /// the source part removed.
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

        finish_cross_device_move(active, CopyOutcome::default(), &src, &src, true);

        assert!(src.symlink_metadata().is_err());
        let task = finished_task(&rx);
        assert!(!task.is_cancelled());
        assert_eq!(None, task.error_message());
    }

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

    #[test]
    fn a_cross_device_move_keeps_its_source_when_an_entry_failed() {
        let (_fx, src, rx, active) = moved("tasks_move_failed");

        finish_cross_device_move(
            active,
            CopyOutcome {
                errors: vec!["a.txt could not be written".to_string()],
                ..CopyOutcome::default()
            },
            &src,
            &src,
            true,
        );

        assert!(src.join("a.txt").exists());
        assert_eq!(
            Some("a.txt could not be written".to_string()),
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

    /// Starts two deletes on the shared worker, both registered before either runs, and waits for
    /// the second.
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

        // Like `rm -rf a a/b`.
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

        let result = run_to_end(TaskCommand::Delete(listed));

        assert!(matches!(result, Err(commands) if commands.is_empty()));
    }
}
