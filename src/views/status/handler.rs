use super::StatusView;
use crate::command::{Command, handler::CommandHandler, result::CommandResult};

impl CommandHandler for StatusView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::NavigatedDirectory { directory, children }
            | Command::RefreshedDirectory { directory, children } => {
                self.set_directory(directory.clone(), children)
            }
            Command::SelectionChanged(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }
}
