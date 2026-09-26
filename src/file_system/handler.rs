use super::{FileSystem, Shown, tasks::TaskCommand};
use crate::command::{Command, handler::CommandHandler, result::CommandResult};

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::GoToParentDirectory => self.go_to_parent_directory(),
            Command::GoToPreviousDirectory => self.go_to_previous_directory(),
            Command::CancelTask => self.cancel_most_recent_task(),
            Command::ResetView => {
                self.cancel_search();
                self.shown = Shown::Directory;
                CommandResult::NotHandled
            }
            Command::AddBookmark { directory, name } => self.add_bookmark(directory, name),
            Command::GetBookmarks => self.show_bookmarks(),
            Command::Chmod { paths, mode } => self.chmod(paths, mode),
            Command::CreateDirectory(name) => self.create_directory(name),
            Command::Copy { srcs, dest } => self.start_paste(false, srcs, dest),
            Command::Move { srcs, dest } => self.start_paste(true, srcs, dest),
            Command::ResolveConflict(choice) => self.resolve_conflict(*choice),
            // Dismissing the conflict prompt abandons the rest of the paste.
            // A no-op for every other prompt, which leaves nothing pending.
            Command::CancelPrompt => self.cancel_paste(),
            Command::Delete(paths) => {
                let mut commands = Vec::new();
                let batch = self.next_batch();
                for path in paths {
                    let (_, task_commands) =
                        self.run_task(batch, TaskCommand::Delete(path.clone()));
                    commands.extend(task_commands);
                }
                commands.into()
            }
            Command::Open(path) => self.open(path),
            Command::OpenCurrentDirectory => self.open_current_directory(),
            Command::OpenNewWindow => self.open_new_window(),
            Command::OpenWith { argv, label, path } => self.open_with(label, path, argv),
            Command::Progress(task) => self.check_progress_for_error(task),
            Command::RefreshDirectory => self.refresh(),
            Command::DirectoryListingComplete { generation } => {
                self.on_listing_complete(*generation)
            }
            Command::Rename { path, name } => self.rename(path, name),
            Command::ExitedSearch { generation } => {
                self.on_search_exited(*generation);
                CommandResult::NotHandled
            }
            Command::StartSearch(query) => self.search(query),
            Command::ListingBatch { items, generation } => {
                self.search_batch(items, *generation);
                CommandResult::NotHandled
            }
            Command::SearchResultsRefreshed { items, generation } => {
                if *generation == self.current_search_generation {
                    self.search_results = items.iter().map(|item| item.path.clone()).collect();
                }
                CommandResult::NotHandled
            }
            _ => CommandResult::NotHandled,
        }
    }
}
