use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{r#async::TaskCommand, FileSystem};
use crate::command::{handler::CommandHandler, result::CommandResult, task::Task, Command};

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match TaskCommand::try_from(command) {
            Ok(task) => task.run(self.tx.clone()),
            Err(_) => match command {
                Command::Open(path) => self.open(path),
                Command::OpenCustom(path) => self.open_custom(path),
                Command::Progress(task) => self.handle_error_and_done_status(task),
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

impl FileSystem {
    fn handle_error_and_done_status(&mut self, task: &Task) -> CommandResult {
        if let Some(message) = task.error_message() {
            return Command::AddError(message).into();
        }
        if task.is_done() {
            return self.refresh();
        }
        CommandResult::NotHandled
    }
}
