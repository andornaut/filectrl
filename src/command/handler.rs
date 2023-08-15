use crossterm::event::{KeyCode, KeyModifiers};

use crate::command::mode::InputMode;

use super::{result::CommandResult, Command};

pub trait CommandHandler {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![]
    }

    fn handle_command(&mut self, _command: &Command) -> CommandResult {
        CommandResult::NotHandled
    }

    fn handle_input(&mut self, _code: &KeyCode, _modifiers: &KeyModifiers) -> CommandResult {
        // Only invoked if self.should_receive_input() is true
        CommandResult::NotHandled
    }

    fn should_receive_input(&self, mode: &InputMode) -> bool {
        matches!(mode, InputMode::Normal)
    }
}
