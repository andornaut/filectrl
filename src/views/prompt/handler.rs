use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Position,
};
use ratatui_textarea::{CursorMove, Input};

use super::PromptView;
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, InputMode, PromptAction, handler::CommandHandler, result::CommandResult},
};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::OpenPrompt(kind) => self.open(kind),
            Command::TextFromClipboard(text) => {
                self.text_area.set_yank_text(text);
                self.text_area.paste();
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        // Delete confirmation: single-keypress y/Y confirms, anything else cancels
        if matches!(self.actions, PromptAction::Delete(_)) {
            return match code {
                KeyCode::Char('y' | 'Y') => Command::ConfirmDelete.into(),
                _ => Command::CancelPrompt.into(),
            };
        }

        // Hardcoded: Esc always cancels
        if *code == KeyCode::Esc {
            return Command::CancelPrompt.into();
        }

        // Rebindable prompt keys (lookup once, reuse after textarea input)
        let action = Config::global().keybindings.prompt_action(code, modifiers);
        match action {
            Some(Action::PromptSubmit) => return self.submit(),
            Some(Action::PromptSelectAll) => {
                self.text_area.select_all();
                return CommandResult::Handled;
            }
            Some(Action::PromptPaste) => {
                return Command::ReadFromClipboard.into();
            }
            Some(Action::PromptReset) => {
                self.reset_text(&self.initial_text.clone());
                return CommandResult::Handled;
            }
            _ => {}
        }

        self.text_area
            .input(Input::from(KeyEvent::new(*code, *modifiers)));

        // Copy/Cut must be checked after textarea processes the key, because
        // ratatui-textarea populates yank_text from the current selection during input().
        if matches!(action, Some(Action::PromptCopy) | Some(Action::PromptCut)) {
            return Command::WriteToClipboard(self.text_area.yank_text()).into();
        }

        CommandResult::Handled
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
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
                self.text_area.input(Input::from(*event)); // handles scroll wheel
            }
        }
        CommandResult::Handled
    }

    fn should_handle_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        self.render_area.contains(Position {
            x: event.column,
            y: event.row,
        })
    }
}
