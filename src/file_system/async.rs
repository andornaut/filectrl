use std::{
    fs::{self, File},
    io::{ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use anyhow::{anyhow, Result};

use super::path_info::PathInfo;
use crate::{
    command::{result::CommandResult, task::ActiveTask, Command},
    file_system::debounce,
};

const BUFFER_SIZE_DIVISOR: u64 = 20;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size

pub(super) fn run_copy_task(
    tx: Sender<Command>,
    path: PathInfo,
    dir: PathInfo,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> CommandResult {
    let (old_path, new_path) = match validate_paths(&path, &dir, "copy") {
        Ok(paths) => paths,
        Err(result) => return result,
    };

    let (active, initial) = ActiveTask::new(path.size, tx);
    let buffer_size = buffer_bytes(path.size, buffer_min_bytes, buffer_max_bytes);

    thread::spawn(move || {
        if let Some(active) = copy_file(&old_path, &new_path, active, buffer_size) {
            active.done();
        }
    });

    Command::Progress(initial).into()
}

pub(super) fn run_move_task(
    tx: Sender<Command>,
    path: PathInfo,
    dir: PathInfo,
    buffer_min_bytes: u64,
    buffer_max_bytes: u64,
) -> CommandResult {
    let (old_path, new_path) = match validate_paths(&path, &dir, "move") {
        Ok(paths) => paths,
        Err(result) => return result,
    };

    let (mut active, initial) = ActiveTask::new(path.size, tx);

    thread::spawn(move || match fs::rename(&old_path, &new_path) {
        Ok(_) => {
            active.increment(path.size);
            active.done();
        }
        Err(error) => match error.kind() {
            // If the file is on a different device/mount-point, we must copy-then-delete it instead
            ErrorKind::CrossesDevices => {
                let buffer_size = buffer_bytes(path.size, buffer_min_bytes, buffer_max_bytes);
                if let Some(active) = copy_file(&old_path, &new_path, active, buffer_size) {
                    match fs::remove_file(&old_path) {
                        Ok(_) => active.done(),
                        Err(error) => active.error(format!(
                            "Copy succeeded, but failed to delete original file {old_path:?}: {error}"
                        )),
                    }
                }
            }
            _ => active.error(format!(
                "Failed to move {old_path:?} to {new_path:?}: {error}"
            )),
        },
    });

    Command::Progress(initial).into()
}

pub(super) fn run_delete_task(tx: Sender<Command>, path: PathInfo) -> CommandResult {
    let (active, initial) = ActiveTask::new(path.size, tx);
    let path = PathBuf::from(&path.path);

    thread::spawn(move || {
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };

        match result {
            Ok(_) => active.done(),
            Err(error) => active.error(format!("Failed to delete {path:?}: {error}")),
        }
    });

    Command::Progress(initial).into()
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
