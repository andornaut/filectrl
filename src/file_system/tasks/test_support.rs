//! Helpers shared by the tests of the task modules.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc,
    time::Duration,
};

use super::{
    PasteJob, TaskCommand, await_end, hold_worker, move_across_devices, validate::validate_paths,
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
    copy_task_with(tx, 1).0
}

/// A copy task of `total` bytes sending to `tx`, with its cancel token.
pub(super) fn copy_task_with(
    tx: std::sync::mpsc::Sender<Command>,
    total: u64,
) -> (ActiveTask, crate::command::progress::CancellationToken) {
    let (active, _, token) = ActiveTask::new(
        tx,
        TaskKind::Copy(Transfer {
            source: String::new(),
            destination: String::new(),
        }),
        total,
    );
    (active, token)
}

/// Runs `task` on the shared worker and waits for it. `Err` carries the alerts of a task refused
/// before it started.
pub(super) fn run_to_end(task: TaskCommand) -> Result<Task, Vec<Command>> {
    let (tx, rx) = mpsc::channel();
    let result = task.run(tx);
    if result.cancel_info.is_none() {
        return Err(result.command_result.into_commands());
    }
    Ok(await_end(&rx))
}

pub(super) fn mode_of(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode()
}

pub(super) fn finished_task(rx: &mpsc::Receiver<Command>) -> Task {
    let mut last = None;
    while let Ok(command) = rx.try_recv() {
        if let Command::Progress(task) = command {
            last = Some(task);
        }
    }
    last.expect("the task sent no progress")
}

/// Pastes `old` into `dest` from a selection made before `change` runs: a copy through the shared
/// worker, or a cross-device move (`move_across_devices`). Returns the destination and the finished
/// task. Runs under a deadline, cancelling the task before failing.
pub(super) fn paste_after(
    is_move: bool,
    dest: &Path,
    old: &Path,
    change: impl FnOnce(),
) -> (PathBuf, Task) {
    transfer(is_move, false, dest, old, change, || false)
}

/// `paste_after` with the replacement of what holds the name already granted.
pub(super) fn paste_over(is_move: bool, dest: &Path, old: &Path) -> (PathBuf, Task) {
    transfer(is_move, true, dest, old, || {}, || false)
}

fn transfer(
    is_move: bool,
    overwrite: bool,
    dest: &Path,
    old: &Path,
    change: impl FnOnce(),
    runaway: impl Fn() -> bool,
) -> (PathBuf, Task) {
    let path = PathInfo::try_from(old).unwrap();
    let dest = PathInfo::try_from(dest).unwrap();
    let new_path = dest.path.join(old.file_name().unwrap());
    if is_move {
        let (old_path, moved_to) = validate_paths(&path, &dest, true, overwrite).ok().unwrap();
        change();
        let task = on_a_thread(runaway, move |active| {
            move_across_devices(overwrite, active, &old_path, &moved_to, &path);
        });
        return (new_path, task);
    }
    // Validated and registered before `change`, as a paste does; run only once the worker is let
    // go.
    let gate = hold_worker();
    let (tx, rx) = mpsc::channel();
    let started = TaskCommand::paste(PasteJob {
        is_move: false,
        overwrite,
        dest,
        source: path,
    })
    .run(tx);
    let token = started.cancel_info.expect("the copy should start").token;
    change();
    drop(gate);
    (new_path, watched(&rx, &token, runaway))
}

const DEADLINE: Duration = Duration::from_secs(10);

