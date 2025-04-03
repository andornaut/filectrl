mod converters;
mod handler;
mod operations;
pub mod path_info;

use std::{fs, path::PathBuf, sync::mpsc::Sender};

use anyhow::{anyhow, Result};
use log::info;

use self::{
    handler::TaskCommand,
    operations::{open_in, run_task},
    path_info::PathInfo,
};
use crate::{
    app::config::Config,
    command::{result::CommandResult, task::Task, Command},
};

pub struct FileSystem {
    directory: PathInfo,
    open_current_directory_template: Option<String>,
    open_new_window_template: Option<String>,
    open_selected_file_template: Option<String>,
    tx: Option<Sender<Command>>,
}

impl FileSystem {
    pub fn new(config: &Config) -> Self {
        Self {
            directory: PathInfo::default(),
            open_current_directory_template: config.open_current_directory_template.clone(),
            open_new_window_template: config.open_new_window_template.clone(),
            open_selected_file_template: config.open_selected_file_template.clone(),
            tx: None,
        }
    }

    pub fn init(&mut self, directory: Option<PathBuf>, tx: Sender<Command>) -> Result<Command> {
        self.tx = Some(tx);

        match directory {
            Some(directory) => match directory.canonicalize() {
                Ok(directory) => match PathInfo::try_from(&directory) {
                    Ok(directory) => self.cd(directory),
                    Err(error) => {
                        anyhow!("Failed to change to directory {directory:?}: {error:?}").into()
                    }
                },
                Err(error) => {
                    anyhow!("Failed to change to directory {directory:?}: {error:?}").into()
                }
            },
            None => self.cd(PathInfo::default()),
        }
        .try_into()
    }

    fn back(&mut self) -> CommandResult {
        match self.directory.parent() {
            Some(parent) => self.cd(parent),
            None => CommandResult::none(),
        }
    }

    fn cd(&mut self, directory: PathInfo) -> CommandResult {
        (match operations::cd(&directory) {
            Ok(children) => {
                self.directory = directory.clone();
                Command::SetDirectory(directory, children)
            }
            Err(error) => anyhow!("Failed to change to directory {directory:?}: {error}").into(),
        })
        .into()
    }

    fn delete(&mut self, path: &PathInfo) -> CommandResult {
        match operations::delete(path) {
            Err(error) => anyhow!("Failed to delete {path:?}: {error}").into(),
            Ok(_) => self.refresh(),
        }
    }

    fn failed_task_to_error(&mut self, task: &Task) -> CommandResult {
        if let Some(message) = task.error_message() {
            Command::AddError(message).into()
        } else {
            CommandResult::none()
        }
    }

    fn open(&mut self, path: &PathInfo) -> CommandResult {
        match fs::canonicalize(&path.path)
            .map_err(anyhow::Error::from)
            .and_then(|path| PathInfo::try_from(&path))
        {
            Ok(path) => {
                if path.is_directory() {
                    self.cd(path)
                } else {
                    info!("Opening path:\"{path}\"");
                    match open::that_detached(&path.path) {
                        Err(error) => anyhow!("Failed to open {path:?}: {error}").into(),
                        Ok(_) => CommandResult::none(),
                    }
                }
            }
            Err(err) => err.into(),
        }
    }

    fn open_current_directory(&self) -> CommandResult {
        open_in(
            self.open_current_directory_template.clone(),
            &self.directory.path,
        )
        .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn open_custom(&self, path: &PathInfo) -> CommandResult {
        open_in(self.open_selected_file_template.clone(), &path.path)
            .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn open_new_window(&self) -> CommandResult {
        open_in(self.open_new_window_template.clone(), &self.directory.path)
            .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn mv(&mut self, old_path: &PathInfo, new_path: &PathInfo) -> CommandResult {
        match operations::mv(old_path, new_path) {
            Err(error) => anyhow!("Failed to move {old_path:?} to {new_path:?}: {error}").into(),
            Ok(_) => self.refresh(),
        }
    }

    fn rename(&mut self, old_path: &PathInfo, new_basename: &str) -> CommandResult {
        match operations::rename(old_path, new_basename) {
            Err(error) => {
                anyhow!("Failed to rename {old_path:?} to {new_basename:?}: {error}").into()
            }
            Ok(_) => self.refresh(),
        }
    }

    fn refresh(&mut self) -> CommandResult {
        self.cd(self.directory.clone())
    }

    fn run_task(&mut self, task_command: TaskCommand) -> CommandResult {
        let tx = self.tx.as_ref().expect("Sender is set");
        match run_task(tx.clone(), task_command) {
            Err(error) => error.into(),
            Ok(task) => Command::Progress(task).into(),
        }
    }
}
