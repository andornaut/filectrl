use crate::app::command::{Command, CommandHandler};
use anyhow::{anyhow, Result};

use std::{env, fs};

use self::path_display::PathDisplay;

mod human;
pub mod path_display;

#[derive(Default)]
pub struct FileSystem {}

impl FileSystem {
    pub fn cd_to_cwd(&self) -> Result<Command> {
        let directory = env::current_dir()?;
        let directory = PathDisplay::try_from(directory)?;
        self.cd(&directory)
    }

    fn cd(&self, directory: &PathDisplay) -> Result<Command> {
        let directory = directory.clone();
        let entries = fs::read_dir(&directory.path)?;
        let (children, errors): (Vec<_>, Vec<_>) = entries
            .map(|entry| -> Result<PathDisplay> { PathDisplay::try_from(entry?.path()) })
            .partition(Result::is_ok);
        if !errors.is_empty() {
            return Err(anyhow!("Some paths could not be read: {:?}", errors));
        }
        let mut children: Vec<PathDisplay> = children.into_iter().map(Result::unwrap).collect();
        children.sort();
        Ok(Command::UpdateCurrentDir(directory, children))
    }
}

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> Option<Command> {
        match command {
            Command::_ChangeDir(directory) => {
                // TODO Propate errors by returning a Result here, and adding an error message Command in App
                Some(self.cd(directory).unwrap())
            }
            _ => None,
        }
    }
}
