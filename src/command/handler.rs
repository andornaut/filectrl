use super::{result::CommandResult, Command};
use crate::command::Focus;

pub trait CommandHandler {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![]
    }

    fn handle_command(&mut self, _command: &Command) -> CommandResult {
        CommandResult::NotHandled
    }

    fn is_focussed(&self, _focus: &Focus) -> bool {
        false
    }
}
