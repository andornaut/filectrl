use std::{
    path::PathBuf,
    sync::mpsc::{channel, Receiver, Sender},
    thread,
};

use anyhow::Result;
use log::error;
use notify::{recommended_watcher, Event, RecommendedWatcher, Watcher};

use crate::command::Command;

pub struct DirectoryWatcher {
    watcher: RecommendedWatcher,
    currently_watched: Option<PathBuf>,
}

impl DirectoryWatcher {
    pub fn try_new(command_tx: Sender<Command>) -> Result<Self> {
        let command_tx = command_tx;
        let (tx, rx) = channel();
        let watcher = recommended_watcher(tx)?;

        thread::spawn(move || background_watcher(command_tx, rx));
        Ok(Self {
            watcher,
            currently_watched: None,
        })
    }

    pub fn watch_directory(&mut self, path: PathBuf) -> Result<()> {
        if let Some(old_path) = &self.currently_watched {
            if let Err(e) = self.watcher.unwatch(old_path.as_path()) {
                error!("Failed to unwatch directory: {}", e);
            }
        }

        self.watcher
            .watch(path.as_path(), notify::RecursiveMode::NonRecursive)?;
        self.currently_watched = Some(path);
        Ok(())
    }
}

fn background_watcher(
    command_tx: Sender<Command>,
    rx: Receiver<std::result::Result<Event, notify::Error>>,
) {
    for result in rx {
        match result {
            Ok(event) => match event.kind {
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_) => {
                    if let Err(e) = command_tx.send(Command::Refresh) {
                        error!("Failed to send refresh command: {}", e);
                    }
                }
                _ => {}
            },
            Err(e) => {
                let error_command = Command::AlertError(format!(
                    "Failed to run the directory watcher in the background: {}",
                    e
                ));
                if let Err(e) = command_tx.send(error_command) {
                    error!("Failed to send error command: {}", e);
                }
            }
        }
    }
}
