use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseEvent};

use super::{Command, InputMode, result::CommandResult};

pub trait CommandHandler {
    fn visit_command_handlers(&mut self, _visitor: &mut dyn FnMut(&mut dyn CommandHandler)) {}

    fn handle_command(&mut self, _command: &Command) -> CommandResult {
        CommandResult::NotHandled
    }

    fn handle_key(&mut self, _code: &KeyCode, _modifiers: &KeyModifiers) -> CommandResult {
        // Only invoked if self.should_handle_key() returns true
        CommandResult::NotHandled
    }

    fn handle_mouse(&mut self, _event: &MouseEvent) -> CommandResult {
        // Only invoked if self.should_handle_mouse() returns true
        CommandResult::NotHandled
    }

    fn should_handle_key(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Normal)
    }

    fn should_handle_mouse(&self, _event: &MouseEvent) -> bool {
        false
    }
}
