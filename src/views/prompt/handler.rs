use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui_textarea::{CursorMove, Input};

use super::PromptView;
use crate::{
    app::config::{Config, keybindings::Action},
    command::{
        Command, ConflictChoice, InputMode, PromptAction, handler::CommandHandler,
        result::CommandResult,
    },
    views::contains,
};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::OpenPrompt(kind) => self.open(kind),
            Command::ClipboardText(text) => {
                let result = self.insert_text(text);
                self.filter_as_typed(result)
            }
            // Any close of a filter prompt without submitting restores the original filter, as Esc
            // does.
            Command::CancelPrompt => self.restore_filter(),
            Command::FilterChanged(_) | Command::NavigatedDirectory { .. } | Command::ResetView => {
                self.live_filter.clone_from(&self.initial_text);
                CommandResult::NotHandled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        // Only Shift may accompany y: a chord such as Ctrl+y must not confirm a permanent delete.
        if matches!(self.actions, PromptAction::Delete(_)) {
            let plain = modifiers.difference(KeyModifiers::SHIFT).is_empty();
            return match code {
                KeyCode::Char('y' | 'Y') if plain => Command::ConfirmDelete.into(),
                _ => Command::CancelPrompt.into(),
            };
        }

        if matches!(self.actions, PromptAction::ConfirmQuit(_)) {
            let plain = modifiers.difference(KeyModifiers::SHIFT).is_empty();
            return match code {
                KeyCode::Char('y' | 'Y') if plain => Command::Quit.into(),
                _ => Command::CancelPrompt.into(),
            };
        }

        // Confirmed because any program could have put the text on the clipboard.
        if let PromptAction::ConfirmPaste { entry, dest } = &self.actions {
            let plain = modifiers.difference(KeyModifiers::SHIFT).is_empty();
            return match code {
                KeyCode::Char('y' | 'Y') if plain => entry.clone().into_paste(dest.clone()).into(),
                _ => Command::CancelPrompt.into(),
            };
        }

        // Uppercase answers for the rest of the batch. Only Shift may accompany a choice,
        // so a chord like Ctrl+O cancels instead of overwriting.
        if let PromptAction::Conflict { can_overwrite, .. } = self.actions {
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
                // Not offered for this collision: ignored so a batch answered with `o` is not
                // abandoned.
                KeyCode::Char('o' | 'O') if plain => CommandResult::Handled,
                _ => Command::CancelPrompt.into(),
            };
        }

        let action = Config::global().keybindings.prompt_action(code, modifiers);
        let result = self.handle_text_key(action, code, modifiers);
        self.filter_as_typed(result)
    }

    /// Text in a y/n prompt is ignored rather than read as answers.
    fn handle_paste(&mut self, text: &str) -> CommandResult {
        if self.actions.is_confirmation() {
            return CommandResult::Handled;
        }
        let result = self.insert_text(text);
        self.filter_as_typed(result)
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
        contains(self.render_area, event)
    }
}

impl PromptView {
    /// Sends the filter prompt's text to the table as it is typed, when it differs from what was
    /// last sent.
    fn filter_as_typed(&mut self, result: CommandResult) -> CommandResult {
        let is_filter = matches!(self.actions, PromptAction::Filter(_));
        if !is_filter || result == CommandResult::NotHandled {
            return result;
        }
        let mut commands = result.into_commands();
        let closes = commands
            .iter()
            .any(|command| matches!(command, Command::FilterChanged(_) | Command::CancelPrompt));
        let text = self.text_area.lines().join("");
        if !closes && text != self.live_filter {
            self.live_filter.clone_from(&text);
            commands.push(Command::FilterEdited(text));
        }
        commands.into()
    }

    /// Closes the prompt, restoring the filter a filter prompt opened with.
    fn cancel(&mut self) -> CommandResult {
        let mut commands = self.restore_filter().into_commands();
        commands.push(Command::CancelPrompt);
        commands.into()
    }

    /// The filter the filter prompt opened with, if typing has applied another.
    fn restore_filter(&mut self) -> CommandResult {
        let is_filter = matches!(self.actions, PromptAction::Filter(_));
        if !is_filter || self.live_filter == self.initial_text {
            return CommandResult::NotHandled;
        }
        self.live_filter.clone_from(&self.initial_text);
        Command::FilterEdited(self.initial_text.clone()).into()
    }

    /// Inserts `text` at the cursor without control characters: the input is one line.
    fn insert_text(&mut self, text: &str) -> CommandResult {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        self.text_area.set_yank_text(text);
        self.text_area.paste();
        self.refresh_suggestions();
        CommandResult::Handled
    }

    /// The text-editing half of `handle_key`; `action` is the prompt keybinding for `code` and
    /// `modifiers`.
    pub(super) fn handle_text_key(
        &mut self,
        action: Option<Action>,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> CommandResult {
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
            Some(Action::PromptCancel) => return self.cancel(),
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
            // Handled here because `input()` hardcodes its copy and cut keys. An empty selection
            // copies nothing.
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

        if inserts_whitespace(code, modifiers) {
            return CommandResult::Handled;
        }
        self.text_area
            .input(Input::from(KeyEvent::new(code, modifiers)));

        if matches!(self.actions, PromptAction::Goto { .. }) {
            self.refresh_suggestions();
        }

        CommandResult::Handled
    }
}

/// Whether `input()` would insert a line break or tab for this key; the input is one line.
fn inserts_whitespace(code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('\n' | '\r') => true,
        KeyCode::Char('m') => {
            modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT)
        }
        _ => false,
    }
}
