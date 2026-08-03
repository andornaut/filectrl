mod debounce;
mod handler;
pub mod open_with;
mod operations;
pub mod path_info;
mod search;
mod stream;
mod tasks;
mod watch;

use std::{
    fmt::Display,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc::Sender},
    thread,
    time::Duration,
};

use anyhow::{Result, anyhow};
use log::warn;

use self::{
    operations::{open_in, spawn_argv},
    path_info::PathInfo,
    search::Limits,
    tasks::{CancelInfo, TaskCommand},
    watch::DirectoryWatcher,
};
use crate::{
    app::config::Config,
    command::{
        Command,
        progress::{CancellationToken, Task},
        result::CommandResult,
    },
};

/// A cancellable in-flight action. Tracked in a single LIFO stack so that
/// `cancel_task` cancels whichever action (file operation or search) was
/// started most recently.
enum Cancellable {
    Task(CancelInfo),
    Search(CancellationToken),
}

pub struct FileSystem {
    /// Directory holding the bookmark symlinks, resolved from the config once
    /// so bookmark reads do not depend on the process-global `Config`.
    bookmarks_dir: PathBuf,
    buffer_max_bytes: u64,
    buffer_min_bytes: u64,
    cancellables: Vec<Cancellable>,
    command_tx: Sender<Command>,
    directory: Option<PathInfo>,
    previous_directory: Option<PathInfo>,
    /// Cancellation token for the in-flight streamed directory load, if any.
    /// Cancelled when a new load starts so stale batches don't bleed across.
    current_load: Option<CancellationToken>,
    /// The latest search's generation. `ExitedSearch` carries it, so every
    /// consumer can ignore messages from a superseded search instead of
    /// tearing down its replacement.
    current_search_generation: u64,
    /// Monotonic id stamped on each directory load and search so consumers
    /// can ignore stale `ListingBatch`es. Shared by both stream kinds so a
    /// generation is never ambiguous between them.
    next_generation: u64,
    open_directory_template: String,
    open_file_template: String,
    open_filectrl_window_template: String,
    search_max_depth: u32,
    search_max_results: u32,
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
            bookmarks_dir: config.bookmarks_dir(),
            buffer_max_bytes: config.file_system.buffer_max_bytes,
            buffer_min_bytes: config.file_system.buffer_min_bytes,
            cancellables: Vec::new(),
            command_tx,
            directory: None,
            previous_directory: None,
            current_load: None,
            current_search_generation: 0,
            next_generation: 0,
            open_directory_template: config.openers.open_directory.clone(),
            open_file_template: config.openers.open_file.clone(),
            open_filectrl_window_template: config.openers.open_filectrl_window.clone(),
            search_max_depth: config.file_system.search_max_depth,
            search_max_results: config.file_system.search_max_results,
            watcher,
        }
    }

    pub fn run_once(&mut self, directory: Option<PathBuf>) -> Result<Vec<Command>> {
        if let Some(watcher) = &mut self.watcher {
            watcher.run_once(&self.command_tx);
        }

        let mut directory = directory
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

        // Fall back to the home directory when the startup directory cannot
        // be opened: every navigation command requires a current directory,
        // so continuing without one is not an option. If home cannot be
        // opened either, exit rather than run in a broken state.
        if let Err(error) = fs::read_dir(&directory.path) {
            let _ = self.command_tx.send(Command::AlertError(format!(
                "Failed to change to directory {directory:?}: {error}"
            )));
            let home = directories::UserDirs::new()
                .map(|dirs| dirs.home_dir().to_path_buf())
                .ok_or_else(|| anyhow!("Cannot determine the home directory"))?;
            directory = PathInfo::try_from(home.as_path())
                .map_err(|error| anyhow!("Failed to read home directory {home:?}: {error}"))?;
            fs::read_dir(&directory.path)
                .map_err(|error| anyhow!("Failed to read home directory {home:?}: {error}"))?;
        }

        Ok(self.cd(directory, true).into_commands())
    }

    fn current_directory(&self) -> &PathInfo {
        self.directory
            .as_ref()
            .expect("directory is set before any navigation command")
    }

    fn go_to_parent_directory(&mut self) -> CommandResult {
        match self.current_directory().parent() {
            Some(parent) => self.cd(parent, true),
            None => CommandResult::Handled,
        }
    }

    fn go_to_previous_directory(&mut self) -> CommandResult {
        match self.previous_directory.clone() {
            Some(directory) => self.cd(directory, true),
            None => CommandResult::Handled,
        }
    }

    fn cd(&mut self, directory: PathInfo, navigate: bool) -> CommandResult {
        // Cheap readability pre-flight so we don't switch into a directory we
        // cannot open (e.g. permission denied). The full per-entry read happens
        // asynchronously in `stream_cd` below.
        if let Err(error) = fs::read_dir(&directory.path) {
            return anyhow!("Failed to change to directory {directory:?}: {error}").into();
        }

        // Track the directory we're leaving so "-" can toggle back to it.
        if navigate
            && let Some(current) = &self.directory
            && current.path != directory.path
        {
            self.previous_directory = Some(current.clone());
        }
        self.directory = Some(directory.clone());
        let path_buf = directory.path.clone();
        if let Some(watcher) = &mut self.watcher
            && let Err(e) = watcher.watch_directory(path_buf.clone())
        {
            self.send_directory_error(&path_buf, e);
        }

        // Cancel any in-flight load so its batches don't bleed into this one,
        // then start streaming the new directory's entries.
        self.cancel_current_load();
        let generation = self.bump_generation();
        let token = CancellationToken::new();
        self.current_load = Some(token.clone());
        operations::stream_cd(
            directory.clone(),
            generation,
            self.command_tx.clone(),
            token,
        );

        if navigate {
            Command::NavigatedDirectory {
                directory,
                generation,
            }
        } else {
            Command::RefreshedDirectory {
                directory,
                generation,
            }
        }
        .into()
    }

    /// The next stream generation. Shared by directory loads and searches so
    /// a generation is never ambiguous between the two.
    fn bump_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    /// Cancels the in-flight streamed directory load, if any. No-op when
    /// nothing is streaming.
    fn cancel_current_load(&mut self) {
        if let Some(token) = self.current_load.take() {
            token.cancel();
        }
    }

    /// Full search teardown (Esc / `ResetView`): cancel and drop every search
    /// entry. No-op if the search was already cancelled via `cancel_task`.
    /// The cancelled thread still emits one final `ExitedSearch`: when a newer
    /// search has superseded it, the stale generation makes every consumer
    /// ignore it; otherwise it arrives with the current generation and the
    /// handlers process it as a no-op (entry and notice state already cleared).
    fn cancel_search(&mut self) {
        self.cancellables.retain(|c| match c {
            Cancellable::Search(token) => {
                token.cancel();
                false
            }
            Cancellable::Task(_) => true,
        });
    }

    /// Handles an `ExitedSearch` from a search thread. Only the current
    /// search's exit drops its entry; exits from superseded searches are
    /// ignored.
    fn on_search_exited(&mut self, generation: u64) {
        if generation == self.current_search_generation {
            self.cancel_search();
        }
    }

    fn cancel_most_recent_task(&mut self) -> CommandResult {
        // LIFO across file operations and search: the keypress always targets
        // the most recent entry and never falls through to an older one,
        // which could cancel work the user did not aim at.
        let Some(cancellable) = self.cancellables.pop() else {
            return Command::AlertWarn("No active task to cancel".into()).into();
        };
        match cancellable {
            Cancellable::Task(info) => {
                // A task that can no longer be cancelled stays on the stack
                // until its terminal Progress prunes it, and the user is told
                // nothing happened. `uncancellable` covers two cases: a task
                // still finishing a stage that cannot be interrupted, and one
                // already finished whose terminal Progress is still in flight.
                // The "Cannot cancel" wording fits the former; for the latter
                // (a sub-frame race) it is momentarily imprecise.
                if info.uncancellable.load(Ordering::Relaxed) {
                    let message = format!("Cannot cancel: {}", info.kind.message());
                    self.cancellables.push(Cancellable::Task(info));
                    return Command::AlertInfo(message).into();
                }
                info.token.cancel();
                Command::AlertInfo(format!("Cancelled: {}", info.kind.message())).into()
            }
            Cancellable::Search(token) => {
                // A search that already finished cancels its own token on
                // exit (see `run_search`). Keep the entry (its in-flight
                // ExitedSearch drops it) and stay silent: the notice
                // resolves momentarily, unlike a seconds-long task stage.
                if token.is_cancelled() {
                    self.cancellables.push(Cancellable::Search(token));
                    return CommandResult::Handled;
                }
                token.cancel();
                // Non-destructive: keep streamed results and the notice;
                // NoticesView relabels it to "Cancelled: [Searching] <query>".
                Command::CancelSearch.into()
            }
        }
    }

    fn check_progress_for_error(&mut self, task: &Task) -> CommandResult {
        if task.is_terminal() {
            self.cancellables.retain(|c| match c {
                Cancellable::Task(info) => info.id != task.id(),
                Cancellable::Search(_) => true,
            });
        }
        if task.is_cancelled() {
            return CommandResult::Handled;
        }
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
                    open_in(&path, &self.open_file_template, self.command_tx.clone()).into()
                }
            }
            Err(err) => err.into(),
        }
    }

    fn open_current_directory(&self) -> CommandResult {
        open_in(
            self.current_directory(),
            &self.open_directory_template,
            self.command_tx.clone(),
        )
        .into()
    }

    fn open_new_window(&self) -> CommandResult {
        open_in(
            self.current_directory(),
            &self.open_filectrl_window_template,
            self.command_tx.clone(),
        )
        .into()
    }

    /// Launch an application the "open with" picker already resolved into an
    /// argv, so no template substitution or shell is involved here.
    fn open_with(&self, working_dir: Option<&Path>, label: &str, argv: &[String]) -> CommandResult {
        spawn_argv(working_dir, label, argv, self.command_tx.clone()).into()
    }

    fn chmod(&mut self, paths: &[PathInfo], mode_str: &str) -> CommandResult {
        let Some(mode) = parse_octal_mode(mode_str) else {
            return anyhow!("Invalid octal mode: {mode_str:?}").into();
        };
        // Return the failures alongside the refresh instead of sending them
        // separately, so they are ordered against it rather than racing the
        // channel drain.
        let mut commands: Vec<Command> = paths
            .iter()
            .filter_map(|path| {
                operations::chmod(path, mode)
                    .err()
                    .map(|error| anyhow!("Failed to chmod {path:?} to {mode_str}: {error}").into())
            })
            .collect();
        commands.extend(self.refresh().into_commands());
        commands.into()
    }

    fn add_bookmark(&mut self, target: &PathInfo, name: &str) -> CommandResult {
        match operations::add_bookmark(&self.bookmarks_dir, target, name) {
            Err(error) => Command::AlertError(error.to_string()).into(),
            Ok(_) => Command::AlertInfo(format!("Bookmark {name:?} added")).into(),
        }
    }

    fn create_directory(&mut self, name: &str) -> CommandResult {
        match operations::create_directory(self.current_directory(), name) {
            Err(error) => anyhow!("Failed to create directory {name:?}: {error}").into(),
            Ok(_) => self.refresh(),
        }
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

    /// Runs each task, registering started tasks on the cancel stack. Returns
    /// the sources of the tasks that failed validation, in batch order (an
    /// empty result means every task started), together with the alerts those
    /// failures produced (e.g. destination exists). The caller broadcasts the
    /// alerts alongside its own follow-up. Started tasks send their initial
    /// progress snapshot themselves, before spawning their worker thread.
    fn run_batch(
        &mut self,
        tasks: impl Iterator<Item = TaskCommand>,
    ) -> (Vec<PathInfo>, Vec<Command>) {
        let mut failed = Vec::new();
        let mut commands = Vec::new();
        for task in tasks {
            let source = task.source().clone();
            let result = task.run(
                self.command_tx.clone(),
                self.buffer_min_bytes,
                self.buffer_max_bytes,
            );
            match result.cancel_info {
                Some(cancel_info) => self.cancellables.push(Cancellable::Task(cancel_info)),
                None => failed.push(source),
            }
            commands.extend(result.command_result.into_commands());
        }
        (failed, commands)
    }

    fn search(&mut self, query: &str) -> CommandResult {
        // Backstop for a StartSearch("") that bypasses the prompt (the
        // prompt, the only producer, resolves an empty submit to
        // CancelPrompt, so this should be unreachable). An empty needle would
        // match every entry, so spawn no walk; emit the started/exited pair
        // so NoticesView and TableView drop back out of search state. This is
        // a guard, not a clean no-result search: BreadcrumbsView only leaves
        // search state on ResetView/navigation, so it is not unwound here.
        // Returning both keeps started-before-exited ordering explicit.
        if query.is_empty() {
            self.cancel_search();
            let generation = self.bump_generation();
            return vec![
                Command::SearchStarted { generation },
                Command::ExitedSearch { generation },
            ]
            .into();
        }

        // One search at a time: cancel any previous search. Its stale
        // results and exit are ignored by generation, not by timing.
        self.cancel_search();
        // Also stop the in-flight directory load: the search replaces the
        // listing, so the load's remaining work is wasted and its batches are
        // stale (their generation is superseded by the search's).
        self.cancel_current_load();
        // Stamped only here: `current_search_generation` tracks the search
        // whose token is registered below, so an `ExitedSearch` from any
        // other generation is ignored by `on_search_exited`.
        let generation = self.bump_generation();
        self.current_search_generation = generation;

        let token = CancellationToken::new();
        self.cancellables.push(Cancellable::Search(token.clone()));

        let tick_token = token.clone();
        let tick_tx = self.command_tx.clone();
        thread::spawn(move || {
            while !tick_token.is_cancelled() {
                thread::sleep(Duration::from_millis(80));
                if tick_token.is_cancelled() {
                    break;
                }
                if tick_tx.send(Command::SearchTick).is_err() {
                    break;
                }
            }
        });

        search::run_search(
            Limits {
                max_depth: self.search_max_depth,
                max_results: self.search_max_results,
            },
            self.command_tx.clone(),
            token,
            self.current_directory().clone(),
            query.to_string(),
            generation,
        );

        // Tell consumers which generation is now current, so they can ignore
        // messages from superseded searches.
        Command::SearchStarted { generation }.into()
    }

    fn send_directory_error(&self, dir: &PathBuf, error: impl Display) {
        let _ = self.command_tx.send(Command::AlertWarn(format!(
            "Failed to read directory {dir:?}: {error}"
        )));
    }
}

