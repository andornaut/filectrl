use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{Handlers, clipboard::ClipboardEntry};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, PromptAction, handler::CommandHandler, result::CommandResult},
};

impl CommandHandler for Handlers {
    fn visit_command_handlers(&mut self, visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {
        visitor(&mut self.file_system);
        visitor(&mut self.root);
        #[cfg(debug_assertions)]
        visitor(&mut self.debug);
    }

    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetClipboardEntry(None) | Command::ResetView => {
                if let Err(error) = self.clipboard.clear() {
                    return Command::AlertError(format!(
                        "Failed to clear the clipboard: {error:#}"
                    ))
                    .into();
                }
                CommandResult::Handled
            }
            Command::OpenPrompt(kind) => {
                if matches!(kind, PromptAction::Delete(_)) {
                    // The derived SetClipboardEntry(None) re-enters the arm
                    // above, which performs the actual clear (and surfaces any
                    // error). Don't also clear inline here, or it would clear
                    // twice and swallow the first error.
                    return Command::SetClipboardEntry(None).into();
                }
                CommandResult::NotHandled
            }
            Command::Paste(dest) => match self.clipboard.get_clipboard_entry() {
                Ok(Some(ClipboardEntry::Copy(srcs))) => Command::Copy {
                    srcs,
                    dest: dest.clone(),
                }
                .into(),
                Ok(Some(ClipboardEntry::Move(srcs))) => Command::Move {
                    srcs,
                    dest: dest.clone(),
                }
                .into(),
                // Nothing to paste and no system clipboard to read: an entry
                // copied in another window would be unreachable here, so warn
                // rather than surprise the user with a silent no-op.
                Ok(None) if !self.clipboard.is_available() => {
                    Command::AlertWarn("Cannot paste: no system clipboard available".into()).into()
                }
                Ok(None) => CommandResult::Handled,
                Err(error) => {
                    Command::AlertWarn(format!("Failed to read the clipboard: {error:#}")).into()
                }
            },
            Command::SetClipboardEntry(Some(entry)) => {
                match self.clipboard.set_clipboard_entry(entry) {
                    Ok(()) => CommandResult::Handled,
                    Err(error) => {
                        Command::AlertError(format!("Failed to update the clipboard: {error:#}"))
                            .into()
                    }
                }
            }
            Command::GetClipboardText => {
                if let Some(text) = self.clipboard.get_text() {
                    Command::ClipboardText(text).into()
                } else {
                    CommandResult::Handled
                }
            }
            Command::SetClipboardText(text) => {
                self.clipboard.set_text(text);
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::CancelTask) => Command::CancelTask.into(),
            Some(Action::Quit) => Command::Quit.into(),
            Some(Action::ResetView) => Command::ResetView.into(),
            _ => CommandResult::NotHandled,
        }
    }
}
