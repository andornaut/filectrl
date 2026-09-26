use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
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

/// The watched directory, shared with the watcher thread: an event on the
/// directory itself (removed, renamed away) leaves the watch on an inode the
/// path may no longer name, so the next watch of that path renews it.
#[derive(Default)]
struct Watched {
    path: Mutex<Option<PathBuf>>,
    stale: AtomicBool,
}

impl Watched {
    fn path(&self) -> Option<PathBuf> {
        self.path.lock().unwrap().clone()
    }

    fn set(&self, path: Option<PathBuf>) {
        *self.path.lock().unwrap() = path;
    }

    /// Marks the watch stale when `event` names the watched directory itself.
    fn note(&self, event: &Event) {
        let path = self.path.lock().unwrap();
        if path
            .as_ref()
            .is_some_and(|watched| event.paths.iter().any(|named| named == watched))
        {
            self.stale.store(true, Ordering::Relaxed);
        }
    }
}

pub struct DirectoryWatcher {
    debounce_threshold: Duration,
    /// Shared with the watcher thread, which reads the current window from it.
    debouncer: Arc<Mutex<debounce::TimeDebouncer>>,
    handle: Option<thread::JoinHandle<()>>,
    notify_rx: Option<Receiver<std::result::Result<Event, notify::Error>>>,
    watched: Arc<Watched>,
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
            watched: Arc::default(),
        })
    }

    pub fn run_once(&mut self, command_tx: &Sender<Command>) {
        // Already running, do nothing
        let Some(notify_rx) = self.notify_rx.take() else {
            return;
        };

        let command_tx = command_tx.clone();
        let debouncer = Arc::clone(&self.debouncer);
        let watched = Arc::clone(&self.watched);
        self.handle = Some(thread::spawn(move || {
            watch_for_notify_events(&command_tx, &notify_rx, &debouncer, &watched);
        }));
    }

    /// Widens the refresh window to `LISTING_COST_FACTOR` times what the last
    /// listing took, never below the configured one.
    pub(super) fn pace(&self, listing: Duration) {
        let threshold = self
            .debounce_threshold
            .max(listing.saturating_mul(LISTING_COST_FACTOR));
        self.debouncer.lock().unwrap().set_threshold(threshold);
    }

    /// Watches `path`, keeping the watch already on it unless an event on the
    /// directory itself marked it stale: an external delete and recreate
    /// leaves the watch on the old inode. Re-registering on every refresh
    /// would restart the watch (on macOS the whole FSEvents stream) for
    /// nothing. The path is cleared before unwatching and set only after a
    /// successful watch, so it never names a path without an active watch.
    pub(super) fn watch_directory(&mut self, path: PathBuf) -> Result<()> {
        if self.watched.path().as_ref() == Some(&path)
            && !self.watched.stale.swap(false, Ordering::Relaxed)
        {
            return Ok(());
        }
        self.unwatch();
        let Some(watcher) = &mut self.watcher else {
            return Ok(());
        };
        watcher.watch(path.as_path(), notify::RecursiveMode::NonRecursive)?;
        self.watched.stale.store(false, Ordering::Relaxed);
        self.watched.set(Some(path));
        Ok(())
    }
}

impl DirectoryWatcher {
    /// Drops the watch on the watched directory, if there is one.
    pub(super) fn unwatch(&mut self) {
        let path = self.watched.path.lock().unwrap().take();
        if let Some(watcher) = &mut self.watcher
            && let Some(path) = path
            && let Err(e) = watcher.unwatch(path.as_path())
        {
            warn!("Failed to unwatch directory: {e}");
        }
    }

