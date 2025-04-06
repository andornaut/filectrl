use crossterm::event::{KeyCode, KeyModifiers};

use crate::command::{handler::CommandHandler, result::CommandResult, Command};

use super::{ClipboardOperation, NoticesView};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClipboardCopy(path) => {
                self.set_clipboard(path.clone(), ClipboardOperation::Copy)
            }
            Command::ClipboardCut(path) => {
                self.set_clipboard(path.clone(), ClipboardOperation::Cut)
            }
            Command::Copy(_, _) | Command::Move(_, _) => {
                self.clipboard = None;
                CommandResult::none()
            }
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('p'), KeyModifiers::NONE) => self.clear_progress(),
            (KeyCode::Char('c'), KeyModifiers::NONE) => self.clear_clipboard(),
            (_, _) => CommandResult::NotHandled,
        }
    }
}