/// Waits for the task on `rx` to end, cancelling it and failing if `runaway` holds or it outlasts
/// the deadline.
fn watched(
    rx: &mpsc::Receiver<Command>,
    token: &CancellationToken,
    runaway: impl Fn() -> bool,
) -> Task {
    const POLL: Duration = Duration::from_millis(5);
    let started = std::time::Instant::now();
    loop {
        match rx.recv_timeout(POLL) {
            Ok(Command::Progress(task)) if task.is_terminal() => return task,
            Err(mpsc::RecvTimeoutError::Disconnected) => panic!("the task ended without a word"),
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let failure = if runaway() {
            "the paste ran away"
        } else if started.elapsed() > DEADLINE {
            "the paste did not finish in time"
        } else {
            continue;
        };
        token.cancel();
        await_end(rx);
        panic!("{failure}");
    }
}

/// Runs `run` on its own thread with a fresh task, watched as `watched` does.
fn on_a_thread(runaway: impl Fn() -> bool, run: impl FnOnce(ActiveTask) + Send + 'static) -> Task {
    let (tx, rx) = mpsc::channel();
    let (active, token) = copy_task_with(tx, 1);
    let worker = std::thread::spawn(move || run(active));
    let task = watched(&rx, &token, runaway);
    worker.join().unwrap();
    task
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Kind {
    File,
    Symlink,
    Fifo,
    Directory,
}

pub(super) fn src_and_dest(label: &str) -> (TempDir, PathBuf, PathBuf) {
    let fx = TempDir::new(label);
    let (src, dest) = (fx.join("src"), fx.join("dest"));
    fs::create_dir(&src).unwrap();
    fs::create_dir(&dest).unwrap();
    (fx, src, dest)
}

/// Makes an entry of `kind` at `path` marked with `mark`: a file's bytes, a symlink's target, or a
/// directory's one file.
pub(super) fn make(kind: Kind, mark: &str, path: &Path) {
    match kind {
        Kind::File => fs::write(path, mark).unwrap(),
        Kind::Symlink => std::os::unix::fs::symlink(mark, path).unwrap(),
        Kind::Fifo => {
            nix::unistd::mkfifo(path, nix::sys::stat::Mode::from_bits_truncate(0o644)).unwrap();
        }
        Kind::Directory => {
            fs::create_dir(path).unwrap();
            fs::write(path.join("inner"), mark).unwrap();
        }
    }
}

/// Asserts `path` is the entry `make(kind, mark, path)` made.
pub(super) fn assert_made(case: &str, kind: Kind, mark: &str, path: &Path) {
    let metadata = fs::symlink_metadata(path).unwrap();
    match kind {
        Kind::File => assert_eq!(mark.as_bytes(), fs::read(path).unwrap(), "{case}"),
        Kind::Symlink => {
            assert_eq!(PathBuf::from(mark), fs::read_link(path).unwrap(), "{case}");
        }
        Kind::Fifo => {
            use std::os::unix::fs::FileTypeExt;
            assert!(metadata.file_type().is_fifo(), "{case}");
        }
        Kind::Directory => {
            let names: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(vec![std::ffi::OsString::from("inner")], names, "{case}");
            assert_eq!(
                mark.as_bytes(),
                fs::read(path.join("inner")).unwrap(),
                "{case}"
            );
        }
    }
}

/// Runs `check` for every case, then fails once naming each failed case.
pub(super) fn every_case<T: std::fmt::Debug>(
    cases: impl IntoIterator<Item = T>,
    check: impl Fn(&T),
) {
    let failed: Vec<String> = cases
        .into_iter()
        .filter_map(|case| {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&case)));
            outcome.is_err().then(|| format!("{case:?}"))
        })
        .collect();
    assert!(
        failed.is_empty(),
        "{} cases failed: {failed:#?}",
        failed.len()
    );
}

pub(super) struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A fresh writable directory on another filesystem than `fx`'s, or `None` (macOS has no
/// `/dev/shm`).
pub(super) fn other_device(fx: &TempDir) -> Option<(RemoveOnDrop, PathBuf)> {
    use std::os::unix::fs::MetadataExt;
    let device = fs::metadata(fx.path()).ok()?.dev();
    let candidates = [
        PathBuf::from("/dev/shm"),
        PathBuf::from(format!("/run/user/{}", nix::unistd::getuid())),
    ];
    let other = candidates.into_iter().find(|dir| {
        fs::metadata(dir).is_ok_and(|metadata| metadata.dev() != device)
            && nix::unistd::access(dir, nix::unistd::AccessFlags::W_OK).is_ok()
    })?;
    let dir = other.join(format!(
        "filectrl-other-{}-{}",
        std::process::id(),
        fx.path().file_name()?.to_string_lossy()
    ));
    fs::create_dir(&dir).ok()?;
    Some((RemoveOnDrop(dir.clone()), dir))
}
