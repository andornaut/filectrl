use super::{FileSystem, path_info::PathInfo, read_bookmarks, tasks::TaskCommand};
use crate::{
    app::clipboard::ClipboardEntry,
    command::{Command, handler::CommandHandler, result::CommandResult},
};

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
            Command::Copy { srcs, dest } => {
                let failed = self.run_batch(
                    srcs.iter()
                        .map(|src| TaskCommand::Copy(src.clone(), dest.clone())),
                );
                // The TableView clears its marks for these same commands, so
                // the clipboard follow-up (see `clipboard_follow_up`) rides
                // the same broadcast.
                clipboard_follow_up(srcs.len(), ClipboardEntry::Copy, failed)
            }
            Command::Move { srcs, dest } => {
                let failed = self.run_batch(
                    srcs.iter()
                        .map(|src| TaskCommand::Move(src.clone(), dest.clone())),
                );
                clipboard_follow_up(srcs.len(), ClipboardEntry::Move, failed)
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

/// The clipboard follow-up for a paste batch. When every source started, the
/// clipboard is cleared; when none started, it is kept untouched so the paste
/// can be retried as-is; when only some started, it is reduced to the failed
/// sources with the same operation, because a full retry would fail on the
/// already-pasted sources (their destinations now exist). `SetClipboardEntry`
/// updates the system clipboard text and the clipboard notice.
fn clipboard_follow_up(
    source_count: usize,
    entry: fn(Vec<PathInfo>) -> ClipboardEntry,
    failed: Vec<PathInfo>,
) -> CommandResult {
    if failed.is_empty() {
        Command::SetClipboardEntry(None).into()
    } else if failed.len() == source_count {
        CommandResult::Handled
    } else {
        Command::SetClipboardEntry(Some(entry(failed))).into()
    }
}
