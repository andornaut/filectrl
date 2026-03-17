use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::command::{Command, handler::CommandHandler, result::CommandResult};

/// Debug key bindings — only compiled in non-release builds.
///
/// Keybindings (bare numbers — Alt/Option is not portable on macOS):
///   1 → AlertInfo
///   2 → AlertWarn
///   3 → AlertError
///   4 → AlertError with a long message (exercises alert text truncation)
///   5 → AlertInfo with unicode/emoji (exercises display-width handling)
///   6 → AlertWarn simulating "no file selected" (real user-facing warning path)
///   7 → AlertError simulating permission denied
///   8 → AlertError simulating rename collision (file already exists)
///   9 → Command::Refresh (exercises watcher/refresh path)
#[derive(Default)]
pub(super) struct DebugHandler;

impl CommandHandler for DebugHandler {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                Command::AlertInfo("Debug: info alert".into()).into()
            }
            (KeyCode::Char('2'), KeyModifiers::NONE) => {
                Command::AlertWarn("Debug: warn alert".into()).into()
            }
            (KeyCode::Char('3'), KeyModifiers::NONE) => {
                Command::AlertError("Debug: error alert".into()).into()
            }
            (KeyCode::Char('4'), KeyModifiers::NONE) => {
                Command::AlertError(
                    "Debug: long error alert — \
                     Lorem ipsum dolor sit amet, consectetur adipiscing elit, \
                     sed do eiusmod tempor incididunt ut labore et dolore magna aliqua"
                        .into(),
                )
                .into()
            }
            (KeyCode::Char('5'), KeyModifiers::NONE) => {
                Command::AlertInfo("Debug: unicode alert — こんにちは 🦀 café naïve 北京".into())
                    .into()
            }
            (KeyCode::Char('6'), KeyModifiers::NONE) => {
                Command::AlertWarn("No file selected".into()).into()
            }
            (KeyCode::Char('7'), KeyModifiers::NONE) => {
                Command::AlertError("Permission denied: /etc/hosts".into()).into()
            }
            (KeyCode::Char('8'), KeyModifiers::NONE) => {
                Command::AlertError(
                    "Failed to rename \"foo.txt\" to \"bar.txt\": file already exists".into(),
                )
                .into()
            }
            (KeyCode::Char('9'), KeyModifiers::NONE) => Command::Refresh.into(),
            _ => CommandResult::NotHandled,
        }
    }
}
