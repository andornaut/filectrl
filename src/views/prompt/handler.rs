use rat_widget::textarea::{self as text_area};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};

use super::PromptView;
use crate::command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command};

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
        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),
            _ => {
                let text_area_state = &mut self.input_state;
                text_area::handle_events(text_area_state, true, &event);

                // Workaround https://github.com/thscharler/rat-salsa/issues/6
                let cursor_position_x = text_area_state.cursor().x;
                let hscroll_offset = text_area_state.hscroll.offset();
                let is_position_after_right_edge =
                    cursor_position_x == text_area_state.area.width as u32 + hscroll_offset as u32;
                if *code == KeyCode::Right && is_position_after_right_edge {
                    text_area_state.hscroll.set_offset(hscroll_offset + 1);
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
