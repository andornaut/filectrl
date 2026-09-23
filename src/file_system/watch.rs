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

/// How many times its own duration a directory listing waits before the next
/// refresh may start, so a listing that is slow to read (a huge directory)
/// takes at most a fifth of the time while something keeps writing to it.
const LISTING_COST_FACTOR: u32 = 4;

pub struct DirectoryWatcher {
    debounce_threshold: Duration,
    /// Shared with the watcher threads, which read the current window from it.
    debouncer: Arc<Mutex<debounce::TimeDebouncer>>,
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
        let debounce_threshold = Duration::from_millis(debounce_ms);
        Ok(Self {
            debounce_threshold,
            debouncer: Arc::new(Mutex::new(debounce::TimeDebouncer::new(debounce_threshold))),
            handles: Vec::new(),
            notify_rx: Some(notify_rx),
            watcher: Some(watcher),
            watched_directory: None,
        })
    }

    pub fn run_once(&mut self, command_tx: &Sender<Command>) {
        // Already running, do nothing
        let Some(notify_rx) = self.notify_rx.take() else {
            return;
        };

        let (delayed_tx, delayed_rx) = channel();
        let command_tx_for_delayed = command_tx.clone();
        let command_tx_for_notify = command_tx.clone();
        // Shared between both threads so a dispatched delayed refresh counts
        // as a trigger (clearing the delayed flag and resetting the window).
        let debouncer = Arc::clone(&self.debouncer);
        let debouncer_for_delayed = Arc::clone(&debouncer);
        self.handles.push(thread::spawn(move || {
            watch_for_delayed_commands(
                &command_tx_for_delayed,
                &delayed_rx,
                &debouncer_for_delayed,
            );
        }));
        self.handles.push(thread::spawn(move || {
            watch_for_notify_events(&command_tx_for_notify, &delayed_tx, notify_rx, &debouncer);
        }));
    }

    /// Widens the refresh window to `LISTING_COST_FACTOR` times what the last
    /// listing took, never below the configured one.
    pub(super) fn pace(&self, listing: Duration) {
        let threshold = self
            .debounce_threshold
            .max(listing.saturating_mul(LISTING_COST_FACTOR));
        // On the UI thread, which a watcher thread that panicked holding the
        // lock must not take down.
        self.debouncer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_threshold(threshold);
    }

    pub(super) fn watch_directory(&mut self, path: PathBuf) -> Result<()> {
        let Some(watcher) = &mut self.watcher else {
            return Ok(());
        };
        // Rewatch even when the path is unchanged: an external delete and
        // recreate invalidates the watch on the old inode, and a refresh has to
        // re-register on the new one. The bookkeeping is cleared before
        // unwatching and set only after a successful watch, so
        // `watched_directory` never names a path without an active watch.
        if let Some(old_path) = self.watched_directory.take()
            && let Err(e) = watcher.unwatch(old_path.as_path())
        {
            warn!("Failed to unwatch directory: {e}");
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

/// Debounces file system events into refreshes, on a background thread. An event
/// arriving after the debounce window refreshes at once; one arriving inside it
/// schedules a single delayed refresh, so a burst produces one refresh at the
/// front and one at the end rather than one per event.
fn watch_for_notify_events(
    command_tx: &Sender<Command>,
    delayed_tx: &Sender<Duration>,
    notify_rx: Receiver<std::result::Result<Event, notify::Error>>,
    debouncer: &Arc<Mutex<debounce::TimeDebouncer>>,
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
                            error!("Failed to send refresh command: {e}");
                        }
                    } else if !debouncer.has_delayed_event() {
                        let delay = debouncer.remaining(Instant::now());
                        if let Err(e) = delayed_tx.send(delay) {
                            error!("Failed to schedule delayed refresh: {e}");
                        } else {
                            debouncer.set_delayed_event();
                        }
                    }
                }
                _ => (),
            },
            Err(e) => {
                error!("File system watcher error: {e}");
                let error_command = Command::AlertError(format!(
                    "Failed to run the directory watcher in the background: {e}"
                ));
                if let Err(e) = command_tx.send(error_command) {
                    error!("Failed to send error command: {e}");
                }
            }
        }
    }
}

