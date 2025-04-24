use std::sync::mpsc::Sender;

use anyhow::{anyhow, Error};

use crate::{
    command::{result::CommandResult, Command},
    file_system::path_info::PathInfo,
};

use super::r#async::{run_copy_task, run_delete_task, run_move_task};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TaskCommand {
    Copy(PathInfo, PathInfo),
    DeletePath(PathInfo),
    Move(PathInfo, PathInfo),
}

impl TaskCommand {
    pub fn run(self, tx: Sender<Command>) -> CommandResult {
        match self {
            TaskCommand::Copy(path, dir) => run_copy_task(tx, path, dir),
            TaskCommand::DeletePath(path) => run_delete_task(tx, path),
            TaskCommand::Move(path, dir) => run_move_task(tx, path, dir),
        }
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
