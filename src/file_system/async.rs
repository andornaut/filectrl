use anyhow::{anyhow, Error, Result};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, ErrorKind, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
};

use super::path_info::PathInfo;
use crate::command::{result::CommandResult, task::Task, Command};

const MAX_BUFFER_BYTES: u64 = 64_000_000;
const MIN_DYNAMIC_BUFFER_BYTES: u64 = 64_000;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum TaskCommand {
    Copy(PathInfo, PathInfo),
    DeletePath(PathInfo),
    Move(PathInfo, PathInfo),
}

impl TaskCommand {
    pub(super) fn run(self, tx: Option<Sender<Command>>) -> CommandResult {
        let tx = tx.expect("Sender is set");
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

        thread::spawn(move || copy_file(&old_path, &new_path, &mut task, &tx, buffer_size));

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
                    if copy_file(&old_path, &new_path, &mut task, &tx, buffer_size) {
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
        let path = PathBuf::from(&path.path);
        let mut task = Task::new(1);
        let original_task = task.clone();

        thread::spawn(move || match fs::remove_file(&path) {
            Ok(_) => {
                task.done();
                tx.send(Command::Progress(task)).expect("Can send command");
            }
            Err(error) => {
                task.error(format!("Failed to delete {path:?}: {error}"));
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
    let bytes = if len <= MIN_DYNAMIC_BUFFER_BYTES {
        len
    } else if len >= (MAX_BUFFER_BYTES * 10) {
        MAX_BUFFER_BYTES
    } else {
        std::cmp::max(MIN_DYNAMIC_BUFFER_BYTES, len / 10)
    };
    bytes as usize
}

fn copy_file(
    old_path: &PathBuf,
    new_path: &PathBuf,
    task: &mut Task,
    tx: &Sender<Command>,
    buffer_size: usize,
) -> bool {
    match open_files(old_path, new_path) {
        Err(error) => {
            task.error(format!(
                "Failed to copy {old_path:?} to {new_path:?}: {error}"
            ));
            tx.send(Command::Progress(task.clone()))
                .expect("Can send command");
            false
        }
        Ok((old_file, new_file)) => {
            let mut buffer = vec![0; buffer_size];
            let mut reader = BufReader::new(old_file);
            let mut writer = BufWriter::new(new_file);
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => return true,
                    Ok(bytes) => match writer.write_all(&buffer[..bytes]) {
                        Ok(()) => task.increment(bytes as u64),
                        Err(error) => {
                            task.error(format!("Failed to write {new_path:?}: {error}"));
                            tx.send(Command::Progress(task.clone()))
                                .expect("Can send command");
                            return false;
                        }
                    },
                    Err(error) => {
                        task.error(format!("Failed to read {old_path:?}: {error}"));
                        tx.send(Command::Progress(task.clone()))
                            .expect("Can send command");
                        return false;
                    }
                }
            }
        }
    }
}

fn open_files(source: &PathBuf, target: &PathBuf) -> Result<(File, File)> {
    let source = File::open(&source)?;
    let target = File::create(&target)?;
    Ok((source, target))
}
