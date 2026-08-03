use super::{FileSystem, read_bookmarks, tasks::TaskCommand};
use crate::command::{Command, handler::CommandHandler, result::CommandResult};

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::GoToParentDirectory => self.go_to_parent_directory(),
            Command::GoToPreviousDirectory => self.go_to_previous_directory(),
            Command::CancelTask => self.cancel_most_recent_task(),
            Command::ResetView => {
                self.cancel_search();
                CommandResult::NotHandled
            }
            Command::AddBookmark { directory, name } => self.add_bookmark(directory, name),
            Command::GetBookmarks => match read_bookmarks(&self.bookmarks_dir) {
                // The bookmarks view replaces any in-flight search; cancel it
                // so its walk stops and its final ExitedSearch clears the
                // search notice. The in-flight directory load is cancelled
                // too, so its batches cannot stream into the bookmarks
                // listing. Both are no-ops when nothing is running.
                //
                // Cancel only once the listing is known to replace them: a
                // failed read broadcasts no Bookmarks command, so nothing
                // would clear the table's loading flag, and a load cancelled
                // mid-drain returns without sending DirectoryListingComplete
                // to clear it instead. The table would be stranded on a
                // truncated, unsorted listing.
                Ok(bookmarks) => {
                    self.cancel_search();
                    self.cancel_current_load();
                    Command::Bookmarks { bookmarks }.into()
                }
                Err(message) => Command::AlertError(message).into(),
            },
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
                for path in paths {
                    let (_, task_commands) = self.run_task(TaskCommand::Delete(path.clone()));
                    commands.extend(task_commands);
                }
                commands.into()
            }
            Command::Open(path) => self.open(path),
            Command::OpenCurrentDirectory => self.open_current_directory(),
            Command::OpenNewWindow => self.open_new_window(),
            Command::OpenWith {
                argv,
                label,
                working_dir,
            } => self.open_with(working_dir.as_deref(), label, argv),
            Command::Progress(task) => self.check_progress_for_error(task),
            Command::RefreshDirectory => self.refresh(),
            Command::Rename { path, name } => self.rename(path, name),
            Command::ExitedSearch { generation } => {
                self.on_search_exited(*generation);
                CommandResult::NotHandled
            }
            Command::StartSearch(query) => self.search(query),
            _ => CommandResult::NotHandled,
        }
    }
}
