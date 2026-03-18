use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    layout::Position,
};
use ratatui_textarea::{CursorMove, Input};

use super::PromptView;
use crate::command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::OpenPrompt(kind, initial_text) => self.open(kind, initial_text),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Esc, _) => return Command::ClosePrompt.into(),
            (KeyCode::Enter, _) => return self.submit(),
            (KeyCode::Char('a'), m) if m == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
                self.text_area.select_all();
                return CommandResult::Handled;
            }
            (KeyCode::Char('v'), KeyModifiers::CONTROL) => {
                if let Some(text) = self.clipboard.get_text() {
                    self.text_area.set_yank_text(text);
                }
                self.text_area.paste();
                return CommandResult::Handled;
            }
            _ => {}
        }

        self.text_area.input(Input::from(KeyEvent::new(*code, *modifiers)));

        if matches!(code, KeyCode::Char('c') | KeyCode::Char('x'))
            && modifiers.contains(KeyModifiers::CONTROL)
        {
            self.clipboard.set_text(&self.text_area.yank_text());
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
        self.render_area.contains(Position { x: event.column, y: event.row })
    }
}