/// Read every entry in the bookmarks directory, creating it if absent.
/// Synchronous: one small directory of symlinks, no streaming. Returns the
/// failure message rather than a command so the caller can tell success from
/// failure before cancelling the listing the bookmarks would replace.
/// Unreadable individual entries are skipped, not fatal.
pub(super) fn read_bookmarks(dir: &Path) -> Result<Vec<PathInfo>, String> {
    if let Err(error) = fs::create_dir_all(dir) {
        return Err(format!(
            "Cannot create bookmarks directory {dir:?}: {error}"
        ));
    }
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("Cannot read bookmarks directory {dir:?}: {error}"))?;
    Ok(entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            match PathInfo::try_from(&path) {
                Ok(info) => Some(info),
                Err(error) => {
                    warn!("Skipping unreadable bookmark {path:?}: {error}");
                    None
                }
            }
        })
        .collect())
}

/// Parses a chmod-style octal mode string. Returns `None` for non-octal input
/// or values exceeding `0o7777` (the permission + setuid/setgid/sticky bits).
fn parse_octal_mode(mode_str: &str) -> Option<u32> {
    match u32::from_str_radix(mode_str, 8) {
        Ok(mode) if mode <= 0o7777 => Some(mode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{
        app::clipboard::ClipboardEntry, command::handler::CommandHandler, test_support::TempDir,
    };

    fn test_file_system(bookmarks: &TempDir, command_tx: Sender<Command>) -> FileSystem {
        FileSystem {
            // A temp path, so bookmark reads never touch the real config dir.
            bookmarks_dir: bookmarks.path().to_path_buf(),
            buffer_max_bytes: 64_000_000,
            buffer_min_bytes: 64_000,
            cancellables: Vec::new(),
            command_tx,
            directory: None,
            previous_directory: None,
            current_load: None,
            current_search_generation: 0,
            next_generation: 0,
            open_directory_template: String::new(),
            open_file_template: String::new(),
            open_filectrl_window_template: String::new(),
            search_max_depth: 20,
            search_max_results: 10_000,
            watcher: None,
        }
    }

    #[test]
    fn run_batch_reports_every_source_when_every_task_fails_validation() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        // A vanished source fails the pre-flight re-stat, so no task starts.
        let mut src = PathInfo::try_from(Path::new("/")).unwrap();
        src.path = PathBuf::from("/nonexistent/filectrl_missing.txt");
        src.display_name = "filectrl_missing.txt".to_string();
        let dest = PathInfo::try_from(std::env::temp_dir().as_path()).unwrap();

        let (failed, commands) =
            file_system.run_batch(std::iter::once(TaskCommand::Copy(src.clone(), dest)));

        assert_eq!(vec![src], failed);
        assert!(file_system.cancellables.is_empty());
        // The failure is returned for the caller to broadcast, not sent.
        assert!(matches!(commands.as_slice(), [Command::AlertError(_)]));
        assert!(rx.try_recv().is_err());
    }

    /// A populated source directory and an empty destination directory, both
    /// inside one self-removing temp directory.
    struct CopyFixture {
        _dir: TempDir,
        src: PathInfo,
        dest: PathInfo,
        missing: PathInfo,
    }

    impl CopyFixture {
        fn new(label: &str) -> Self {
            let dir = TempDir::new(label);
            let src_dir = dir.join("src");
            let dest_dir = dir.join("dest");
            fs::create_dir_all(&src_dir).unwrap();
            fs::create_dir_all(&dest_dir).unwrap();
            fs::write(src_dir.join("a.txt"), b"x").unwrap();

            // A vanished source fails the pre-flight re-stat, so its task
            // never starts.
            let mut missing = PathInfo::try_from(Path::new("/")).unwrap();
            missing.path = src_dir.join("missing.txt");
            missing.display_name = "missing.txt".to_string();

            Self {
                src: PathInfo::try_from(src_dir.join("a.txt").as_path()).unwrap(),
                dest: PathInfo::try_from(dest_dir.as_path()).unwrap(),
                missing,
                _dir: dir,
            }
        }
    }

    /// Blocks until a task reports a terminal status, so the worker thread has
    /// finished with the fixture directory before it is removed.
    fn await_terminal_task(rx: &std::sync::mpsc::Receiver<Command>) {
        loop {
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Command::Progress(task)) if task.is_terminal() => return,
                Ok(_) => {}
                Err(error) => panic!("task did not finish: {error}"),
            }
        }
    }

    #[test]
    fn run_batch_reports_no_failed_sources_when_every_task_starts() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_run_batch");

        let (failed, commands) = file_system.run_batch(std::iter::once(TaskCommand::Copy(
            fx.src.clone(),
            fx.dest.clone(),
        )));

        assert!(failed.is_empty());
        assert!(commands.is_empty());
        assert_eq!(1, file_system.cancellables.len());
        await_terminal_task(&rx);
    }

    #[test]
    fn run_batch_reports_only_the_failed_sources_on_partial_failure() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_run_batch_mixed");

        let (failed, commands) = file_system.run_batch(
            [
                TaskCommand::Copy(fx.missing.clone(), fx.dest.clone()),
                TaskCommand::Copy(fx.src.clone(), fx.dest.clone()),
            ]
            .into_iter(),
        );

        // Only the failed source is reported: the handler reduces the
        // clipboard to exactly these so a retry carries only what was not
        // pasted.
        assert_eq!(vec![fx.missing.clone()], failed);
        // The failed task's alert is returned; the started one contributes none.
        assert!(matches!(commands.as_slice(), [Command::AlertError(_)]));
        assert_eq!(1, file_system.cancellables.len());
        await_terminal_task(&rx);
    }

    #[test]
    fn copy_with_partial_failure_reduces_the_clipboard_to_the_failed_sources() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_partial_clipboard");

        let result = file_system.handle_command(&Command::Copy {
            srcs: vec![fx.missing.clone(), fx.src.clone()],
            dest: fx.dest.clone(),
        });

        // The failed source's alert rides the same broadcast as the clipboard
        // follow-up, which keeps the operation and only the failed source, so
        // a retry carries just what was not pasted.
        let commands = result.into_commands();
        let [
            Command::AlertError(_),
            Command::SetClipboardEntry(Some(ClipboardEntry::Copy(paths))),
        ] = commands.as_slice()
        else {
            panic!("expected an alert and SetClipboardEntry(Copy), got {commands:?}");
        };
        assert_eq!(&vec![fx.missing.clone()], paths);
        await_terminal_task(&rx);
    }

    #[test]
    fn read_bookmarks_creates_the_directory_and_lists_its_entries() {
        let base = TempDir::reserved("read_bookmarks_ok");
        let dir = base.join("bookmarks");

        // The directory does not exist yet; reading it creates it.
        let bookmarks = read_bookmarks(&dir).expect("expected the bookmarks to be read");
        assert!(dir.is_dir());
        assert!(bookmarks.is_empty());

        fs::write(dir.join("one"), b"").unwrap();
        fs::write(dir.join("two"), b"").unwrap();
        let mut names: Vec<String> = read_bookmarks(&dir)
            .expect("expected the bookmarks to be read")
            .iter()
            .map(|info| info.display_name.clone())
            .collect();
        names.sort();
        assert_eq!(vec!["one".to_string(), "two".to_string()], names);
    }

    #[test]
    fn read_bookmarks_reports_an_uncreatable_directory() {
        let base = TempDir::new("read_bookmarks_err");
        // A regular file cannot be a parent directory, so create_dir_all fails.
        let file = base.join("not-a-dir");
        fs::write(&file, b"").unwrap();

        let error = read_bookmarks(&file.join("bookmarks"))
            .expect_err("expected an error for an uncreatable directory");

        assert!(
            error.starts_with("Cannot create bookmarks directory"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn get_bookmarks_cancels_the_in_flight_directory_load() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let load_token = CancellationToken::new();
        file_system.current_load = Some(load_token.clone());

        let result = file_system.handle_command(&Command::GetBookmarks);

        // Load batches must not stream into the bookmarks listing. The cancel
        // is paired with the Bookmarks broadcast that replaces it: only that
        // command clears the table's loading flag, and a load cancelled
        // mid-drain sends no DirectoryListingComplete to clear it instead.
        let command = Command::try_from(result).expect("expected a derived command");
        assert!(matches!(command, Command::Bookmarks { .. }));
        assert!(load_token.is_cancelled());
        assert!(file_system.current_load.is_none());
        drop(rx);
    }

    #[test]
    fn search_cancels_the_in_flight_directory_load() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        // A fixture rather than the real temp directory: `search` spawns a
        // detached walk, and rooting it at /tmp would traverse every other
        // test's fixtures to the full production depth.
        let root = TempDir::new("fs_search_root");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        let load_token = CancellationToken::new();
        file_system.current_load = Some(load_token.clone());

        let _ = file_system.search("query");

        // A load left running would keep walking the directory for batches
        // that are already stale (the search generation supersedes theirs).
        assert!(load_token.is_cancelled());
        assert!(file_system.current_load.is_none());
        // Nothing cancels the spawned walk on drop, and an empty batch never
        // notices the closed channel, so stop it before the fixture goes away.
        file_system.cancel_search();
        drop(rx);
    }

    #[test]
    fn search_with_an_empty_query_returns_the_started_exited_pair() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.directory = Some(PathInfo::try_from(std::env::temp_dir().as_path()).unwrap());

        let result = file_system.search("");

        // No walk is spawned; the pair unwinds the consumers' search state,
        // and started must precede exited or the exit is ignored as stale.
        let commands = result.into_commands();
        let [
            Command::SearchStarted {
                generation: started,
            },
            Command::ExitedSearch { generation: exited },
        ] = commands.as_slice()
        else {
            panic!("expected a SearchStarted/ExitedSearch pair, got {commands:?}");
        };
        assert_eq!(started, exited);
        // Nothing goes out of band on the channel.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn parse_octal_mode_accepts_valid_modes() {
        assert_eq!(Some(0o644), parse_octal_mode("644"));
        assert_eq!(Some(0o755), parse_octal_mode("755"));
        assert_eq!(Some(0o0), parse_octal_mode("0"));
        assert_eq!(Some(0o7777), parse_octal_mode("7777"));
        assert_eq!(Some(0o4755), parse_octal_mode("4755"));
    }

    #[test]
    fn parse_octal_mode_rejects_out_of_range() {
        assert_eq!(None, parse_octal_mode("10000"));
        assert_eq!(None, parse_octal_mode("77777"));
    }

    #[test]
    fn parse_octal_mode_rejects_non_octal() {
        assert_eq!(None, parse_octal_mode("888"));
        assert_eq!(None, parse_octal_mode("0o644"));
        assert_eq!(None, parse_octal_mode("rwx"));
        assert_eq!(None, parse_octal_mode(""));
        assert_eq!(None, parse_octal_mode("-1"));
    }
}
