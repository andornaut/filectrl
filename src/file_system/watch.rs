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
    /// Shared with the watcher thread, which reads the current window from it.
    debouncer: Arc<Mutex<debounce::TimeDebouncer>>,
    handle: Option<thread::JoinHandle<()>>,
    notify_rx: Option<Receiver<std::result::Result<Event, notify::Error>>>,
    watched_directory: Option<PathBuf>,
    /// Option so `Drop` can `.take()` it before joining the watcher thread.
    /// Dropping the watcher closes the notify channel sender, which unblocks
    /// the thread waiting on the receiver so it can exit.
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
            handle: None,
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

        let command_tx = command_tx.clone();
        let debouncer = Arc::clone(&self.debouncer);
        self.handle = Some(thread::spawn(move || {
            watch_for_notify_events(&command_tx, &notify_rx, &debouncer);
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
        // disconnects notify_rx, which exits watch_for_notify_events, including
        // one waiting out a debounce window. Without this, handle.join() below
        // would block forever.
        self.watcher.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Debounces file system events into refreshes, on a background thread. An event
/// arriving after the debounce window refreshes at once; one arriving inside it
/// leaves a trailing refresh pending until the window ends, so a burst produces
/// one refresh at the front and one at the end rather than one per event.
///
/// While a refresh is pending the wait for the next event times out at the end
/// of the window, read again on every pass: `pace` may have widened it
/// meanwhile, in which case the refresh waits out the rest rather than being
/// dropped. An event that refreshes at once makes a pending one redundant.
fn watch_for_notify_events(
    command_tx: &Sender<Command>,
    notify_rx: &Receiver<std::result::Result<Event, notify::Error>>,
    debouncer: &Mutex<debounce::TimeDebouncer>,
) {
    let mut pending = false;
    loop {
        let received = if pending {
            let remaining = debouncer.lock().unwrap().remaining(Instant::now());
            notify_rx.recv_timeout(remaining)
        } else {
            notify_rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
        };
        match received {
            Ok(Ok(event)) => match event.kind {
                notify::EventKind::Create(_)
                | notify::EventKind::Modify(_)
                | notify::EventKind::Remove(_) => {
                    pending = !refresh(command_tx, debouncer);
                }
                _ => (),
            },
            Ok(Err(e)) => {
                error!("File system watcher error: {e}");
                let error_command = Command::AlertError(format!(
                    "Failed to run the directory watcher in the background: {e}"
                ));
                if let Err(e) = command_tx.send(error_command) {
                    error!("Failed to send error command: {e}");
                }
            }
            Err(RecvTimeoutError::Timeout) => pending = !refresh(command_tx, debouncer),
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Sends a refresh if the debounce window allows one now, returning whether it
/// did.
fn refresh(command_tx: &Sender<Command>, debouncer: &Mutex<debounce::TimeDebouncer>) -> bool {
    if !debouncer.lock().unwrap().should_trigger(Instant::now()) {
        return false;
    }
    if let Err(e) = command_tx.send(Command::RefreshDirectory) {
        debug!("Refresh not sent, likely due to shutdown: {e}");
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    /// The window a refresh triggered at `at` waits out, read from the shared
    /// debouncer the way the watcher thread reads it.
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

    fn created() -> Event {
        Event::new(notify::EventKind::Create(notify::event::CreateKind::File))
    }

    /// A listing that finishes while a trailing refresh waits widens the
    /// window under it. The refresh is postponed to the new end, not dropped:
    /// dropping it would leave the last change of a burst unshown.
    #[test]
    fn a_delayed_refresh_outlasts_a_window_widened_while_it_waits() {
        const WINDOW: Duration = Duration::from_millis(200);
        const WIDENED: Duration = Duration::from_millis(400);
        let debouncer = Arc::new(Mutex::new(debounce::TimeDebouncer::new(WINDOW)));
        let (command_tx, command_rx) = channel();
        let (notify_tx, notify_rx) = channel();
        let shared = Arc::clone(&debouncer);
        let handle = thread::spawn(move || {
            watch_for_notify_events(&command_tx, &notify_rx, &shared);
        });
        let start = Instant::now();

        // The first event of the burst refreshes at once, and the second,
        // inside the window, leaves the trailing refresh pending.
        notify_tx.send(Ok(created())).unwrap();
        let leading = command_rx.recv_timeout(Duration::from_secs(2));
        assert!(matches!(leading, Ok(Command::RefreshDirectory)));
        notify_tx.send(Ok(created())).unwrap();
        thread::sleep(Duration::from_millis(20));
        debouncer.lock().unwrap().set_threshold(WIDENED);

        let trailing = command_rx.recv_timeout(Duration::from_secs(2));
        let waited = start.elapsed();
        drop(notify_tx);
        handle.join().unwrap();
        assert!(matches!(trailing, Ok(Command::RefreshDirectory)));
        assert!(waited >= WIDENED, "refreshed after {waited:?}");
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
