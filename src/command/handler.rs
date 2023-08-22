use super::{mode::InputMode, result::CommandResult, Command};
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent};

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

    fn handle_mouse(&mut self, _mouse: &MouseEvent) -> CommandResult {
        // Only invoked if self.should_receive_mouse() is true
        CommandResult::NotHandled
    }

    fn should_receive_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Normal)
    }

    fn should_receive_mouse(&self, _column: u16, _row: u16) -> bool {
        false
    }
}
