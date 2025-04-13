use log::debug;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent};

use super::PromptView;
use crate::command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command};
use rat_focus::HasFocus;
use rat_widget::textarea::{self as text_area};

impl CommandHandler for PromptView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::SetDirectory(directory, _) => {
                if let Some(previous_directory) = &self.directory {
                    if previous_directory.path != directory.path {
                        self.directory = Some(directory.clone());
                        return Command::SetFilter("".into()).into();
                    }
                }
                CommandResult::none()
            }
            Command::OpenPrompt(kind) => self.open(kind),
            Command::SetFilter(filter) => self.set_filter(filter.clone()),
            Command::SetSelected(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        let key_event = KeyEvent::new(*code, *modifiers);
        let event = Event::Key(key_event);
        let mut state = &mut self.input_state;
        match *code {
            KeyCode::Esc => Command::ClosePrompt.into(),
            KeyCode::Enter => self.submit(),
            _ => {
                text_area::handle_events(&mut state, true, &event);
                let mut cursor_position = state.cursor();

                // Don't allow the cursor to move past the end of the text
                if cursor_position.x == state.text().len() as u32 {
                    cursor_position.x = cursor_position.x - 1;
                    state.set_cursor(cursor_position, false);
                }

                // Workaround https://github.com/thscharler/rat-salsa/issues/5
                let area_width = state.area.width;
                if *code == KeyCode::Right && cursor_position.x >= area_width as u32 {
                    let hscroll = state.hscroll.offset();
                    let last_position = state.text().len() - hscroll - 1;
                    if cursor_position.x <= last_position as u32 {
                        state.hscroll.set_offset(hscroll + 1);
                    }
                }
                CommandResult::none()
            }
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        let mut state = &mut self.input_state;
        debug!("mouse_event: {:?}, column: {:?}", event, event.column);
        let event = Event::Mouse(*event);
        let event_result = text_area::handle_readonly_events(&mut state, true, &event);
        debug!("mouse result: {:?}", event_result);
        CommandResult::none()
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        let area = self.input_state.area();
        x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
    }
}
