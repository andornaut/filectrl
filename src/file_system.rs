use crossterm::event::{KeyCode, KeyModifiers};

use self::{human::HumanPath, operations::open_in};
use crate::{
    app::config::Config,
    command::{handler::CommandHandler, result::CommandResult, Command},
};
use anyhow::{anyhow, Result};
use std::{fs, path::PathBuf};

mod converters;
pub mod human;
mod operations;

pub struct FileSystem {
    directory: HumanPath,
    open_current_directory_template: Option<String>,
    open_new_window_template: Option<String>,
    open_selected_file_template: Option<String>,
}

impl FileSystem {
    pub fn new(config: &Config) -> Self {
        Self {
            directory: HumanPath::default(),
            open_current_directory_template: config.open_current_directory_template.clone(),
            open_new_window_template: config.open_new_window_template.clone(),
            open_selected_file_template: config.open_selected_file_template.clone(),
        }
    }

    pub fn init(&mut self, directory: Option<PathBuf>) -> Result<Command> {
        match directory {
            Some(directory) => match directory.canonicalize() {
                Ok(directory) => match HumanPath::try_from(&directory) {
                    Ok(directory) => self.cd(directory),
                    Err(error) => {
                        anyhow!("Cannot change directory to {directory:?}: {error:?}").into()
                    }
                },
                Err(error) => anyhow!("Cannot change directory to {directory:?}: {error:?}").into(),
            },
            None => self.cd(HumanPath::default()),
        }
        .try_into()
    }

    fn back(&mut self) -> CommandResult {
        match self.directory.parent() {
            Some(parent) => self.cd(parent),
            None => CommandResult::none(),
        }
    }

    fn cd(&mut self, directory: HumanPath) -> CommandResult {
        (match operations::cd(&directory) {
            Ok(children) => {
                self.directory = directory.clone();
                Command::SetDirectory(directory, children)
            }
            Err(error) => anyhow!("Cannot change directory to {directory}: {error}").into(),
        })
        .into()
    }

    fn delete(&mut self, path: &HumanPath) -> CommandResult {
        match operations::delete(path) {
            Err(error) => anyhow!("Cannot delete {path}: {error}").into(),
            Ok(_) => self.refresh(),
        }
    }

    fn open(&mut self, path: &HumanPath) -> CommandResult {
        let path = fs::canonicalize(&path.path).unwrap();
        let path = HumanPath::try_from(&path).unwrap();
        if path.is_directory() {
            self.cd(path)
        } else {
            match open::that_detached(&path.path) {
                Err(error) => anyhow!("Failed to open file: {error}").into(),
                Ok(_) => CommandResult::none(),
            }
        }
    }

    fn open_current_directory(&self) -> CommandResult {
        open_in(
            self.open_current_directory_template.clone(),
            &self.directory.path,
        )
        .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn open_custom(&self, path: &HumanPath) -> CommandResult {
        open_in(self.open_selected_file_template.clone(), &path.path)
            .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn open_new_window(&self) -> CommandResult {
        open_in(self.open_new_window_template.clone(), &self.directory.path)
            .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn rename(&mut self, old_path: &HumanPath, new_basename: &str) -> CommandResult {
        match operations::rename(old_path, new_basename) {
            Err(error) => anyhow!("Cannot rename {old_path} to {new_basename}: {error}").into(),
            Ok(_) => self.refresh(),
        }
    }

    fn refresh(&mut self) -> CommandResult {
        self.cd(self.directory.clone())
    }
}

impl CommandHandler for FileSystem {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::DeletePath(path) => self.delete(path),
            Command::Open(path) => self.open(path),
            Command::OpenCustom(path) => self.open_custom(path),
            Command::RenamePath(old_path, new_basename) => self.rename(old_path, new_basename),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::F(5), _) => self.refresh(),
            (code, _) => match code {
                KeyCode::Backspace | KeyCode::Left | KeyCode::Char('b') | KeyCode::Char('h') => {
                    self.back()
                }
                KeyCode::Char('w') => self.open_new_window(),
                KeyCode::Char('t') => self.open_current_directory(),
                _ => CommandResult::NotHandled,
            },
        }
    }
}
