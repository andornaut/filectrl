// The r# prefix is a "raw identifier" syntax in Rust that allows using reserved keywords
// as identifiers. Here we use r#async because 'async' is a reserved keyword that we want
// to use as a module name.
pub mod r#async;
pub mod debounce;
pub mod handler;
pub mod path_info;
pub mod sync;
pub mod watcher;

use std::{fs, path::PathBuf, sync::mpsc::Sender};

use anyhow::{anyhow, Result};
use log::{error, info};
use r#async::TaskCommand;

use self::{path_info::PathInfo, sync::open_in, watcher::DirectoryWatcher};
use crate::{
    app::config::Config,
    command::{result::CommandResult, task::Task, Command},
};

pub struct FileSystem {
    command_tx: Sender<Command>,
    directory: Option<PathInfo>,
    open_current_directory_template: Option<String>,
    open_new_window_template: Option<String>,
    open_selected_file_template: Option<String>,
    watcher: DirectoryWatcher,
}

impl FileSystem {
    pub fn new(config: &Config, command_tx: Sender<Command>) -> Self {
        Self {
            command_tx,
            directory: None,
            open_current_directory_template: config.open_current_directory_template.clone(),
            open_new_window_template: config.open_new_window_template.clone(),
            open_selected_file_template: config.open_selected_file_template.clone(),
            watcher: DirectoryWatcher::try_new().expect("Can initialize watcher"),
        }
    }

    pub fn run_once(&mut self, directory: Option<PathBuf>) -> Result<Command> {
        self.watcher.run_once(&self.command_tx);

        let directory = match directory {
            Some(dir) => PathInfo::try_from(&dir.canonicalize()?)?,
            None => PathInfo::default(),
        };

        self.cd(directory).try_into()
    }

    fn back(&mut self) -> CommandResult {
        let directory = self.directory.as_ref().unwrap();
        match directory.parent() {
            Some(parent) => self.cd(parent),
            None => CommandResult::Handled,
        }
    }

    fn cd(&mut self, directory: PathInfo) -> CommandResult {
        (match sync::cd(&directory) {
            Ok(children) => {
                self.directory = Some(directory.clone());

                if let Err(e) = self.watcher.watch_directory(PathBuf::from(&directory.path)) {
                    error!("Failed to watch directory: {}", e);
                }
                Command::SetDirectory(directory, children)
            }
            Err(error) => anyhow!("Failed to change to directory {directory:?}: {error}").into(),
        })
        .into()
    }

    fn handle_progress(&mut self, task: &Task) -> CommandResult {
        task.error_message()
            .map_or(CommandResult::NotHandled, |msg| {
                Command::AlertError(msg).into()
            })
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
                        Ok(_) => CommandResult::Handled,
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
        .map_or_else(|error| error.into(), |_| CommandResult::Handled)
    }

    fn open_custom(&self, path: &PathInfo) -> CommandResult {
        open_in(self.open_selected_file_template.clone(), &path.path)
            .map_or_else(|error| error.into(), |_| CommandResult::Handled)
    }

    fn open_new_window(&self) -> CommandResult {
        let directory = self.directory.as_ref().unwrap();
        open_in(self.open_new_window_template.clone(), &directory.path)
            .map_or_else(|error| error.into(), |_| CommandResult::Handled)
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

    fn run_task(&mut self, task: TaskCommand) -> CommandResult {
        task.run(self.command_tx.clone())
    }
}
