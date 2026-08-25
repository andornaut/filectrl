use super::StatusView;
use crate::command::{Command, handler::CommandHandler, result::CommandResult};

impl CommandHandler for StatusView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::NavigatedDirectory {
                directory,
                generation,
            } => self.begin_directory(directory.clone(), *generation),
            Command::RefreshedDirectory {
                directory,
                generation,
            } => self.begin_reload(directory.clone(), *generation),
            Command::ListingBatch { items, generation } => self.count_listing(items, *generation),
            Command::DirectoryListingComplete { generation } => self.finish_listing(*generation),
            Command::SelectionChanged { selected, .. } => self.set_selected(selected.clone()),
            _ => CommandResult::NotHandled,
        }
    }
}
