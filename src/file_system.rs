use self::path::HumanPath;
use crate::command::{
    handler::CommandHandler, result::CommandResult, unwrap_option_or_error_command,
    unwrap_or_error_command, Command,
};
use anyhow::{anyhow, Result};
use std::{fs, path::PathBuf};

mod converters;
mod human;
pub mod path;

#[derive(Default)]
pub struct FileSystem {
    directory: HumanPath,
}

impl FileSystem {
    pub fn cd_to_cwd(&mut self) -> Result<Command> {
        // TODO accept a path specified on the CLI
        self.cd(HumanPath::default())
    }

    fn back(&mut self) -> Result<Option<Command>> {
        let path_buf = PathBuf::try_from(&self.directory.path)?;

        if let Some(parent) = path_buf.parent() {
            let parent = HumanPath::try_from(&parent.to_path_buf())?;
            return Ok(Some(self.cd(parent)?));
        }

        // We're at the root directory, so do nothing.
        return Ok(None);
    }

    fn cd(&mut self, directory: HumanPath) -> Result<Command> {
        let entries = fs::read_dir(&directory.path)?;
        let (children, errors): (Vec<_>, Vec<_>) = entries
            .map(|entry| -> Result<HumanPath> { HumanPath::try_from(&entry?.path()) })
            .partition(Result::is_ok);
        if !errors.is_empty() {
            return Err(anyhow!("Some paths could not be read: {:?}", errors));
        }
        let mut children: Vec<HumanPath> = children.into_iter().map(Result::unwrap).collect();
        children.sort();
        self.directory = directory.clone();
        Ok(Command::UpdateCurrentDir(directory, children))
    }

    fn delete(&mut self, path: &HumanPath) -> CommandResult {
        eprintln!("TODO: Delete: {path:?}");
        self.refresh()
    }

    fn rename(&mut self, old_path: &HumanPath, new_basename: &str) -> CommandResult {
        eprintln!(
            "TODO: Rename: {old_basename} -> {new_basename}",
            old_basename = old_path.basename
        );
        self.refresh()
    }

    fn refresh(&mut self) -> CommandResult {
        unwrap_or_error_command(self.cd(self.directory.clone())).into()
    }
}

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::BackDir => CommandResult::option(unwrap_option_or_error_command(self.back())),
            Command::ChangeDir(directory) => {
                unwrap_or_error_command(self.cd(directory.clone())).into()
            }
            Command::DeletePath(path) => self.delete(path).into(),
            Command::RefreshDir => self.refresh(),
            Command::RenamePath(old_path, new_basename) => self.rename(old_path, new_basename),
            _ => CommandResult::NotHandled,
        }
    }
}
