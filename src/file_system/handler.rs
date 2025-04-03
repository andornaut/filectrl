use anyhow::{anyhow, Error};
use crossterm::event::{KeyCode, KeyModifiers};

use super::{path_info::PathInfo, FileSystem};
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum TaskCommand {
    Copy(PathInfo, PathInfo),
}

impl TryFrom<&Command> for TaskCommand {
    type Error = Error;

    fn try_from(value: &Command) -> Result<Self, Self::Error> {
        match value {
            Command::Copy(path, dir) => Ok(Self::Copy(path.clone(), dir.clone())),
            _ => Err(anyhow!("Cannot convert Command:{value:?} to TaskCommand")),
        }
    }
}

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match TaskCommand::try_from(command) {
            Ok(operation) => self.run_task(operation),
            Err(_) => match command {
                Command::DeletePath(path) => self.delete(path),
                Command::Move(from, to) => self.mv(from, to),
                Command::Open(path) => self.open(path),
                Command::OpenCustom(path) => self.open_custom(path),
                Command::Progress(task) => self.failed_task_to_error(task),
                Command::RenamePath(old_path, new_basename) => self.rename(old_path, new_basename),
                _ => CommandResult::NotHandled,
            },
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::F(5), KeyModifiers::NONE) => {
                self.refresh()
            }
            (code, KeyModifiers::NONE) => match code {
                KeyCode::Backspace | KeyCode::Left | KeyCode::Char('b') | KeyCode::Char('h') => {
                    self.back()
                }
                KeyCode::Char('w') => self.open_new_window(),
                KeyCode::Char('t') => self.open_current_directory(),
                _ => CommandResult::NotHandled,
            },
            (_, _) => CommandResult::NotHandled,
        }
    }
}
