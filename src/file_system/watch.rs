use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, Sender, channel},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use log::{debug, error, warn};
use notify::{Event, RecommendedWatcher, Watcher, recommended_watcher};

use crate::{command::Command, file_system::debounce};

pub struct DirectoryWatcher {
    debounce_threshold: Duration,
    handles: Vec<thread::JoinHandle<()>>,
    notify_rx: Option<Receiver<std::result::Result<Event, notify::Error>>>,
    watched_directory: Option<PathBuf>,
    /// Option so `Drop` can `.take()` it before joining the watcher threads.
    /// Dropping the watcher closes the notify channel sender, which unblocks
    /// the threads waiting on the receiver so they can exit.
    watcher: Option<RecommendedWatcher>,
}

impl DirectoryWatcher {
    pub fn try_new(debounce_ms: u64) -> Result<Self> {
        let (notify_tx, notify_rx) = channel();
        let watcher = recommended_watcher(notify_tx)?;
        Ok(Self {
            debounce_threshold: Duration::from_millis(debounce_ms),
            handles: Vec::new(),
            notify_rx: Some(notify_rx),
            watcher: Some(watcher),
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
        // Shared between both threads so a dispatched delayed refresh counts
        // as a trigger (clearing the delayed flag and resetting the window).
        let debouncer = Arc::new(Mutex::new(debounce::TimeDebouncer::new(
            self.debounce_threshold,
        )));
        let debouncer_for_delayed = Arc::clone(&debouncer);
        self.handles.push(thread::spawn(move || {
            watch_for_delayed_commands(command_tx_for_delayed, delayed_rx, debouncer_for_delayed)
        }));
        self.handles.push(thread::spawn(move || {
            watch_for_notify_events(command_tx_for_notify, delayed_tx, notify_rx, debouncer)
        }));
    }

    pub(super) fn watch_directory(&mut self, path: PathBuf) -> Result<()> {
        let Some(watcher) = &mut self.watcher else {
            return Ok(());
        };
        if let Some(old_path) = &self.watched_directory
            && let Err(e) = watcher.unwatch(old_path.as_path())
        {
            warn!("Failed to unwatch directory: {}", e);
        }

        watcher.watch(path.as_path(), notify::RecursiveMode::NonRecursive)?;
        self.watched_directory = Some(path);
        Ok(())
    }
}

impl Drop for DirectoryWatcher {
    fn drop(&mut self) {
        // Drop the watcher first: it owns the notify channel sender. Dropping it
        // causes notify_rx.recv() to return Err, which exits watch_for_notify_events,
        // which in turn drops delayed_tx, exiting watch_for_delayed_commands.
        // Without this, handle.join() below would block forever.
        self.watcher.take();
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
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
/// The debouncing is implemented using a timer thread that sleeps out the remainder of the
/// debounce period before sending the refresh command. This ensures we don't miss the last
/// update in a series of rapid changes.
fn watch_for_notify_events(
    command_tx: Sender<Command>,
    delayed_tx: Sender<Duration>,
    notify_rx: Receiver<std::result::Result<Event, notify::Error>>,
    debouncer: Arc<Mutex<debounce::TimeDebouncer>>,
) {
    for result in notify_rx {
        match result {
            Ok(event) => match event.kind {
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_) => {
                    let mut debouncer = debouncer.lock().unwrap();
                    if debouncer.should_trigger(Instant::now()) {
                        if let Err(e) = command_tx.send(Command::RefreshDirectory) {
                            error!("Failed to send refresh command: {}", e);
                        }
                    } else if !debouncer.has_delayed_event() {
                        let delay = debouncer.remaining(Instant::now());
                        if let Err(e) = delayed_tx.send(delay) {
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

/// Dispatches delayed refreshes. Each queued entry carries the remaining
/// debounce delay; after sleeping it out, the dispatch is routed through the
/// shared debouncer so it counts as a trigger (clearing the delayed flag and
/// resetting the window). `should_trigger` returns false if an event already
/// triggered a refresh while this thread was sleeping, in which case the
/// delayed refresh is redundant and skipped.
fn watch_for_delayed_commands(
    command_tx: Sender<Command>,
    delayed_rx: Receiver<Duration>,
    debouncer: Arc<Mutex<debounce::TimeDebouncer>>,
) {
    while let Ok(mut delay) = delayed_rx.recv() {
        // Wait out the remainder on the channel rather than sleeping, so a
        // disconnect (shutdown) interrupts the wait instead of blocking
        // `Drop`'s join for up to the full debounce window.
        loop {
            match delayed_rx.recv_timeout(delay) {
                Ok(next) => delay = next,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if debouncer.lock().unwrap().should_trigger(Instant::now())
            && let Err(e) = command_tx.send(Command::RefreshDirectory)
        {
            debug!("Delayed refresh not sent, likely due to shutdown: {}", e);
        }
    }
}
