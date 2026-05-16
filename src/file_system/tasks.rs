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
    command::{Command, progress::{ActiveTask, CancellationToken}, result::CommandResult},
    file_system::debounce,
};

const BUFFER_SIZE_DIVISOR: u64 = 20;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size

pub struct TaskRunResult {
    pub command_result: CommandResult,
    pub cancel_info: Option<(usize, CancellationToken, String)>,
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
        Err(result) => {
            return TaskRunResult {
                command_result: result,
                cancel_info: None,
            };
        }
    };

    info!("Copying {old_path:?} to {new_path:?}");
    let description = format!("copy of {}", path.basename);

    if path.is_directory() {
        let total_size = match dir_total_size(&old_path) {
            Ok(size) => size,
            Err(error) => {
                return TaskRunResult {
                    command_result: Command::AlertError(format!(
                        "Failed to read directory {old_path:?}: {error}"
                    ))
                    .into(),
                    cancel_info: None,
                };
            }
        };
        let (active, initial, token) = ActiveTask::new(tx, total_size);
        let cancel_info = Some((initial.id(), token, description));
        let buffer_size = buffer_bytes(total_size, buffer_min_bytes, buffer_max_bytes);

        thread::spawn(move || {
            if let Some(active) = copy_directory(&old_path, &new_path, active, buffer_size) {
                active.done();
            }
        });

        TaskRunResult {
            command_result: Command::Progress(initial).into(),
            cancel_info,
        }
    } else {
        let (active, initial, token) = ActiveTask::new(tx, path.size);
        let cancel_info = Some((initial.id(), token, description));
        let buffer_size = buffer_bytes(path.size, buffer_min_bytes, buffer_max_bytes);
        let source_mode = path.mode();

        thread::spawn(move || {
            if let Some(active) = copy_file(&old_path, &new_path, active, buffer_size) {
                apply_permissions(source_mode, &new_path);
                active.done();
            }
        });

        TaskRunResult {
            command_result: Command::Progress(initial).into(),
            cancel_info,
        }
    }
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
        Err(result) => {
            return TaskRunResult {
                command_result: result,
                cancel_info: None,
            };
        }
    };

    info!("Moving {old_path:?} to {new_path:?}");
    let description = format!("move of {}", path.basename);
    let (mut active, initial, token) = ActiveTask::new(tx, path.size);
    let cancel_info = Some((initial.id(), token, description));
    let source_mode = path.mode();

    thread::spawn(move || match fs::rename(&old_path, &new_path) {
        Ok(_) => {
            active.increment(path.size);
            active.done();
        }
        Err(error) => match error.kind() {
            // If the file is on a different device/mount-point, we must copy-then-delete it instead
            ErrorKind::CrossesDevices => {
                let is_directory = path.is_directory();
                let buffer_size = buffer_bytes(path.size, buffer_min_bytes, buffer_max_bytes);
                let copy_result = if is_directory {
                    copy_directory(&old_path, &new_path, active, buffer_size)
                } else {
                    copy_file(&old_path, &new_path, active, buffer_size)
                };
                if let Some(active) = copy_result {
                    let remove_result = if is_directory {
                        fs::remove_dir_all(&old_path)
                    } else {
                        fs::remove_file(&old_path)
                    };
                    match remove_result {
                        Ok(_) => {
                            if !is_directory {
                                apply_permissions(source_mode, &new_path);
                            }
                            active.done()
                        }
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

    TaskRunResult {
        command_result: Command::Progress(initial).into(),
        cancel_info,
    }
}

fn run_delete_task(tx: Sender<Command>, path: PathInfo) -> TaskRunResult {
    let description = format!("delete of {}", path.basename);
    let (active, initial, token) = ActiveTask::new(tx, path.size);
    let cancel_info = Some((initial.id(), token, description));
    // Use PathInfo::is_directory() (lstat-based, does not follow symlinks) so that a
    // symlink to a directory is removed with remove_file(), not remove_dir_all().
    // PathBuf::is_dir() follows symlinks and would incorrectly recurse into the target.
    let is_directory = path.is_directory();
    let path = PathBuf::from(&path.path);
    info!("Deleting {path:?}");

    thread::spawn(move || {
        let result = if is_directory {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };

        match result {
            Ok(_) => active.done(),
            Err(error) => active.error(format!("Failed to delete {path:?}: {error}")),
        }
    });

    TaskRunResult {
        command_result: Command::Progress(initial).into(),
        cancel_info,
    }
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

fn copy_directory(
    old_path: &Path,
    new_path: &Path,
    mut active: ActiveTask,
    buffer_size: usize,
) -> Option<ActiveTask> {
    if let Err(error) = fs::create_dir(new_path) {
        active.error(format!("Failed to create directory {new_path:?}: {error}"));
        return None;
    }

    if let Ok(metadata) = fs::symlink_metadata(old_path) {
        apply_permissions(metadata.permissions().mode(), new_path);
    }

    let entries = match fs::read_dir(old_path) {
        Ok(entries) => entries,
        Err(error) => {
            active.error(format!("Failed to read directory {old_path:?}: {error}"));
            return None;
        }
    };

    for entry in entries {
        if active.is_cancelled() {
            let _ = fs::remove_dir_all(new_path);
            active.error("Cancelled".to_string());
            return None;
        }

        let entry = match entry {
            Ok(e) => e,
            Err(error) => {
                active.error(format!("Failed to read entry in {old_path:?}: {error}"));
                return None;
            }
        };

        let src = entry.path();
        let dst = new_path.join(entry.file_name());
        let metadata = match fs::symlink_metadata(&src) {
            Ok(m) => m,
            Err(error) => {
                active.error(format!("Failed to read metadata for {src:?}: {error}"));
                return None;
            }
        };

        if metadata.is_symlink() {
            match fs::read_link(&src) {
                Ok(target) => {
                    if let Err(error) = std::os::unix::fs::symlink(&target, &dst) {
                        active.error(format!("Failed to create symlink {dst:?}: {error}"));
                        return None;
                    }
                }
                Err(error) => {
                    active.error(format!("Failed to read symlink {src:?}: {error}"));
                    return None;
                }
            }
        } else if metadata.is_dir() {
            active = copy_directory(&src, &dst, active, buffer_size)?;
        } else {
            let mode = metadata.permissions().mode();
            active = copy_file(&src, &dst, active, buffer_size)?;
            apply_permissions(mode, &dst);
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
                    active.error("Cancelled".to_string());
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

    Ok((old_path, new_path))
}
