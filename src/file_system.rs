use std::path::PathBuf;

use self::human::HumanPath;
use crate::command::{handler::CommandHandler, result::CommandResult, Command};

mod converters;
pub mod human;
mod operations;

#[derive(Default)]
pub struct FileSystem {
    directory: HumanPath,
}

impl FileSystem {
    pub fn init(&mut self, directory: Option<PathBuf>) -> CommandResult {
        match directory {
            Some(directory) => match directory.canonicalize() {
                Ok(directory) => match HumanPath::try_from(&directory) {
                    Ok(directory) => self.cd(directory),
                    Err(error) => Command::AddError(format!(
                        "Cannot change directory to {:?}: {error}",
                        directory
                    ))
                    .into(),
                },
                Err(error) => Command::AddError(format!(
                    "Cannot change directory to {:?}: {error}",
                    directory
                ))
                .into(),
            },
            None => self.cd(HumanPath::default()),
        }
    }

    fn back(&mut self) -> CommandResult {
        match self.directory.parent() {
            Some(parent) => self.cd(parent),
            None => CommandResult::none(),
        }
    }

    fn cd(&mut self, directory: HumanPath) -> CommandResult {
        // TODO This fails entirely if eg. `directory` contains one broken
        // symlink. This should handle this case more gracefully:
        // include the broken file in the returned directory list.
        (match operations::cd(&directory) {
            Ok(children) => {
                self.directory = directory.clone();
                Command::SetDirectory(directory, children)
            }
            Err(error) => {
                Command::AddError(format!("Cannot change directory to {directory}: {error}"))
            }
        })
        .into()
    }

    fn delete(&mut self, path: &HumanPath) -> CommandResult {
        if let Err(error) = operations::delete(path) {
            return Command::AddError(format!("Cannot delete {path}: {error}")).into();
        }
        self.refresh()
    }

    fn open(&mut self, path: &HumanPath) -> CommandResult {
        open::that_in_background(&path.path);
        CommandResult::none()
    }

    fn rename(&mut self, old_path: &HumanPath, new_basename: &str) -> CommandResult {
        if let Err(error) = operations::rename(old_path, new_basename) {
            let message = format!("Cannot rename {old_path} to {new_basename}: {error}");
            return Command::AddError(message).into();
        }
        self.refresh()
    }

    fn refresh(&mut self) -> CommandResult {
        self.cd(self.directory.clone())
    }
}

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::BackDir => self.back(),
            Command::ChangeDir(directory) => self.cd(directory.clone()),
            Command::DeletePath(path) => self.delete(path),
            Command::OpenFile(path) => self.open(path),
            Command::RefreshDir => self.refresh(),
            Command::RenamePath(old_path, new_basename) => self.rename(old_path, new_basename),
            _ => CommandResult::NotHandled,
        }
    }
}
