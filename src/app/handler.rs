use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{App, clipboard::ClipboardEntry};
use crate::{
    app::config::keybindings::Action,
    command::{Command, PromptKind, handler::CommandHandler, mode::InputMode, result::CommandResult},
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
                self.state.clipboard_entry = None;
                CommandResult::Handled
            }
            Command::ClosePrompt | Command::ConfirmDelete | Command::RenamePath(_, _) | Command::SetFilter(_) => {
                self.state.mode = InputMode::Normal;
                CommandResult::Handled
            }
            Command::OpenPrompt(kind, _) => {
                self.state.mode = InputMode::Prompt;
                if *kind == PromptKind::Delete {
                    let _ = self.clipboard.clear();
                    self.state.clipboard_entry = None;
                }
                CommandResult::Handled
            }
            // Intent: TableView emits Paste(dest); App enriches with AppState::clipboard_entry
            Command::Paste(dest) => match &self.state.clipboard_entry {
                Some(ClipboardEntry::Copy(srcs)) => Command::Copy { srcs: srcs.clone(), dest: dest.clone() }.into(),
                Some(ClipboardEntry::Move(srcs)) => Command::Move { srcs: srcs.clone(), dest: dest.clone() }.into(),
                None => CommandResult::Handled,
            },
            Command::SetClipboard(entry) => {
                match self.clipboard.set_clipboard_entry(entry) {
                    Ok(()) => {
                        self.state.clipboard_entry = Some(entry.clone());
                        CommandResult::Handled
                    }
                    Err(e) => Command::AlertError(format!("Failed to update clipboard: {e}")).into(),
                }
            }
            Command::SetMarkCount(count) => {
                // Marks and clipboard are mutually exclusive
                if *count > 0 {
                    let _ = self.clipboard.clear();
                    self.state.clipboard_entry = None;
                }
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
        match self.config.keybindings.normal_action(code, modifiers) {
            Some(Action::Quit) => Command::Quit.into(),
            Some(Action::Reset) => Command::Reset.into(),
            _ => CommandResult::NotHandled,
        }
    }
}
