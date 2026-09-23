use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Position,
};
use ratatui_textarea::{CursorMove, Input};

use super::PromptView;
use crate::{
    app::config::{Config, keybindings::Action},
    command::{
        Command, ConflictChoice, InputMode, PromptAction, handler::CommandHandler,
        result::CommandResult,
    },
};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::OpenPrompt(kind) => self.open(kind),
            Command::ClipboardText(text) => {
                self.text_area.set_yank_text(text);
                self.text_area.paste();
                // Pasting changes the input, so the Goto suggestions must be
                // recomputed like any other edit (no-op for other prompts).
                self.refresh_suggestions();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        // Delete confirmation: single-keypress y/Y confirms, anything else
        // cancels. A chord such as Ctrl+y is a different key and must not
        // confirm a permanent delete, so only Shift may accompany the letter.
        if matches!(self.actions, PromptAction::Delete(_)) {
            let plain = modifiers.difference(KeyModifiers::SHIFT).is_empty();
            return match code {
                KeyCode::Char('y' | 'Y') if plain => Command::ConfirmDelete.into(),
                _ => Command::CancelPrompt.into(),
            };
        }

        // Paste of an entry from elsewhere: confirmed like a delete, since the
        // text could have been put on the clipboard by any program.
        if let PromptAction::ConfirmPaste { entry, dest } = &self.actions {
            let plain = modifiers.difference(KeyModifiers::SHIFT).is_empty();
            return match code {
                KeyCode::Char('y' | 'Y') if plain => entry.clone().into_paste(dest.clone()).into(),
                _ => Command::CancelPrompt.into(),
            };
        }

        // Paste conflict: single keypress, uppercase answering for the rest of
        // the batch too. Overwrite is only bound when the existing entry is not
        // a directory, so an unbound key cancels the paste rather than falling
        // through to a choice the prompt did not offer.
        if let PromptAction::Conflict { can_overwrite, .. } = self.actions {
            // Shift is what produces the uppercase "all" choices, so it is the
            // only modifier the offered keys carry. A chord like Ctrl+O is a
            // different key entirely and must not resolve to the destructive
            // choice it shares a letter with; it falls through to the cancel
            // below, which loses nothing because the clipboard is restored.
            let plain = modifiers.difference(KeyModifiers::SHIFT).is_empty();
            return match code {
                KeyCode::Char('s') if plain => {
                    Command::ResolveConflict(ConflictChoice::Skip).into()
                }
                KeyCode::Char('S') if plain => {
                    Command::ResolveConflict(ConflictChoice::SkipAll).into()
                }
                KeyCode::Char('o') if plain && can_overwrite => {
                    Command::ResolveConflict(ConflictChoice::Overwrite).into()
                }
                KeyCode::Char('O') if plain && can_overwrite => {
                    Command::ResolveConflict(ConflictChoice::OverwriteAll).into()
                }
                // A real choice that this collision cannot offer. Ignoring it
                // keeps the prompt up: treating it as the abandon key would
                // lose the rest of a batch for someone who has been answering
                // `o` and reaches the first directory.
                KeyCode::Char('o' | 'O') if plain => CommandResult::Handled,
                _ => Command::CancelPrompt.into(),
            };
        }

        let action = Config::global().keybindings.prompt_action(code, modifiers);
        self.handle_text_key(action, code, modifiers)
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> CommandResult {
        let visual_col = event.column.saturating_sub(self.render_area.x);
        let char_idx = self.display_col_to_char_idx(visual_col.saturating_add(self.scroll_col));
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.text_area.cancel_selection();
                self.text_area.move_cursor(CursorMove::Jump(0, char_idx));
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.text_area.is_selecting() {
                    self.text_area.start_selection();
                }
                self.text_area.move_cursor(CursorMove::Jump(0, char_idx));
            }
            _ => {
                self.text_area.input(Input::from(event)); // handles scroll wheel
            }
        }
        CommandResult::Handled
    }

    fn should_handle_key(&self, mode: InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_handle_mouse(&self, event: MouseEvent) -> bool {
        self.render_area.contains(Position {
            x: event.column,
            y: event.row,
        })
    }
}

impl PromptView {
    /// The text-editing half of `handle_key`, after the single-keypress prompts
    /// have had their turn. `action` is `code` and `modifiers` looked up in the
    /// prompt keybindings.
    pub(super) fn handle_text_key(
        &mut self,
        action: Option<Action>,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> CommandResult {
        // Goto type-ahead: Tab accepts, Enter accepts then submits,
        // Down/Up cycle through matches
        if matches!(self.actions, PromptAction::Goto { .. }) {
            match action {
                Some(Action::PromptAcceptSuggestion) => {
                    self.accept_suggestion();
                    return CommandResult::Handled;
                }
                Some(Action::PromptNextSuggestion) => {
                    self.cycle_suggestion(1);
                    return CommandResult::Handled;
                }
                Some(Action::PromptPreviousSuggestion) => {
                    self.cycle_suggestion(-1);
                    return CommandResult::Handled;
                }
                Some(Action::PromptSubmit) => {
                    self.accept_suggestion();
                    return self.submit();
                }
                _ => {}
            }
        }
        match action {
            Some(Action::PromptCancel) => return Command::CancelPrompt.into(),
            Some(Action::PromptSubmit) => return self.submit(),
            Some(Action::PromptSelectAll) => {
                self.text_area.select_all();
                return CommandResult::Handled;
            }
            Some(Action::PromptPaste) => {
                return Command::GetClipboardText.into();
            }
            Some(Action::PromptReset) => {
                self.reset_text(&self.initial_text.clone());
                self.refresh_suggestions();
                return CommandResult::Handled;
            }
            // Performed here rather than left to `input()`, whose copy and cut
            // keys are hardcoded and ignore the keybindings. Without a
            // selection there is nothing to copy, and the yank buffer still
            // holds whatever was last cut or pasted. A selection that was
            // started and moved back to its anchor is empty, and `copy()`
            // leaves the yank buffer alone for it too.
            Some(Action::PromptCopy) => {
                if self
                    .text_area
                    .selection_range()
                    .is_none_or(|(start, end)| start == end)
                {
                    return CommandResult::Handled;
                }
                self.text_area.copy();
                return Command::SetClipboardText(self.text_area.yank_text()).into();
            }
            Some(Action::PromptCut) => {
                if !self.text_area.cut() {
                    return CommandResult::Handled;
                }
                self.refresh_suggestions();
                return Command::SetClipboardText(self.text_area.yank_text()).into();
            }
            _ => {}
        }

        self.text_area
            .input(Input::from(KeyEvent::new(code, modifiers)));

        if matches!(self.actions, PromptAction::Goto { .. }) {
            self.refresh_suggestions();
        }

        CommandResult::Handled
    }
}
