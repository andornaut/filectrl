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
            // A confirmed delete clears the clipboard, which may name the
            // entries it removes; declining the prompt leaves it alone. The
            // derived SetClipboardEntry(None) re-enters the arm above, which
            // performs the actual clear (and surfaces any error), so this arm
            // does not also clear inline.
            Command::ConfirmDelete => CommandResult::from(Command::SetClipboardEntry(None)),
            Command::Paste(dest) => match self.clipboard.get_clipboard_entry() {
                Ok(Some((entry, true))) => entry.into_paste(dest.clone()).into(),
                Ok(Some((entry, false))) => Command::OpenPrompt(PromptAction::ConfirmPaste {
                    entry,
                    dest: dest.clone(),
                })
                .into(),
                Ok(None) => nothing_to_paste(self.clipboard.is_available()).into(),
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
            // Quitting ends the worker, and with it any file operation part
            // way through, so that is confirmed first. A signal still quits
            // at once: it does not come through here.
            Some(Action::Quit) => match self.file_system.task_count() {
                0 => Command::Quit.into(),
                tasks => Command::OpenPrompt(PromptAction::ConfirmQuit(tasks)).into(),
            },
            // Closing help is all the key does there, which RootView handles:
            // a reset would also drop the marks, filter and clipboard entry
            // the help screen was covering.
            Some(Action::ResetView) if self.root.is_help_visible() => CommandResult::NotHandled,
            Some(Action::ResetView) => Command::ResetView.into(),
            _ => CommandResult::NotHandled,
        }
    }
}

/// The warning for a paste that found no entry: the clipboard is empty or
/// holds text another program put there. Without a system clipboard, an entry
/// copied in another window is unreachable, which is the more useful thing to
/// say.
fn nothing_to_paste(system_clipboard: bool) -> Command {
    Command::AlertWarn(
        if system_clipboard {
            "Cannot paste: nothing has been copied or cut"
        } else {
            "Cannot paste: no system clipboard available"
        }
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use test_case::test_case;

    use super::*;
    use crate::{
        app::{
            claims::{Fixture, test_handlers},
            clipboard::ClipboardEntry,
        },
        file_system::path_info::PathInfo,
    };

    fn handlers(fixture: &Fixture) -> Handlers {
        let (tx, _rx) = mpsc::channel();
        test_handlers(tx, fixture)
    }

    #[test]
    fn the_reset_key_resets_the_view_unless_help_is_shown() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);
        assert_eq!(
            CommandResult::from(Command::ResetView),
            handlers.handle_key(KeyCode::Esc, KeyModifiers::NONE)
        );

        handlers
            .root
            .handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(
            CommandResult::NotHandled,
            handlers.handle_key(KeyCode::Esc, KeyModifiers::NONE)
        );
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

    /// The clipboard may name the entries a delete removes, so a confirmed
    /// delete clears it. Opening the prompt and declining it leave it alone.
    #[test]
    fn only_a_confirmed_delete_clears_the_clipboard() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);
        let entry = ClipboardEntry::Copy(vec![fixture.file()]);
        handlers.handle_command(&Command::SetClipboardEntry(Some(entry.clone())));

        assert_eq!(
            CommandResult::NotHandled,
            handlers.handle_command(&Command::OpenPrompt(PromptAction::Delete(1)))
        );
        assert_eq!(
            CommandResult::NotHandled,
            handlers.handle_command(&Command::CancelPrompt)
        );
        assert_eq!(
            CommandResult::from(entry.into_paste(fixture.directory())),
            handlers.handle_command(&Command::Paste(fixture.directory()))
        );

        assert_eq!(
            CommandResult::from(Command::SetClipboardEntry(None)),
            handlers.handle_command(&Command::ConfirmDelete)
        );
    }

    #[test_case(true => Command::AlertWarn("Cannot paste: nothing has been copied or cut".into()) ; "with a system clipboard")]
    #[test_case(false => Command::AlertWarn("Cannot paste: no system clipboard available".into()) ; "without one")]
    fn a_paste_with_nothing_to_paste_says_why(system_clipboard: bool) -> Command {
        nothing_to_paste(system_clipboard)
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

    /// Quitting ends the worker, so a delete it has not finished is confirmed
    /// first. A task counts until its terminal progress is handled, which
    /// nothing feeds back here, so the count holds for the whole test.
    #[test]
    fn quit_asks_first_while_file_operations_are_running() {
        let fixture = Fixture::new();
        let mut handlers = handlers(&fixture);
        let paths: Vec<_> = ["a.txt", "b.txt"]
            .map(|name| {
                let path = fixture.cwd().join(name);
                std::fs::write(&path, b"x").unwrap();
                PathInfo::try_from(path.as_path()).unwrap()
            })
            .into();
        handlers.file_system.handle_command(&Command::Delete(paths));

        assert_eq!(
            CommandResult::from(Command::OpenPrompt(PromptAction::ConfirmQuit(2))),
            handlers.handle_key(KeyCode::Char('q'), KeyModifiers::NONE)
        );
    }

    #[test_case(KeyCode::Char('q'), KeyModifiers::NONE => CommandResult::from(Command::Quit) ; "quit while idle")]
    #[test_case(KeyCode::Char('K'), KeyModifiers::SHIFT => CommandResult::from(Command::CancelTask) ; "cancel task")]
    #[test_case(KeyCode::Esc, KeyModifiers::NONE => CommandResult::from(Command::ResetView) ; "reset view")]
    #[test_case(KeyCode::Char('j'), KeyModifiers::NONE => CommandResult::NotHandled ; "an action another handler owns")]
    fn a_global_key_becomes_its_command(code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let fixture = Fixture::new();
        handlers(&fixture).handle_key(code, modifiers)
    }
}
