use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use anyhow::{Result, anyhow};
use log::{info, warn};

use super::path_info::PathInfo;
use crate::{
    command::{
        Command,
        progress::{ActiveTask, CancellationToken, Task, TaskKind},
        result::CommandResult,
    },
    file_system::debounce,
};

const BUFFER_SIZE_DIVISOR: u64 = 20;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size

pub type CancelInfo = (usize, CancellationToken, TaskKind);

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

    fn started(initial: Task, token: CancellationToken) -> Self {
        Self {
            cancel_info: Some((initial.id(), token, initial.kind().clone())),
            command_result: Command::Progress(initial).into(),
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

    info!("Copying {old_path:?} to {new_path:?}");
    let kind = TaskKind::Copy {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    };

    let is_directory = path.is_directory();
    // Pre-flight: if the directory's top level cannot even be read, fail fast
    // *before* the task is registered. The expensive part (the recursive size
    // walk) still runs off the UI thread below, but this cheap top-level read
    // catches the common failure (unreadable directory / permission denied)
    // without ever sending an initial progress snapshot. That preserves the
    // guarantee that a failed directory copy never orphans a progress notice:
    // a worker-thread terminal error could otherwise race ahead of the
    // main-thread initial snapshot and leave a stuck progress bar.
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

    thread::spawn(move || {
        let total_size = if is_directory {
            match dir_total_size(&old_path) {
                Ok(size) => {
                    active.set_total(size);
                    size
                }
                Err(error) => {
                    active.error(format!("Failed to read directory {old_path:?}: {error}"));
                    return;
                }
            }
        } else {
            file_size
        };
        let buffer_size = buffer_bytes(total_size, buffer_min_bytes, buffer_max_bytes);
        if let Some(active) = copy_path(
            &old_path,
            &new_path,
            active,
            buffer_size,
            is_directory,
            source_mode,
        ) {
            active.done();
        }
    });

    TaskRunResult::started(initial, token)
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

    info!("Moving {old_path:?} to {new_path:?}");
    let kind = TaskKind::Move {
        source: display_path(&old_path),
        destination: display_path(&new_path),
    };
    let (mut active, initial, token) = ActiveTask::new(tx, kind, path.size);
    let size = path.size;
    let source_mode = path.mode();
    let is_directory = path.is_directory();
    let buffer_size = buffer_bytes(size, buffer_min_bytes, buffer_max_bytes);

    thread::spawn(move || match fs::rename(&old_path, &new_path) {
        Ok(_) => {
            active.increment(size);
            active.done();
        }
        Err(error) => match error.kind() {
            // If the file is on a different device/mount-point, we must copy-then-delete it instead
            ErrorKind::CrossesDevices => {
                if let Some(active) = copy_path(
                    &old_path,
                    &new_path,
                    active,
                    buffer_size,
                    is_directory,
                    source_mode,
                ) {
                    match remove_path(&old_path, is_directory) {
                        Ok(_) => active.done(),
                        Err(error) => active.error(format!(
                            "Copy succeeded, but failed to delete original {old_path:?}: {error}"
                        )),
                    }
                }
            }
            _ => active.error(format!(
                "Failed to move {old_path:?} to {new_path:?}: {error}"
            )),
        },
    });

    TaskRunResult::started(initial, token)
}

fn run_delete_task(tx: Sender<Command>, path: PathInfo) -> TaskRunResult {
    let kind = TaskKind::Delete {
        path: display_path(Path::new(&path.path)),
    };
    let (active, initial, token) = ActiveTask::new(tx, kind, path.size);
    // Use PathInfo::is_directory() (lstat-based, does not follow symlinks) so that a
    // symlink to a directory is removed with remove_file(), not remove_dir_all().
    // PathBuf::is_dir() follows symlinks and would incorrectly recurse into the target.
    let is_directory = path.is_directory();
    let path = PathBuf::from(&path.path);
    info!("Deleting {path:?}");

    thread::spawn(move || match remove_path(&path, is_directory) {
        Ok(_) => active.done(),
        Err(error) => active.error(format!("Failed to delete {path:?}: {error}")),
    });

    TaskRunResult::started(initial, token)
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

fn dir_total_size(path: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_dir() {
            total += dir_total_size(&entry.path())?;
        } else if !metadata.is_symlink() {
            total += metadata.len();
        }
    }
    Ok(total)
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

fn copy_directory(
    old_path: &Path,
    new_path: &Path,
    mut active: ActiveTask,
    buffer_size: usize,
) -> Option<ActiveTask> {
    try_or_abort!(
        active,
        fs::create_dir(new_path),
        format!("Failed to create directory {new_path:?}")
    );

    if let Ok(metadata) = fs::symlink_metadata(old_path) {
        apply_permissions(metadata.permissions().mode(), new_path);
    }

    let entries = try_or_abort!(
        active,
        fs::read_dir(old_path),
        format!("Failed to read directory {old_path:?}")
    );

    for entry in entries {
        if active.is_cancelled() {
            let _ = fs::remove_dir_all(new_path);
            active.cancelled();
            return None;
        }

        let entry = try_or_abort!(
            active,
            entry,
            format!("Failed to read entry in {old_path:?}")
        );

        let src = entry.path();
        let dst = new_path.join(entry.file_name());
        let metadata = try_or_abort!(
            active,
            fs::symlink_metadata(&src),
            format!("Failed to read metadata for {src:?}")
        );

        if metadata.is_symlink() {
            let target = try_or_abort!(
                active,
                fs::read_link(&src),
                format!("Failed to read symlink {src:?}")
            );
            try_or_abort!(
                active,
                std::os::unix::fs::symlink(&target, &dst),
                format!("Failed to create symlink {dst:?}")
            );
        } else {
            active = copy_path(
                &src,
                &dst,
                active,
                buffer_size,
                metadata.is_dir(),
                metadata.permissions().mode(),
            )?;
        }
    }

    Some(active)
}

/// Copies a file chunk-by-chunk, sending debounced progress updates via `active`.
///
/// Returns `Some(active)` on success, leaving finalization to the caller.
/// Returns `None` if an error occurred — the task has already been finalized via `active.error()`.
fn copy_file(
    old_path: &Path,
    new_path: &Path,
    mut active: ActiveTask,
    buffer_size: usize,
) -> Option<ActiveTask> {
    let total_size = active.total_size();
    match open_files(old_path, new_path) {
        Err(error) => {
            active.error(format!(
                "Failed to copy {old_path:?} to {new_path:?}: {error}"
            ));
            None
        }

        Ok((mut old_file, mut new_file)) => {
            let mut buffer = vec![0; buffer_size];
            let mut debouncer =
                debounce::BytesDebouncer::new(PROGRESS_DEBOUNCE_PERCENTAGE, total_size);

            loop {
                if active.is_cancelled() {
                    drop(old_file);
                    drop(new_file);
                    let _ = fs::remove_file(new_path);
                    active.cancelled();
                    break None;
                }

                match old_file.read(&mut buffer) {
                    Ok(0) => break Some(active),
                    Ok(bytes) => match new_file.write_all(&buffer[..bytes]) {
                        Ok(()) => {
                            active.increment(bytes as u64);
                            if debouncer.should_trigger(bytes as u64) {
                                active.send_progress();
                            }
                        }
                        Err(error) => {
                            active.error(format!("Failed to write {new_path:?}: {error}"));
                            break None;
                        }
                    },
                    Err(error) => {
                        active.error(format!("Failed to read {old_path:?}: {error}"));
                        break None;
                    }
                }
            }
        }
    }
}

/// Copies a directory or file, dispatching on `is_directory`. For files, the
/// source mode is applied to the destination on success; directories apply
/// their own permissions recursively in `copy_directory`.
fn copy_path(
    old_path: &Path,
    new_path: &Path,
    active: ActiveTask,
    buffer_size: usize,
    is_directory: bool,
    source_mode: u32,
) -> Option<ActiveTask> {
    if is_directory {
        copy_directory(old_path, new_path, active, buffer_size)
    } else {
        copy_file(old_path, new_path, active, buffer_size)
            .inspect(|_| apply_permissions(source_mode, new_path))
    }
}

fn remove_path(path: &Path, is_directory: bool) -> std::io::Result<()> {
    if is_directory {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn apply_permissions(mode: u32, path: &Path) {
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        warn!("Failed to set permissions on {path:?}: {e}");
    }
}

fn open_files(source: &Path, target: &Path) -> Result<(File, File)> {
    let source = File::open(source)?;
    let target = File::create(target)?;
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
    let old_path = PathBuf::from(&source.path);
    let new_path = PathBuf::from(&destination_directory.path).join(&source.basename);

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

    fn path_info(path: &str, basename: &str) -> PathInfo {
        // Private fields prevent literal construction; build from a real path
        // then overwrite the public fields used by validate_paths.
        let mut info = PathInfo::try_from(Path::new("/")).unwrap();
        info.path = path.to_string();
        info.basename = basename.to_string();
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
}