/// Dispatches delayed refreshes. Each queued entry carries the remaining debounce
/// delay; once slept out, the dispatch goes through the shared debouncer so it
/// counts as a trigger. `should_trigger` returns false when an event already
/// refreshed while this thread slept, making the delayed one redundant, or when
/// `pace` widened the window meanwhile, in which case it waits out the rest.
fn watch_for_delayed_commands(
    command_tx: &Sender<Command>,
    delayed_rx: &Receiver<Duration>,
    debouncer: &Arc<Mutex<debounce::TimeDebouncer>>,
) {
    while let Ok(mut delay) = delayed_rx.recv() {
        loop {
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
            let mut debouncer = debouncer.lock().unwrap();
            let now = Instant::now();
            if debouncer.should_trigger(now) {
                if let Err(e) = command_tx.send(Command::RefreshDirectory) {
                    debug!("Delayed refresh not sent, likely due to shutdown: {e}");
                }
                break;
            }
            if !debouncer.has_delayed_event() {
                break;
            }
            delay = debouncer.remaining(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// The window a refresh triggered at `at` waits out, read from the shared
    /// debouncer the way the watcher threads read it.
    fn window(watcher: &DirectoryWatcher, at: Instant) -> Duration {
        let mut debouncer = watcher.debouncer.lock().unwrap();
        assert!(debouncer.should_trigger(at));
        debouncer.remaining(at)
    }

    #[test]
    fn a_slow_listing_widens_the_refresh_window_and_a_fast_one_keeps_the_configured_one() {
        let watcher = DirectoryWatcher::try_new(500).unwrap();
        let start = Instant::now();

        watcher.pace(Duration::from_secs(1));
        assert_eq!(Duration::from_secs(4), window(&watcher, start));

        watcher.pace(Duration::from_millis(10));
        let later = start + Duration::from_mins(1);
        assert_eq!(Duration::from_millis(500), window(&watcher, later));
    }

    /// A listing that finishes while a delayed refresh waits widens the
    /// window under it. The refresh is postponed to the new end, not dropped:
    /// dropping it would leave the last change of a burst unshown.
    #[test]
    fn a_delayed_refresh_outlasts_a_window_widened_while_it_waits() {
        let debouncer = Arc::new(Mutex::new(debounce::TimeDebouncer::new(
            Duration::from_millis(100),
        )));
        {
            let mut debouncer = debouncer.lock().unwrap();
            assert!(debouncer.should_trigger(Instant::now()));
            debouncer.set_delayed_event();
        }
        let (command_tx, command_rx) = channel();
        let (delayed_tx, delayed_rx) = channel();
        let shared = Arc::clone(&debouncer);
        let handle = thread::spawn(move || {
            watch_for_delayed_commands(&command_tx, &delayed_rx, &shared);
        });

        delayed_tx.send(Duration::from_millis(100)).unwrap();
        debouncer
            .lock()
            .unwrap()
            .set_threshold(Duration::from_millis(300));

        let refresh = command_rx.recv_timeout(Duration::from_secs(2));
        drop(delayed_tx);
        handle.join().unwrap();
        assert!(matches!(refresh, Ok(Command::RefreshDirectory)));
    }

    #[test]
    fn watch_directory_tracks_only_successful_watches() {
        let temp = TempDir::new("watch");
        let dir = temp.path().to_path_buf();
        let mut watcher = DirectoryWatcher::try_new(100).unwrap();

        watcher.watch_directory(dir.clone()).unwrap();
        assert_eq!(Some(&dir), watcher.watched_directory.as_ref());

        // Re-watching the unchanged path unwatches and watches again, so it
        // must not fail on the second registration.
        watcher.watch_directory(dir.clone()).unwrap();
        assert_eq!(Some(&dir), watcher.watched_directory.as_ref());

        // A failed watch must not record its path: there is no active watch,
        // so a later return to the previous directory must re-register.
        let missing = dir.join("missing");
        assert!(watcher.watch_directory(missing).is_err());
        assert!(watcher.watched_directory.is_none());

        watcher.watch_directory(dir.clone()).unwrap();
        assert_eq!(Some(&dir), watcher.watched_directory.as_ref());
    }
}
