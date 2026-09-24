//! Helpers shared by the tests of the task modules.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use super::{
    TaskCommand, copy_to, move_across_devices,
    sys::{AtFlags, CWD, fstatat},
    validate::validate_paths,
    walk::DirId,
};
use crate::{
    command::{
        Command,
        progress::{ActiveTask, CancellationToken, Task, TaskKind, Transfer},
    },
    file_system::path_info::PathInfo,
    test_support::TempDir,
};

pub(super) fn copy_task(tx: std::sync::mpsc::Sender<Command>) -> ActiveTask {
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

/// Runs `task` on the shared worker and waits for how it finished. `Err`
/// carries the alerts of a task that was refused before it started.
pub(super) fn run_to_end(task: TaskCommand) -> Result<Task, Vec<Command>> {
    let (tx, rx) = mpsc::channel();
    let result = task.run(tx, None);
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

pub(super) fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode()
}

/// The last task the operation sent, which carries how it finished.
pub(super) fn finished_task(rx: &mpsc::Receiver<Command>) -> Task {
    let mut last = None;
    while let Ok(command) = rx.try_recv() {
        if let Command::Progress(task) = command {
            last = Some(task);
        }
    }
    last.expect("the task sent no progress")
}

/// The identity of the entry `path` names now.
pub(super) fn id_of(path: &Path) -> DirId {
    DirId::of_stat(&fstatat(CWD, path, AtFlags::AT_SYMLINK_NOFOLLOW).unwrap())
}

/// A source file holding "src", a destination file holding "dest", and an
/// `ActiveTask` for the operation that would replace one with the other.
/// The receiver is leaked for the same reason as in `raced_parts`.
pub(super) fn destination(
    label: &str,
) -> (TempDir, PathBuf, PathBuf, ActiveTask, CancellationToken) {
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

/// Pastes `old` into `dest` through the worker's own code, from a selection
/// made before `change` runs: a copy, or the cross-device arm of
/// `run_move_task`, which removes what it copied, whatever devices the two
/// are on. Returns the destination and the finished task.
///
/// The paste runs on its own thread under a deadline, so a copy that never
/// ends fails the test rather than hanging it; the task is cancelled before
/// the panic, so the runaway stops.
pub(super) fn paste_after(
    old: &Path,
    dest: &Path,
    is_move: bool,
    change: impl FnOnce(),
) -> (PathBuf, Task) {
    paste_after_unless(old, dest, is_move, change, || false)
}

/// `paste_after`, cancelled and failed as soon as `runaway` holds. A copy
/// descending into its own output grows a tree deeper with every level, and
/// one left to the deadline is too deep for `TempDir` to remove, so a test
/// that can recognize that shape early stops it at a depth that can be.
pub(super) fn paste_after_unless(
    old: &Path,
    dest: &Path,
    is_move: bool,
    change: impl FnOnce(),
    runaway: impl Fn() -> bool,
) -> (PathBuf, Task) {
    transfer(old, dest, is_move, false, change, runaway)
}

/// `paste_after` with the replacement of what holds the name already granted,
/// as a conflict answer grants it.
pub(super) fn paste_over(old: &Path, dest: &Path, is_move: bool) -> (PathBuf, Task) {
    transfer(old, dest, is_move, true, || {}, || false)
}

fn transfer(
    old: &Path,
    dest: &Path,
    is_move: bool,
    overwrite: bool,
    change: impl FnOnce(),
    runaway: impl Fn() -> bool,
) -> (PathBuf, Task) {
    const DEADLINE: Duration = Duration::from_secs(10);
    const POLL: Duration = Duration::from_millis(5);

    let verb = if is_move { "move" } else { "copy" };
    let path = PathInfo::try_from(old).unwrap();
    let dest = PathInfo::try_from(dest).unwrap();
    let (old_path, new_path) = validate_paths(&path, &dest, verb, overwrite).ok().unwrap();
    change();
    let (tx, rx) = mpsc::channel();
    let (active, _, token) = ActiveTask::new(
        tx,
        TaskKind::Copy(Transfer {
            source: String::new(),
            destination: String::new(),
        }),
        1,
    );
    let (done_tx, done_rx) = mpsc::channel();
    let destination = new_path.clone();
    let worker = std::thread::spawn(move || {
        let transfer = if is_move {
            move_across_devices
        } else {
            copy_to
        };
        transfer(active, &old_path, &destination, &path, overwrite, None);
        let _ = done_tx.send(());
    });
    let started = std::time::Instant::now();
    loop {
        match done_rx.recv_timeout(POLL) {
            Ok(()) => break,
            Err(_) if runaway() => {
                token.cancel();
                worker.join().unwrap();
                panic!("the paste ran away");
            }
            Err(_) if started.elapsed() > DEADLINE => {
                token.cancel();
                worker.join().unwrap();
                panic!("the paste did not finish within {DEADLINE:?}");
            }
            Err(_) => {}
        }
    }
    worker.join().unwrap();
    (new_path, finished_task(&rx))
}
