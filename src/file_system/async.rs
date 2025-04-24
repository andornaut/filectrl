use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use anyhow::{anyhow, Result};
use log::info;

use super::path_info::PathInfo;
use crate::{
    command::{result::CommandResult, task::Task, Command},
    file_system::debounce,
};

const BUFFER_SIZE_DIVISOR: u64 = 20;
const MAX_BUFFER_BYTES: u64 = 64_000_000;
const MIN_BUFFER_BYTES: u64 = 64_000;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size

pub(super) fn run_copy_task(tx: Sender<Command>, path: PathInfo, dir: PathInfo) -> CommandResult {
    let (old_path, new_path) = match validate_paths(&path, &dir, "copy") {
        Ok(paths) => paths,
        Err(result) => return result,
    };

    let mut task = Task::new(path.size);
    let task_clone = task.clone();
    let buffer_size = buffer_bytes(path.size);

    thread::spawn(move || copy_file(&old_path, &new_path, &mut task, &tx, buffer_size, path.size));

    Command::Progress(task_clone).into()
}

pub(super) fn run_move_task(tx: Sender<Command>, path: PathInfo, dir: PathInfo) -> CommandResult {
    let (old_path, new_path) = match validate_paths(&path, &dir, "move") {
        Ok(paths) => paths,
        Err(result) => return result,
    };

    let mut task = Task::new(path.size);
    let task_clone = task.clone();

    thread::spawn(move || match fs::rename(&old_path, &new_path) {
        Ok(_) => {
            task.increment(path.size);
            task.done();
            tx.send(Command::Progress(task)).expect("Can send command");
        }
        Err(error) => match error.kind() {
            // If the file is on a different device/mount-point, we must copy-then-delete it instead
            ErrorKind::CrossesDevices => {
                let buffer_size = buffer_bytes(path.size);
                if copy_file(&old_path, &new_path, &mut task, &tx, buffer_size, path.size) {
                    if let Err(error) = fs::remove_file(&old_path) {
                        task.error(format!(
                            "Copy succeeded, but failed to delete original file {old_path:?}: {error}"
                        ));
                        tx.send(Command::Progress(task)).expect("Can send command");
                    } else {
                        task.done();
                        tx.send(Command::Progress(task)).expect("Can send command");
                    }
                }
            }
            _ => {
                task.error(format!(
                    "Failed to move {old_path:?} to {new_path:?}: {error}"
                ));
                tx.send(Command::Progress(task)).expect("Can send command");
            }
        },
    });

    Command::Progress(task_clone).into()
}

pub(super) fn run_delete_task(tx: Sender<Command>, path: PathInfo) -> CommandResult {
    let mut task = Task::new(path.size);
    let task_clone = task.clone();
    let path = PathBuf::from(&path.path);

    thread::spawn(move || {
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };

        match result {
            Ok(_) => {
                task.done();
                tx.send(Command::Progress(task)).expect("Can send command");
            }
            Err(error) => {
                task.error(format!("Failed to delete {path:?}: {error}"));
                tx.send(Command::Progress(task)).expect("Can send command");
            }
        }
    });

    Command::Progress(task_clone).into()
}

fn buffer_bytes(len: u64) -> usize {
    // 1) For files ≤ 64KB:
    //    Use len of the file as the buffer size
    // 2) For files ≥ 1.28GB:
    //    Use 64MB buffer
    // 3) For files > 64KB and < 1.28GB:
    //    Use the maximum of 64KB or len / 20
    //    This ensures we never go below 64KB
    //    Example: 1MB file → 64KB buffer (since 1MB/20 = 50KB < 64KB)
    //    Example: 100MB file → 5MB buffer (since 100MB/20 = 5MB > 64KB)
    //    Example: 1.27GB file → 63.5MB buffer (since 1.27GB/20 = 63.5MB > 64KB)
    if len <= MIN_BUFFER_BYTES {
        len as usize
    } else if len >= (MAX_BUFFER_BYTES * 20) {
        MAX_BUFFER_BYTES as usize
    } else {
        std::cmp::max(MIN_BUFFER_BYTES, len / BUFFER_SIZE_DIVISOR) as usize
    }
}

fn copy_file(
    old_path: &Path,
    new_path: &Path,
    task: &mut Task,
    tx: &Sender<Command>,
    buffer_size: usize,
    total_size: u64,
) -> bool {
    match open_files(old_path, new_path) {
        Err(error) => {
            task.error(format!(
                "Failed to copy {old_path:?} to {new_path:?}: {error}"
            ));
            false
        }

        Ok((old_file, new_file)) => {
            let mut buffer = vec![0; buffer_size];
            // Use with_capacity to specify optimal buffer sizes for reader and writer
            let mut reader = BufReader::with_capacity(buffer_size, old_file);
            let mut writer = BufWriter::with_capacity(buffer_size, new_file);
            let mut debouncer =
                debounce::BytesDebouncer::new(PROGRESS_DEBOUNCE_PERCENTAGE, total_size);

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        task.done();
                        tx.send(Command::Progress(task.clone()))
                            .expect("Can send command");
                        break true;
                    }
                    Ok(bytes) => match writer.write_all(&buffer[..bytes]) {
                        Ok(()) => {
                            task.increment(bytes as u64);

                            if debouncer.should_trigger(bytes as u64) {
                                info!("Sending progress command: {:?}", task);
                                tx.send(Command::Progress(task.clone()))
                                    .expect("Can send command");
                            }
                        }
                        Err(error) => {
                            task.error(format!("Failed to write {new_path:?}: {error}"));
                            break false;
                        }
                    },
                    Err(error) => {
                        task.error(format!("Failed to read {old_path:?}: {error}"));
                        break false;
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
