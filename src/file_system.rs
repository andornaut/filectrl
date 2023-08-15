use crossterm::event::{KeyCode, KeyModifiers};

use self::human::HumanPath;
use crate::command::{handler::CommandHandler, result::CommandResult, Command};
use anyhow::{anyhow, Result};
use std::{fs, path::PathBuf};

mod converters;
pub mod human;
mod operations;

const TERMINAL_EMULATOR: &'static str = "x-terminal-emulator";

#[derive(Default)]
pub struct FileSystem {
    directory: HumanPath,
}

impl FileSystem {
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
            open::that_in_background(&path.path);
            CommandResult::none()
        }
    }

    fn open_terminal(&mut self) -> CommandResult {
        eprintln!("opening {} via {TERMINAL_EMULATOR}", &self.directory.path);
        open::with_in_background(&self.directory.path, TERMINAL_EMULATOR);
        CommandResult::none()
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
            Command::RenamePath(old_path, new_basename) => self.rename(old_path, new_basename),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_input(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Backspace, _)
            | (KeyCode::Left, _)
            | (KeyCode::Char('b'), _)
            | (KeyCode::Char('h'), _) => self.back(),
            (KeyCode::Char('t'), _) => self.open_terminal(),
            (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::F(5), _) => self.refresh(),
            _ => CommandResult::NotHandled,
        }
    }
}
