//! Helpers shared by the tests of the task modules.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use super::{
    TaskCommand,
    copy::{CopySettings, copy_with_progress, prepare_destination},
    finalize, finish_cross_device_move,
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

/// Pastes `old` into `dest` the way the worker does, from a selection made
/// before `change` runs: a copy, or the cross-device arm of
/// `run_move_task`, which removes what it copied. Returns the destination
/// and the finished task.
pub(super) fn paste_after(
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
        &path,
        CopySettings {
            preserve: is_move,
            conflicts: None,
        },
    ) {
        if is_move {
            finish_cross_device_move(active, outcome, &old_path, path.is_directory());
        } else {
            finalize(active, outcome.errors);
        }
    }
    (new_path, finished_task(&rx))
}
