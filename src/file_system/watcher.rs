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
}

impl DirectoryWatcher {
    pub fn try_new(command_tx: Sender<Command>) -> Result<Self> {
        let command_tx = command_tx;
        let (tx, rx) = channel();
        let watcher = recommended_watcher(tx)?;

        thread::spawn(move || background_watcher(command_tx, rx));
        Ok(Self { watcher })
    }

    pub fn watch_directory(&mut self, path: PathBuf) -> Result<()> {
        self.watcher
            .watch(path.as_path(), notify::RecursiveMode::NonRecursive)?;
        Ok(())
    }

    pub fn unwatch_directory(&mut self, path: PathBuf) -> Result<()> {
        self.watcher.unwatch(path.as_path())?;
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
                let error_command =
                    Command::AlertError(format!("Failed to watch directory: {}", e));
                if let Err(e) = command_tx.send(error_command) {
                    error!("Failed to send error command: {}", e);
                }
            }
        }
    }
}
