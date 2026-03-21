use super::{tasks::TaskCommand, FileSystem};
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Back => self.back(),
            Command::Copy { srcs, dest } => {
                self.run_batch(srcs.iter().map(|src| TaskCommand::Copy(src.clone(), dest.clone())));
                CommandResult::Handled
            }
            Command::Move { srcs, dest } => {
                self.run_batch(srcs.iter().map(|src| TaskCommand::Move(src.clone(), dest.clone())));
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
            Command::Rename(old_path, new_basename) => self.rename(old_path, new_basename),
            _ => CommandResult::NotHandled,
        }
    }
}
