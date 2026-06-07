use super::StatusView;
use crate::command::{Command, handler::CommandHandler, result::CommandResult};

impl CommandHandler for StatusView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::NavigatedDirectory {
                directory,
                generation,
            }
            | Command::RefreshedDirectory {
                directory,
                generation,
            } => self.begin_directory(directory.clone(), *generation),
            Command::DirectoryListing { items, generation } => {
                self.count_listing(items, *generation)
            }
            Command::SelectionChanged(selected) => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }
}
