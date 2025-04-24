use rat_widget::textarea;
use ratatui::{
    crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent},
    layout::Position,
};

use super::{word_navigation, PromptView};
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

        match (*code, *modifiers) {
            (KeyCode::Esc, _) => Command::ClosePrompt.into(),
            (KeyCode::Enter, _) => self.submit(),
            (KeyCode::Left, KeyModifiers::CONTROL) => {
                self.navigate_by_word_boundary(word_navigation::find_prev_word_boundary)
            }
            (KeyCode::Right, KeyModifiers::CONTROL) => {
                self.navigate_by_word_boundary(word_navigation::find_next_word_boundary)
            }
            (KeyCode::Right, _) => self.workaround_navigate_right_when_at_edge(&event),
            (_, _) => {
                let text_area_state = &mut self.text_area_state;
                textarea::handle_events(text_area_state, true, &event);
                CommandResult::Handled
            }
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        textarea::handle_mouse_events(&mut self.text_area_state, &Event::Mouse(*event));
        CommandResult::Handled
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Prompt)
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.text_area_state.area.contains(Position { x, y })
    }
}
