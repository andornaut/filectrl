mod r#async;
mod handler;
pub mod path_info;
mod sync;
mod watcher;

use std::{fs, path::PathBuf, sync::mpsc::Sender};

use anyhow::{anyhow, Result};
use log::{error, info};

use self::{path_info::PathInfo, sync::open_in, watcher::DirectoryWatcher};
use crate::{
    app::config::Config,
    command::{result::CommandResult, Command},
};

pub struct FileSystem {
    directory: Option<PathInfo>,
    open_current_directory_template: Option<String>,
    open_new_window_template: Option<String>,
    open_selected_file_template: Option<String>,
    tx: Option<Sender<Command>>,
    watcher: Option<DirectoryWatcher>,
}

impl FileSystem {
    pub fn new(config: &Config) -> Self {
        Self {
            directory: None,
            open_current_directory_template: config.open_current_directory_template.clone(),
            open_new_window_template: config.open_new_window_template.clone(),
            open_selected_file_template: config.open_selected_file_template.clone(),
            tx: None,
            watcher: None,
        }
    }

    pub fn init(&mut self, directory: Option<PathBuf>, tx: Sender<Command>) -> Result<Command> {
        self.tx = Some(tx.clone());
        self.watcher = Some(DirectoryWatcher::try_new(tx)?);

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
        let directory = self.directory.as_ref().unwrap();
        match directory.parent() {
            Some(parent) => self.cd(parent),
            None => CommandResult::none(),
        }
    }
    fn cd(&mut self, directory: PathInfo) -> CommandResult {
        let watcher = self.watcher.as_mut().expect("Watcher not initialized");

        (match sync::cd(&directory) {
            Ok(children) => {
                self.directory = Some(directory.clone());

                if let Err(e) = watcher.watch_directory(PathBuf::from(&directory.path)) {
                    error!("Failed to watch directory: {}", e);
                }
                Command::SetDirectory(directory, children)
            }
            Err(error) => anyhow!("Failed to change to directory {directory:?}: {error}").into(),
        })
        .into()
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
        let directory = self.directory.as_ref().unwrap();
        open_in(
            self.open_current_directory_template.clone(),
            &directory.path,
        )
        .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn open_custom(&self, path: &PathInfo) -> CommandResult {
        open_in(self.open_selected_file_template.clone(), &path.path)
            .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn open_new_window(&self) -> CommandResult {
        let directory = self.directory.as_ref().unwrap();
        open_in(self.open_new_window_template.clone(), &directory.path)
            .map_or_else(|error| error.into(), |_| CommandResult::none())
    }

    fn rename(&mut self, old_path: &PathInfo, new_basename: &str) -> CommandResult {
        match sync::rename(old_path, new_basename) {
            Err(error) => {
                anyhow!("Failed to rename {old_path:?} to {new_basename:?}: {error}").into()
            }
            Ok(_) => self.refresh(),
        }
    }

    fn refresh(&mut self) -> CommandResult {
        let directory = self.directory.as_ref().unwrap();
        self.cd(directory.clone())
    }
}
