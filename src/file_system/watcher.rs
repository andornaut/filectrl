use std::{
    path::PathBuf,
    sync::mpsc::{channel, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use log::error;
use notify::{recommended_watcher, Event, RecommendedWatcher, Watcher};

use crate::{command::Command, file_system::debounce};

const CHECK_DELAYED_THRESHOLD: Duration = Duration::from_millis(1000);
const DEBOUNCE_THRESHOLD: Duration = Duration::from_millis(500);

pub struct DirectoryWatcher {
    notify_rx: Option<Receiver<std::result::Result<Event, notify::Error>>>,
    watched_directory: Option<PathBuf>,
    watcher: RecommendedWatcher,
}

impl DirectoryWatcher {
    pub fn try_new() -> Result<Self> {
        let (notify_tx, notify_rx) = channel();
        let watcher = recommended_watcher(notify_tx)?;
        Ok(Self {
            notify_rx: Some(notify_rx),
            watcher,
            watched_directory: None,
        })
    }

    pub fn run_once(&mut self, command_tx: &Sender<Command>) {
        let notify_rx = match self.notify_rx.take() {
            Some(rx) => rx,
            None => return, // Already running, do nothing
        };

        let (delayed_tx, delayed_rx) = channel();
        let command_tx_for_delayed = command_tx.clone();
        let command_tx_for_notify = command_tx.clone();
        thread::spawn(move || watch_for_delayed_commands(command_tx_for_delayed, delayed_rx));
        thread::spawn(move || {
            watch_for_notify_events(command_tx_for_notify, delayed_tx, notify_rx)
        });
    }

    pub(super) fn watch_directory(&mut self, path: PathBuf) -> Result<()> {
        if let Some(old_path) = &self.watched_directory {
            if let Err(e) = self.watcher.unwatch(old_path.as_path()) {
                error!("Failed to unwatch directory: {}", e);
            }
        }

        self.watcher
            .watch(path.as_path(), notify::RecursiveMode::NonRecursive)?;
        self.watched_directory = Some(path);
        Ok(())
    }
}

/// Watches for file system events and debounces them to prevent too frequent refreshes
///
/// This function runs in a background thread and handles file system events:
/// 1. When a file system event arrives, it checks if enough time has passed since the last refresh
/// 2. If enough time has passed, it triggers a refresh immediately
/// 3. If not enough time has passed, it schedules a delayed refresh after the debounce period
/// 4. Multiple events within the debounce period will only result in one delayed refresh
///
/// The debouncing is implemented using a timer thread that sleeps for the debounce period
/// before sending the refresh command. This ensures we don't miss the last update in a series
/// of rapid changes.
fn watch_for_notify_events(
    command_tx: Sender<Command>,
    delayed_tx: Sender<Command>,
    notify_rx: Receiver<std::result::Result<Event, notify::Error>>,
) {
    let mut debouncer = debounce::TimeDebouncer::new(DEBOUNCE_THRESHOLD);
    for result in notify_rx {
        match result {
            Ok(event) => match event.kind {
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_) => {
                    if debouncer.should_trigger(Instant::now()) {
                        if let Err(e) = command_tx.send(Command::Refresh) {
                            error!("Failed to send refresh command: {}", e);
                        }
                    } else if !debouncer.has_delayed_event() {
                        if let Err(e) = delayed_tx.send(Command::Refresh) {
                            error!("Failed to schedule delayed refresh: {}", e);
                        } else {
                            debouncer.set_delayed_event();
                        }
                    }
                }
                _ => (),
            },
            Err(e) => {
                error!("File system watcher error: {}", e);
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

fn watch_for_delayed_commands(command_tx: Sender<Command>, delayed_rx: Receiver<Command>) {
    while let Ok(command) = delayed_rx.recv() {
        thread::sleep(CHECK_DELAYED_THRESHOLD);
        if let Err(e) = command_tx.send(command) {
            error!("Failed to send delayed refresh command: {}", e);
        }
    }
}
