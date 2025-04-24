use rat_widget::text::TextRange;
use rat_widget::textarea::{self as text_area};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::layout::Position;

use super::word_navigation;
use super::PromptView;
use crate::command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command};

// Remove module declaration from here
// mod word_navigation;

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::OpenPrompt(kind) => self.open(kind),
            Command::SetDirectory(directory, _) => self.set_directory(directory),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        let key_event = KeyEvent::new(*code, *modifiers);
        let event = Event::Key(key_event);

        match (*code, *modifiers) {
            (KeyCode::Esc, _) => Command::ClosePrompt.into(),
            (KeyCode::Enter, _) => self.submit(),
            (KeyCode::Left, KeyModifiers::CONTROL) => {
                let text = self.text_area_state.text();
                let current_pos = self.text_area_state.cursor();
                let current_byte_offset = self
                    .text_area_state
                    .try_bytes_at_range(TextRange::new((0, 0), current_pos))
                    .map(|r| r.end)
                    .unwrap_or(0); // Use unwrap_or for default

                let new_byte_offset =
                    word_navigation::find_prev_word_boundary(&text, current_byte_offset);
                let new_pos = self
                    .text_area_state
                    .try_byte_pos(new_byte_offset)
                    .unwrap_or(current_pos);

                self.text_area_state.set_cursor(new_pos, false);
                CommandResult::Handled
            }
            (KeyCode::Right, KeyModifiers::CONTROL) => {
                let text = self.text_area_state.text();
                let current_pos = self.text_area_state.cursor();
                let current_byte_offset = self
                    .text_area_state
                    .try_bytes_at_range(TextRange::new((0, 0), current_pos))
                    .map(|r| r.end)
                    .unwrap_or(0); // Use unwrap_or for default

                let new_byte_offset =
                    word_navigation::find_next_word_boundary(&text, current_byte_offset);
                let new_pos = self
                    .text_area_state
                    .try_byte_pos(new_byte_offset)
                    .unwrap_or(current_pos);

                self.text_area_state.set_cursor(new_pos, false);
                CommandResult::Handled
            }
            (KeyCode::Right, _) => {
                let text_area_state = &mut self.text_area_state;
                text_area::handle_events(text_area_state, true, &event);

                // Workaround https://github.com/thscharler/rat-salsa/issues/6
                let cursor_position_x = text_area_state.cursor().x;
                let hscroll_offset = text_area_state.hscroll.offset();
                let is_position_after_right_edge =
                    cursor_position_x == text_area_state.area.width as u32 + hscroll_offset as u32;
                if is_position_after_right_edge {
                    text_area_state.hscroll.set_offset(hscroll_offset + 1);
                }
                CommandResult::Handled
            }
            (_, _) => {
                let text_area_state = &mut self.text_area_state;
                text_area::handle_events(text_area_state, true, &event);
                CommandResult::Handled
            }
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        text_area::handle_mouse_events(&mut self.text_area_state, &Event::Mouse(*event));
        CommandResult::Handled
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.text_area_state.area.contains(Position { x, y })
    }
}
