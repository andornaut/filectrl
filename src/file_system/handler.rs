use super::{FileSystem, tasks::TaskCommand};
use crate::command::{Command, handler::CommandHandler, result::CommandResult};

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Back => self.back(),
            Command::CancelTask => self.cancel_most_recent(),
            Command::Chmod { paths, mode } => self.chmod(paths, mode),
            Command::CreateDirectory(name) => self.create_directory(name),
            Command::Copy { srcs, dest } => {
                self.run_batch(
                    srcs.iter()
                        .map(|src| TaskCommand::Copy(src.clone(), dest.clone())),
                );
                CommandResult::Handled
            }
            Command::Move { srcs, dest } => {
                self.run_batch(
                    srcs.iter()
                        .map(|src| TaskCommand::Move(src.clone(), dest.clone())),
                );
                CommandResult::Handled
            }
            Command::Delete(paths) => {
                self.run_batch(paths.iter().map(|path| TaskCommand::Delete(path.clone())));
                CommandResult::Handled
            }
            Command::Open(path) => self.open(path),
            Command::OpenCurrentDirectory => self.open_current_directory(),
            Command::OpenNewWindow => self.open_new_window(),
            Command::Progress(task) => self.check_progress_for_error(task),
            Command::Refresh => self.refresh(),
            Command::Rename { path, name } => self.rename(path, name),
            Command::SearchComplete => {
                self.search_cancel = None;
                CommandResult::NotHandled
            }
            Command::StartSearch(query) => {
                self.search(query);
                CommandResult::Handled
            }
            _ => CommandResult::NotHandled,
        }
    }
}
