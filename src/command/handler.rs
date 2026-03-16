use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseEvent};

use super::{Command, mode::InputMode, result::CommandResult};

pub trait CommandHandler {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![]
    }

    fn handle_command(&mut self, _command: &Command) -> CommandResult {
        CommandResult::NotHandled
    }

    fn handle_key(&mut self, _code: &KeyCode, _modifiers: &KeyModifiers) -> CommandResult {
        // Only invoked if self.should_receive_key() is true
        CommandResult::NotHandled
    }

    fn handle_mouse(&mut self, _event: &MouseEvent) -> CommandResult {
        // Only invoked if self.should_receive_mouse() is true
        CommandResult::NotHandled
    }

    fn should_handle_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Normal)
    }

    fn should_handle_mouse(&self, _event: &MouseEvent) -> bool {
        false
    }
}
