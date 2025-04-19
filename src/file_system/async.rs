use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use anyhow::{anyhow, Error, Result};
use log::info;

use super::path_info::PathInfo;
use crate::{
    command::{result::CommandResult, task::Task, Command},
    file_system::debounce,
};

const MAX_BUFFER_BYTES: u64 = 64_000_000;
const MIN_BUFFER_BYTES: u64 = 64_000;
const PROGRESS_DEBOUNCE_PERCENTAGE: u64 = 1; // 1% of total size

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum TaskCommand {
    Copy(PathInfo, PathInfo),
    DeletePath(PathInfo),
    Move(PathInfo, PathInfo),
}

impl TaskCommand {
    pub(super) fn run(self, tx: Sender<Command>) -> CommandResult {
        match self {
            TaskCommand::Copy(path, dir) => Self::run_copy_task(tx, path, dir),
            TaskCommand::DeletePath(path) => Self::run_delete_task(tx, path),
            TaskCommand::Move(path, dir) => Self::run_move_task(tx, path, dir),
        }
    }

    fn run_copy_task(tx: Sender<Command>, path: PathInfo, dir: PathInfo) -> CommandResult {
        let old_path = PathBuf::from(&path.path);
        let new_path = Path::new(&dir.path).join(&path.basename);
        if old_path == new_path {
            return anyhow!("Cannot copy {old_path:?} to {new_path:?}: Source and destination paths must be different").into();
        }
        let mut task = Task::new(path.size);
        let original_task = task.clone();
        let buffer_size = buffer_bytes(path.size);

        thread::spawn(move || {
            copy_file(&old_path, &new_path, &mut task, &tx, buffer_size, path.size)
        });

        Command::Progress(original_task).into()
    }

    fn run_move_task(tx: Sender<Command>, path: PathInfo, dir: PathInfo) -> CommandResult {
        let old_path = PathBuf::from(&path.path);
        let new_path = Path::new(&dir.path).join(&path.basename);
        if old_path == new_path {
            return anyhow!("Cannot move {old_path:?} to {new_path:?}: Source and destination paths must be different").into();
        }
        let mut task = Task::new(path.size);
        let original_task = task.clone();
        let buffer_size = buffer_bytes(path.size);

        thread::spawn(move || match fs::rename(&old_path, &new_path) {
            Ok(_) => {
                task.increment(path.size);
                task.done();
                tx.send(Command::Progress(task)).expect("Can send command");
            }
            Err(error) => match error.kind() {
                // If the file is on a different device/mount-point, we must copy-then-delete it instead
                ErrorKind::CrossesDevices => {
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

        Command::Progress(original_task).into()
    }

    fn run_delete_task(tx: Sender<Command>, path: PathInfo) -> CommandResult {
        let path_buf = PathBuf::from(&path.path);
        let mut task = Task::new(path.size);
        let original_task = task.clone();

        thread::spawn(move || match fs::remove_file(&path_buf) {
            Ok(_) => {
                task.done();
                tx.send(Command::Progress(task)).expect("Can send command");
            }
            Err(error) => {
                task.error(format!("Failed to delete {path_buf:?}: {error}"));
                tx.send(Command::Progress(task)).expect("Can send command");
            }
        });

        Command::Progress(original_task).into()
    }
}

impl TryFrom<&Command> for TaskCommand {
    type Error = Error;

    fn try_from(value: &Command) -> Result<Self, Self::Error> {
        match value {
            Command::Copy(path, dir) => Ok(Self::Copy(path.clone(), dir.clone())),
            Command::Move(path, dir) => Ok(Self::Move(path.clone(), dir.clone())),
            Command::DeletePath(path) => Ok(Self::DeletePath(path.clone())),
            _ => Err(anyhow!("Cannot convert Command:{value:?} to TaskCommand")),
        }
    }
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
        std::cmp::max(MIN_BUFFER_BYTES, len / 20) as usize
    }
}

fn copy_file(
    old_path: &PathBuf,
    new_path: &PathBuf,
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
            let mut reader = BufReader::new(old_file);
            let mut writer = BufWriter::new(new_file);
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

fn open_files(source: &PathBuf, target: &PathBuf) -> Result<(File, File)> {
    let source = File::open(source)?;
    let target = File::create(target)?;
    Ok((source, target))
}
