use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{App, clipboard::ClipboardEntry};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, PromptAction, handler::CommandHandler, result::CommandResult},
};

impl CommandHandler for App {
    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        visitor(&mut self.file_system);
        visitor(&mut self.root);
        #[cfg(debug_assertions)]
        visitor(&mut self.debug);
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::ClearClipboard | Command::Reset => {
                if let Err(e) = self.clipboard.clear() {
                    return Command::AlertError(format!("Failed to clear clipboard: {e}")).into();
                }
                CommandResult::Handled
            }
            Command::OpenPrompt(kind) => {
                if matches!(kind, PromptAction::Delete(_)) {
                    let _ = self.clipboard.clear();
                    return Command::ClearClipboard.into();
                }
                CommandResult::Handled
            }
            Command::Paste(dest) => match self.clipboard.get_clipboard_entry() {
                Some(ClipboardEntry::Copy(srcs)) => Command::Copy {
                    srcs,
                    dest: dest.clone(),
                }
                .into(),
                Some(ClipboardEntry::Move(srcs)) => Command::Move {
                    srcs,
                    dest: dest.clone(),
                }
                .into(),
                None => CommandResult::Handled,
            },
            Command::SetClipboard(entry) => match self.clipboard.set_clipboard_entry(entry) {
                Ok(()) => CommandResult::Handled,
                Err(e) => Command::AlertError(format!("Failed to update clipboard: {e}")).into(),
            },
            Command::ReadFromClipboard => {
                if let Some(text) = self.clipboard.get_text() {
                    Command::TextFromClipboard(text).into()
                } else {
                    CommandResult::Handled
                }
            }
            Command::WriteToClipboard(text) => {
                self.clipboard.set_text(text);
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        // Hardcoded keys
        if let (KeyCode::Esc, KeyModifiers::NONE) = (*code, *modifiers) {
            return Command::Reset.into();
        }
        // Rebindable keys
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::Quit) => Command::Quit.into(),
            Some(Action::Reset) => Command::Reset.into(),
            _ => CommandResult::NotHandled,
        }
    }
}