    #[cfg(test)]
    pub(super) fn watched_directory(&self) -> Option<PathBuf> {
        self.watched.path()
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
    watched: &Watched,
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
            Ok(Ok(event)) => {
                watched.note(&event);
                // A rescan (an overflowed inotify queue, dropped FSEvents) means
                // changes went unreported, whatever the event's kind.
                if event.need_rescan()
                    || matches!(
                        event.kind,
                        notify::EventKind::Create(_)
                            | notify::EventKind::Modify(_)
                            | notify::EventKind::Remove(_)
                    )
                {
                    pending = !refresh(command_tx, debouncer);
                }
            }
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
        // The widen must land inside the window, so it is wide enough for a
        // loaded runner to reach it well before the trailing refresh fires.
        const WINDOW: Duration = Duration::from_millis(500);
        const WIDENED: Duration = Duration::from_secs(1);
        let debouncer = Arc::new(Mutex::new(debounce::TimeDebouncer::new(WINDOW)));
        let (command_tx, command_rx) = channel();
        let (notify_tx, notify_rx) = channel();
        let shared = Arc::clone(&debouncer);
        let handle = thread::spawn(move || {
            watch_for_notify_events(&command_tx, &notify_rx, &shared, &Watched::default());
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

    /// Listing the watched directory opens it, which inotify reports as an
    /// access. Refreshing on one would make every reload trigger the next.
    #[test]
    fn opening_or_closing_the_directory_does_not_refresh() {
        use notify::event::{AccessKind, AccessMode};
        let debouncer = Mutex::new(debounce::TimeDebouncer::new(Duration::ZERO));
        let (command_tx, command_rx) = channel();
        let (notify_tx, notify_rx) = channel();
        for kind in [
            AccessKind::Open(AccessMode::Any),
            AccessKind::Close(AccessMode::Read),
        ] {
            notify_tx
                .send(Ok(Event::new(notify::EventKind::Access(kind))))
                .unwrap();
        }
        drop(notify_tx);

        watch_for_notify_events(&command_tx, &notify_rx, &debouncer, &Watched::default());

        assert_eq!(0, command_rx.try_iter().count());
    }

    /// An overflowed event queue reports only that changes went unseen.
    #[test]
    fn a_rescan_refreshes() {
        use notify::event::Flag;
        let debouncer = Mutex::new(debounce::TimeDebouncer::new(Duration::ZERO));
        let (command_tx, command_rx) = channel();
        let (notify_tx, notify_rx) = channel();
        notify_tx
            .send(Ok(
                Event::new(notify::EventKind::Other).set_flag(Flag::Rescan)
            ))
            .unwrap();
        drop(notify_tx);

        watch_for_notify_events(&command_tx, &notify_rx, &debouncer, &Watched::default());

        assert!(matches!(
            command_rx.try_iter().collect::<Vec<_>>().as_slice(),
            [Command::RefreshDirectory]
        ));
    }

    /// An event on the watched directory itself marks the watch stale, and
    /// one on an entry inside it does not.
    #[test]
    fn only_an_event_on_the_directory_itself_marks_the_watch_stale() {
        let watched = Watched::default();
        watched.set(Some(PathBuf::from("/d")));
        let removed = |path: &str| {
            Event::new(notify::EventKind::Remove(notify::event::RemoveKind::Any))
                .add_path(PathBuf::from(path))
        };

        watched.note(&removed("/d/entry"));
        assert!(!watched.stale.load(Ordering::Relaxed));

        watched.note(&removed("/d"));
        assert!(watched.stale.load(Ordering::Relaxed));
    }

    #[test]
    fn watch_directory_tracks_only_successful_watches() {
        let temp = TempDir::new("watch");
        let dir = temp.path().to_path_buf();
        let mut watcher = DirectoryWatcher::try_new(100).unwrap();

        watcher.watch_directory(dir.clone()).unwrap();
        assert_eq!(Some(&dir), watcher.watched_directory().as_ref());

        // A stale watch of the unchanged path is renewed, so the second
        // registration must not fail.
        watcher.watched.stale.store(true, Ordering::Relaxed);
        watcher.watch_directory(dir.clone()).unwrap();
        assert_eq!(Some(&dir), watcher.watched_directory().as_ref());
        assert!(!watcher.watched.stale.load(Ordering::Relaxed));

        // A failed watch must not record its path: there is no active watch,
        // so a later return to the previous directory must re-register.
        let missing = dir.join("missing");
        assert!(watcher.watch_directory(missing).is_err());
        assert!(watcher.watched_directory().is_none());

        watcher.watch_directory(dir.clone()).unwrap();
        assert_eq!(Some(&dir), watcher.watched_directory().as_ref());
    }

    /// A refresh of the directory already watched keeps the watch, rather
    /// than restarting it (on macOS the whole FSEvents stream) each time.
    #[test]
    fn watching_the_watched_directory_again_keeps_its_watch() {
        let temp = TempDir::new("watch_again");
        let dir = temp.path().to_path_buf();
        let mut watcher = DirectoryWatcher::try_new(100).unwrap();
        watcher.watch_directory(dir.clone()).unwrap();
        // Without a notify watcher a re-registration clears the path and sets
        // nothing, so only keeping the existing watch leaves it recorded.
        watcher.watcher = None;

        assert!(watcher.watch_directory(dir.clone()).is_ok());
        assert_eq!(Some(&dir), watcher.watched_directory().as_ref());
    }
}
