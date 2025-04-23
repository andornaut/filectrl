use rat_widget::text::TextRange;
use rat_widget::textarea::{self as text_area};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};

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

        let is_ctrl = modifiers.contains(KeyModifiers::CONTROL);

        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),

            // Use the refactored word navigation logic with conversions
            KeyCode::Left if is_ctrl => {
                let text = self.input_state.text();
                let current_pos = self.input_state.cursor();
                // Handle Result from bytes_at_range, default to 0 offset on error
                let current_byte_offset = self
                    .input_state
                    .try_bytes_at_range(TextRange::new((0, 0), current_pos))
                    .map(|r| r.end)
                    .unwrap_or(0); // Use unwrap_or for default

                let new_byte_offset =
                    word_navigation::find_prev_word_boundary(&text, current_byte_offset);
                // Handle Result from byte_pos, default to current pos on error
                let new_pos = self
                    .input_state
                    .try_byte_pos(new_byte_offset)
                    .unwrap_or(current_pos);

                self.input_state.set_cursor(new_pos, false);
                //self.input_state.scroll_cursor_to_visible();
                CommandResult::Handled
            }
            KeyCode::Right if is_ctrl => {
                let text = self.input_state.text();
                let current_pos = self.input_state.cursor();
                // Handle Result from bytes_at_range, default to 0 offset on error
                let current_byte_offset = self
                    .input_state
                    .try_bytes_at_range(TextRange::new((0, 0), current_pos))
                    .map(|r| r.end)
                    .unwrap_or(0); // Use unwrap_or for default

                let new_byte_offset =
                    word_navigation::find_next_word_boundary(&text, current_byte_offset);
                // Handle Result from byte_pos, default to current pos on error
                let new_pos = self
                    .input_state
                    .try_byte_pos(new_byte_offset)
                    .unwrap_or(current_pos);

                self.input_state.set_cursor(new_pos, false);
                //self.input_state.scroll_cursor_to_visible();
                CommandResult::Handled
            }

            // Default handling for other keys
            _ => {
                let text_area_state = &mut self.input_state;
                text_area::handle_events(text_area_state, true, &event);

                // Workaround https://github.com/thscharler/rat-salsa/issues/6
                // Only apply if not handled by our custom CTRL+Right
                if *code == KeyCode::Right && !is_ctrl {
                    let cursor_position_x = text_area_state.cursor().x;
                    let hscroll_offset = text_area_state.hscroll.offset();
                    // Check area width before using it
                    if text_area_state.area.width > 0 {
                        let is_position_after_right_edge = cursor_position_x
                            == text_area_state.area.width as u32 + hscroll_offset as u32;
                        if is_position_after_right_edge {
                            text_area_state.hscroll.set_offset(hscroll_offset + 1);
                        }
                    }
                }
                CommandResult::Handled
            }
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        text_area::handle_mouse_events(&mut self.input_state, &Event::Mouse(*event));
        CommandResult::Handled
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        let area = self.input_area;
        x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
    }
}
