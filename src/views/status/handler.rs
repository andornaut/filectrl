use crossterm::event::{KeyCode, KeyModifiers};

use super::StatusView;
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for StatusView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClipboardCopy(path) => self.set_clipboard_copy(path.clone()),
            Command::ClipboardCut(path) => self.set_clipboard_cut(path.clone()),
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::SetDirectory(directory, children) => {
                self.set_directory(directory.clone(), children)
            }
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('p'), KeyModifiers::NONE) => self.clear_progress(),
            (_, _) => CommandResult::NotHandled,
        }
    }
}
