use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::command::{Command, handler::CommandHandler, result::CommandResult};

/// Debug key bindings — only compiled in non-release builds.
///
/// Bare numbers only — Alt/Option is not portable on macOS.
#[derive(Default)]
pub(super) struct DebugHandler;

fn debug_command(ch: char) -> Option<Command> {
    Some(match ch {
        '1' => Command::AlertInfo("Debug: info alert".into()),
        '2' => Command::AlertWarn("Debug: warn alert".into()),
        '3' => Command::AlertError("Debug: error alert".into()),
        // Exercises alert text truncation.
        '4' => Command::AlertError(
            "Debug: long error alert — \
             Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
             sed do eiusmod tempor incididunt ut labore et dolore magna aliqua"
                .into(),
        ),
        // Exercises display-width handling.
        '5' => Command::AlertInfo("Debug: unicode alert — こんにちは 🦀 café naïve 北京".into()),
        '6' => Command::AlertWarn("No file selected".into()),
        '7' => Command::AlertError("Permission denied: /etc/hosts".into()),
        '8' => Command::AlertError(
            "Failed to rename \"foo.txt\" to \"bar.txt\": file already exists".into(),
        ),
        '9' => Command::RefreshDirectory,
        _ => return None,
    })
}

impl CommandHandler for DebugHandler {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        if *modifiers != KeyModifiers::NONE {
            return CommandResult::NotHandled;
        }
        match code {
            KeyCode::Char(ch) => match debug_command(*ch) {
                Some(command) => command.into(),
                None => CommandResult::NotHandled,
            },
            _ => CommandResult::NotHandled,
        }
    }
}
