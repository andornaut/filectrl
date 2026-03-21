mod debounce;
mod handler;
pub mod path_info;
mod operations;
mod tasks;
mod watch;

use std::{fmt::Display, fs, path::PathBuf, sync::mpsc::Sender};

use anyhow::{anyhow, Result};
use log::warn;

use self::{
    path_info::PathInfo, operations::open_in, tasks::TaskCommand, watch::DirectoryWatcher,
};
use crate::{
    app::config::Config,
    command::{result::CommandResult, progress::Task, Command},
};

pub struct FileSystem {
    buffer_max_bytes: u64,
    buffer_min_bytes: u64,
    command_tx: Sender<Command>,
    directory: Option<PathInfo>,
    open_current_directory_template: String,
    open_new_window_template: String,
    open_selected_file_template: String,
    watcher: Option<DirectoryWatcher>,
}

impl FileSystem {
    pub fn new(config: &Config, command_tx: Sender<Command>) -> Self {
        let watcher = DirectoryWatcher::try_new(config.file_system.refresh_debounce_milliseconds)
            .inspect_err(|e| {
                warn!("Failed to initialize directory watcher: {e}");
                let _ = command_tx.send(Command::AlertWarn(format!(
                    "Directory watcher unavailable: {e}. Use Ctrl+R to refresh manually."
                )));
            })
            .ok();
        Self {
            buffer_max_bytes: config.file_system.buffer_max_bytes,
            buffer_min_bytes: config.file_system.buffer_min_bytes,
            command_tx,
            directory: None,
            open_current_directory_template: config.openers.open_current_directory.clone(),
            open_new_window_template: config.openers.open_new_window.clone(),
            open_selected_file_template: config.openers.open_selected_file.clone(),
            watcher,
        }
    }

    pub fn run_once(&mut self, directory: Option<PathBuf>) -> Result<Command> {
        if let Some(watcher) = &mut self.watcher {
            watcher.run_once(&self.command_tx);
        }

        let directory = directory
            .and_then(|path| {
                path.canonicalize()
                    .inspect_err(|error| self.send_directory_error(&path, error))
                    .ok()
            })
            .and_then(|path| {
                PathInfo::try_from(&path)
                    .inspect_err(|error| self.send_directory_error(&path, error))
                    .ok()
            })
            .unwrap_or_default();

        self.cd(directory, true).try_into()
    }

    fn current_directory(&self) -> &PathInfo {
        self.directory.as_ref().expect("directory is set before any navigation command")
    }

    fn back(&mut self) -> CommandResult {
        match self.current_directory().parent() {
            Some(parent) => self.cd(parent, true),
            None => CommandResult::Handled,
        }
    }

    fn cd(&mut self, directory: PathInfo, navigate: bool) -> CommandResult {
        match operations::cd(&directory) {
            Ok((children, error_count)) => {
                if error_count > 0 {
                    let _ = self.command_tx.send(Command::AlertWarn(format!(
                        "{error_count} entries in {directory:?} could not be read"
                    )));
                }
                self.directory = Some(directory.clone());
                let path_buf = PathBuf::from(&directory.path);
                if let Some(watcher) = &mut self.watcher
                    && let Err(e) = watcher.watch_directory(path_buf.clone())
                {
                    self.send_directory_error(&path_buf, e);
                }
                if navigate {
                    Command::NavigateDirectory(directory, children)
                } else {
                    Command::RefreshDirectory(directory, children)
                }
            }
            Err(error) => anyhow!("Failed to change to directory {directory:?}: {error}").into(),
        }
        .into()
    }

    fn check_progress_for_error(&mut self, task: &Task) -> CommandResult {
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
                    self.cd(path, true)
                } else {
                    open_in(&path, &self.open_selected_file_template).into()
                }
            }
            Err(err) => err.into(),
        }
    }

    fn open_current_directory(&self) -> CommandResult {
        open_in(self.current_directory(), &self.open_current_directory_template).into()
    }

    fn open_new_window(&self) -> CommandResult {
        open_in(self.current_directory(), &self.open_new_window_template).into()
    }

    fn rename(&mut self, path: &PathInfo, new_basename: &str) -> CommandResult {
        match operations::rename(path, new_basename) {
            Err(error) => anyhow!("Failed to rename {path:?} to {new_basename:?}: {error}").into(),
            Ok(_) => self.refresh(),
        }
    }

    fn refresh(&mut self) -> CommandResult {
        self.cd(self.current_directory().clone(), false)
    }

    fn run_batch(&mut self, tasks: impl Iterator<Item = TaskCommand>) {
        for task in tasks {
            let result = task.run(
                self.command_tx.clone(),
                self.buffer_min_bytes,
                self.buffer_max_bytes,
            );
            // Send initial progress commands through the channel so NoticesView picks them up
            if let CommandResult::HandledWith(cmd) = result {
                let _ = self.command_tx.send(*cmd);
            }
        }
    }

    fn send_directory_error(&self, dir: &PathBuf, error: impl Display) {
        self.command_tx
            .send(Command::AlertWarn(format!(
                "Failed to read directory {dir:?}: {error}"
            )))
            .expect("command channel should be open");
    }
}
