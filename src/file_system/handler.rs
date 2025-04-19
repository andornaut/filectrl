use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{r#async::TaskCommand, FileSystem};
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match TaskCommand::try_from(command) {
            Ok(task) => self.run_task(task),
            Err(_) => match command {
                Command::Open(path) => self.open(path),
                Command::OpenCustom(path) => self.open_custom(path),
                Command::Progress(task) => self.check_progress_for_error(task),
                Command::Refresh => self.refresh(),
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
