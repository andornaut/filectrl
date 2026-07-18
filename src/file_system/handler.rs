use super::{FileSystem, tasks::TaskCommand};
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
            Command::GetBookmarks => self.get_bookmarks(),
            Command::Chmod { paths, mode } => self.chmod(paths, mode),
            Command::CreateDirectory(name) => self.create_directory(name),
            Command::Copy { srcs, dest } => {
                self.run_batch(
                    srcs.iter()
                        .map(|src| TaskCommand::Copy(src.clone(), dest.clone())),
                );
                // The TableView clears its marks for these same commands; clearing
                // the clipboard here keeps both effects on a single broadcast.
                Command::ClearClipboard.into()
            }
            Command::Move { srcs, dest } => {
                self.run_batch(
                    srcs.iter()
                        .map(|src| TaskCommand::Move(src.clone(), dest.clone())),
                );
                Command::ClearClipboard.into()
            }
            Command::Delete(paths) => {
                self.run_batch(paths.iter().map(|path| TaskCommand::Delete(path.clone())));
                CommandResult::Handled
            }
            Command::Open(path) => self.open(path),
            Command::OpenCurrentDirectory => self.open_current_directory(),
            Command::OpenNewWindow => self.open_new_window(),
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
