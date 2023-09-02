use crossterm::event::{KeyCode, KeyModifiers};

use crate::command::{handler::CommandHandler, result::CommandResult, Command};

use super::FileSystem;

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Copy(from, to) => self.copy(from, to),
            Command::DeletePath(path) => self.delete(path),
            Command::Move(from, to) => self.mv(from, to),
            Command::Open(path) => self.open(path),
            Command::OpenCustom(path) => self.open_custom(path),
            Command::RenamePath(old_path, new_basename) => self.rename(old_path, new_basename),
            _ => CommandResult::NotHandled,
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
