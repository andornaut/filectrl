use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::Handlers;
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
                Ok(Some((entry, true))) => entry.into_paste(dest.clone()).into(),
                Ok(Some((entry, false))) => Command::OpenPrompt(PromptAction::ConfirmPaste {
                    entry,
                    dest: dest.clone(),
                })
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

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::CancelTask) => Command::CancelTask.into(),
            Some(Action::Quit) => Command::Quit.into(),
            Some(Action::ResetView) => Command::ResetView.into(),
            _ => CommandResult::NotHandled,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use test_case::test_case;

    use super::*;
    use crate::app::{
        claims::{Fixture, test_handlers},
        clipboard::ClipboardEntry,
    };

    fn handlers(fixture: &Fixture) -> Handlers {
        let (tx, _rx) = mpsc::channel();
        test_handlers(tx, fixture)
    }

    #[test]
    fn a_paste_becomes_the_operation_the_clipboard_entry_names() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);
        let (srcs, dest) = (vec![fixture.file()], fixture.directory());

        handlers.handle_command(&Command::SetClipboardEntry(Some(ClipboardEntry::Copy(
            srcs.clone(),
        ))));
        assert_eq!(
            CommandResult::from(Command::Copy {
                srcs: srcs.clone(),
                dest: dest.clone()
            }),
            handlers.handle_command(&Command::Paste(dest.clone()))
        );

        handlers.handle_command(&Command::SetClipboardEntry(Some(ClipboardEntry::Move(
            srcs.clone(),
        ))));
        assert_eq!(
            CommandResult::from(Command::Move {
                srcs,
                dest: dest.clone()
            }),
            handlers.handle_command(&Command::Paste(dest))
        );
    }

    /// Text any program could have written, shaped like an entry: it is
    /// confirmed rather than carried out.
    #[test]
    fn a_paste_of_an_entry_this_window_did_not_write_asks_first() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);
        let (file, dest) = (fixture.file(), fixture.directory());
        let text = format!("mv {}", shell_words::quote(&file.path.to_string_lossy()));
        handlers.handle_command(&Command::SetClipboardText(text));

        assert_eq!(
            CommandResult::from(Command::OpenPrompt(PromptAction::ConfirmPaste {
                entry: ClipboardEntry::Move(vec![file]),
                dest: dest.clone(),
            })),
            handlers.handle_command(&Command::Paste(dest))
        );
    }

    #[test]
    fn a_paste_with_nothing_to_paste_and_no_system_clipboard_warns() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);

        assert_eq!(
            CommandResult::from(Command::AlertWarn(
                "Cannot paste: no system clipboard available".into()
            )),
            handlers.handle_command(&Command::Paste(fixture.directory()))
        );
    }

    #[test_case(&Command::ResetView ; "resetting the view")]
    #[test_case(&Command::SetClipboardEntry(None) ; "clearing the entry")]
    fn the_clipboard_entry_is_cleared_by(clear: &Command) {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);
        handlers.handle_command(&Command::SetClipboardEntry(Some(ClipboardEntry::Copy(
            vec![fixture.file()],
        ))));

        assert_eq!(CommandResult::Handled, handlers.handle_command(clear));

        // Nothing is left to paste, so the paste warns instead of copying.
        assert!(matches!(
            Command::try_from(handlers.handle_command(&Command::Paste(fixture.directory()))),
            Ok(Command::AlertWarn(_))
        ));
    }

    #[test]
    fn only_a_delete_prompt_clears_the_clipboard() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);

        assert_eq!(
            CommandResult::from(Command::SetClipboardEntry(None)),
            handlers.handle_command(&Command::OpenPrompt(PromptAction::Delete(1)))
        );
        assert_eq!(
            CommandResult::NotHandled,
            handlers.handle_command(&Command::OpenPrompt(PromptAction::CreateDirectory))
        );
    }

    #[test]
    fn clipboard_text_is_read_back_as_written() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);

        handlers.handle_command(&Command::SetClipboardText("text".into()));

        assert_eq!(
            CommandResult::from(Command::ClipboardText("text".into())),
            handlers.handle_command(&Command::GetClipboardText)
        );
    }

    #[test_case(KeyCode::Char('q'), KeyModifiers::NONE => CommandResult::from(Command::Quit) ; "quit")]
    #[test_case(KeyCode::Char('K'), KeyModifiers::SHIFT => CommandResult::from(Command::CancelTask) ; "cancel task")]
    #[test_case(KeyCode::Esc, KeyModifiers::NONE => CommandResult::from(Command::ResetView) ; "reset view")]
    #[test_case(KeyCode::Char('j'), KeyModifiers::NONE => CommandResult::NotHandled ; "an action another handler owns")]
    fn a_global_key_becomes_its_command(code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let fixture = Fixture::new();
        handlers(&fixture).handle_key(code, modifiers)
    }
}
