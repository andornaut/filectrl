mod conflicts;
mod debounce;
mod handler;
pub mod open_with;
mod operations;
pub(crate) use operations::{exit_cause, failure_prefix};
mod paste;
pub mod path_info;
mod search;
pub(crate) mod shell;
mod stream;
mod tasks;
mod watch;

use std::{
    env,
    ffi::OsString,
    fmt::Display,
    fs,
    path::{Path, PathBuf},
    sync::{atomic::Ordering, mpsc::Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use log::warn;

use self::{
    conflicts::same_name_refusal,
    operations::{open_in, spawn_argv},
    paste::{PasteStep, PendingPaste},
    path_info::{PathInfo, compact},
    search::Limits,
    tasks::{CancelInfo, PasteJob, TaskCommand},
    watch::DirectoryWatcher,
};
use crate::{
    app::config::Config,
    command::{
        Command, ConflictChoice, PromptAction,
        progress::{CancellationToken, Task},
        result::CommandResult,
    },
};

/// Heartbeat for a running search's loading indicator, whose position comes
/// from elapsed time.
const SEARCH_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// A cancellable in-flight action, kept in registration order.
enum Cancellable {
    /// A file operation and its batch (one paste or one delete), cancelled together.
    Task(CancelInfo, u64),
    Search(CancellationToken),
}

/// What the table lists, which decides what a refresh re-reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Shown {
    Directory,
    /// Search results, re-read by path once the walk has ended.
    Search,
    /// The bookmarks directory, watched in place of the current one.
    Bookmarks,
}

/// A file operation already told to stop, waiting for its terminal progress.
fn is_cancelled_task(cancellable: &Cancellable) -> bool {
    matches!(cancellable, Cancellable::Task(info, _) if info.token.is_cancelled())
}

/// A task in a stage that cannot be interrupted (a move removing its original).
fn is_uncancellable_task(cancellable: &Cancellable) -> bool {
    matches!(cancellable, Cancellable::Task(info, _) if info.uncancellable.load(Ordering::Relaxed))
}

pub struct FileSystem {
    bookmarks_dir: PathBuf,
    cancellables: Vec<Cancellable>,
    last_batch: u64,
    /// The batch the paste in progress starts its tasks in.
    paste_batch: u64,
    command_tx: Sender<Command>,
    directory: Option<PathInfo>,
    previous_directory: Option<PathInfo>,
    /// The in-flight directory load's generation and cancellation token.
    current_load: Option<(u64, CancellationToken)>,
    /// When the in-flight load started; paces the watcher.
    load_started: Option<Instant>,
    /// A refresh arrived during a load; it is re-issued when the load completes.
    reload_pending: bool,
    /// The latest search's generation; an `ExitedSearch` from any other is ignored.
    current_search_generation: u64,
    /// Generation counter shared by directory loads and searches, so stale
    /// `ListingBatch`es can be ignored.
    next_generation: u64,
    open_directory_template: String,
    open_file_template: String,
    open_filectrl_window_template: String,
    /// The paste awaiting a conflict answer. Only the paste queue prompts, never a worker.
    pending_paste: Option<PendingPaste>,
    search_max_depth: u32,
    search_max_results: u32,
    /// The current search's results, by path, for a refresh to re-read.
    search_results: Vec<PathBuf>,
    shown: Shown,
    watcher: Option<DirectoryWatcher>,
}

impl FileSystem {
    pub fn new(config: &Config, command_tx: Sender<Command>) -> Self {
        let watcher = DirectoryWatcher::try_new(config.file_system.refresh_debounce_milliseconds)
            .inspect_err(|e| {
                warn!("Failed to start the directory watcher: {e}");
                let _ = command_tx.send(Command::AlertWarn(format!(
                    "Failed to start the directory watcher: {e}"
                )));
            })
            .ok();
        Self {
            bookmarks_dir: config.bookmarks_dir(),
            cancellables: Vec::new(),
            last_batch: 0,
            paste_batch: 0,
            command_tx,
            directory: None,
            previous_directory: None,
            current_load: None,
            load_started: None,
            reload_pending: false,
            current_search_generation: 0,
            next_generation: 0,
            open_directory_template: config.openers.open_directory.clone(),
            open_file_template: config.openers.open_file.clone(),
            open_filectrl_window_template: config.openers.open_filectrl_window.clone(),
            pending_paste: None,
            search_max_depth: config.file_system.search_max_depth,
            search_max_results: config.file_system.search_max_results,
            search_results: Vec::new(),
            shown: Shown::Directory,
            watcher,
        }
    }

    pub fn run_once(&mut self, directory: Option<PathBuf>) -> Result<Vec<Command>> {
        if let Some(watcher) = &mut self.watcher {
            watcher.run_once(&self.command_tx);
        }

        // Already canonical: the argument is canonicalized when validated, and
        // `getcwd` returns a canonical path.
        let directory = directory
            .or_else(|| {
                env::current_dir()
                    .inspect_err(|error| {
                        let _ = self.command_tx.send(Command::AlertError(format!(
                            "Failed to read the current directory: {error}"
                        )));
                    })
                    .ok()
            })
            .and_then(|path| {
                PathInfo::try_from(&path)
                    .inspect_err(|error| self.send_directory_error(&path, error))
                    .ok()
            })
            .filter(|directory| {
                fs::read_dir(&directory.path)
                    .inspect_err(|error| {
                        let _ = self.command_tx.send(Command::AlertError(format!(
                            "Failed to change to directory {}: {error}",
                            compact(&directory.path)
                        )));
                    })
                    .is_ok()
            });

        // Every navigation command needs a current directory: fall back to home, and
        // fail if that cannot be opened either.
        let directory = directory.map_or_else(home_directory, Ok)?;

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
        // Refuse to switch into a directory that cannot be opened.
        if let Err(error) = fs::read_dir(&directory.path) {
            let verb = if navigate { "change to" } else { "read" };
            return anyhow!(
                "Failed to {verb} directory {}: {error}",
                compact(&directory.path)
            )
            .into();
        }
        // Track the directory we're leaving so "-" can toggle back to it.
        if navigate
            && let Some(current) = &self.directory
            && current.path != directory.path
        {
            self.previous_directory = Some(current.clone());
        }
        // A navigation replaces the search results; a reload leaves any search alone.
        if navigate {
            self.cancel_search();
            self.shown = Shown::Directory;
        }
        self.directory = Some(directory.clone());
        self.watch(&directory.path);

        self.cancel_current_load();
        let generation = self.bump_generation();
        let token = CancellationToken::new();
        self.current_load = Some((generation, token.clone()));
        self.load_started = Some(Instant::now());
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

    /// Watches `path` for changes, alerting when it cannot: the listing itself
    /// may load fine, and only the automatic refresh is lost.
    fn watch(&mut self, path: &Path) {
        if let Some(watcher) = &mut self.watcher
            && let Err(e) = watcher.watch_directory(path.to_path_buf())
        {
            let _ = self.command_tx.send(Command::AlertWarn(format!(
                "Failed to watch directory {}: {e}",
                compact(path)
            )));
        }
    }

    /// Reads the bookmarks and watches their directory while they are shown,
    /// so one added or removed elsewhere reloads the view.
    fn show_bookmarks(&mut self) -> CommandResult {
        match read_bookmarks(&self.bookmarks_dir) {
            // Cancel only once the listing is known to replace them: a failed read sends
            // no Bookmarks command, which would leave the table's loading flag set.
            Ok(bookmarks) => {
                self.cancel_search();
                self.cancel_current_load();
                self.shown = Shown::Bookmarks;
                let dir = self.bookmarks_dir.clone();
                self.watch(&dir);
                Command::Bookmarks { bookmarks }.into()
            }
            Err(message) => Command::AlertError(message).into(),
        }
    }

    fn searching(&mut self) {
        self.shown = Shown::Search;
        self.search_results.clear();
    }

    fn search_batch(&mut self, items: &[PathInfo], generation: u64) {
        if self.shown == Shown::Search && generation == self.current_search_generation {
            self.search_results
                .extend(items.iter().map(|item| item.path.clone()));
        }
    }

    /// Re-reads an ended search's results by path, off the calling thread, dropping
    /// those that are gone. While the walk runs, its results are current.
    fn refresh_search(&mut self) -> CommandResult {
        if self
            .cancellables
            .iter()
            .any(|c| matches!(c, Cancellable::Search(_)))
        {
            return CommandResult::Handled;
        }
        let paths = self.search_results.clone();
        let generation = self.current_search_generation;
        let tx = self.command_tx.clone();
        thread::spawn(move || {
            let items = paths
                .iter()
                .filter_map(|path| PathInfo::try_from(path).ok())
                .collect();
            let _ = tx.send(Command::SearchResultsRefreshed { items, generation });
        });
        CommandResult::Handled
    }

    fn bump_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    fn cancel_current_load(&mut self) {
        self.reload_pending = false;
        if let Some((_, token)) = self.current_load.take() {
            token.cancel();
        }
    }

    /// Clears the finished load and re-issues a refresh that arrived during it.
    fn on_listing_complete(&mut self, generation: u64) -> CommandResult {
        if self
            .current_load
            .as_ref()
            .is_none_or(|(current, _)| *current != generation)
        {
            return CommandResult::NotHandled;
        }
        self.current_load = None;
        if let (Some(started), Some(watcher)) = (self.load_started.take(), &self.watcher) {
            watcher.pace(started.elapsed());
        }
        if std::mem::take(&mut self.reload_pending) {
            return self.refresh();
        }
        CommandResult::NotHandled
    }

    /// Full search teardown (Esc / `ResetView`). The thread's final `ExitedSearch`
    /// is then ignored.
    fn cancel_search(&mut self) {
        self.cancellables.retain(|c| match c {
            Cancellable::Search(token) => {
                token.cancel();
                false
            }
            Cancellable::Task(..) => true,
        });
    }

    /// Drops the current search's entry; exits of superseded searches are ignored.
    fn on_search_exited(&mut self, generation: u64) {
        if generation == self.current_search_generation {
            self.cancel_search();
        }
    }

    /// Running or queued file operations. A cancelled task counts until its
    /// terminal progress arrives.
    pub(crate) fn task_count(&self) -> usize {
        self.cancellables
            .iter()
            .filter(|cancellable| matches!(cancellable, Cancellable::Task(..)))
            .count()
    }

    /// The entry a cancel keypress targets: the newest search if it is newest,
    /// otherwise the oldest cancellable task of the newest batch holding one.
    /// Cancelled tasks are never targeted; if only uncancellable ones remain, the
    /// oldest is returned so the key can say why nothing was cancelled.
    fn cancel_target(&self) -> Option<usize> {
        let newest = self
            .cancellables
            .iter()
            .rposition(|cancellable| !is_cancelled_task(cancellable))?;
        if let Cancellable::Search(_) = self.cancellables[newest] {
            return Some(newest);
        }
        let live = |cancellable: &Cancellable| {
            matches!(cancellable, Cancellable::Task(..)) && !is_cancelled_task(cancellable)
        };
        let cancellable_batch =
            self.cancellables
                .iter()
                .rev()
                .find_map(|cancellable| match cancellable {
                    Cancellable::Task(_, batch)
                        if live(cancellable) && !is_uncancellable_task(cancellable) =>
                    {
                        Some(*batch)
                    }
                    _ => None,
                });
        self.cancellables
            .iter()
            .position(|cancellable| match (cancellable, cancellable_batch) {
                (Cancellable::Task(_, batch), Some(target)) => {
                    *batch == target && live(cancellable) && !is_uncancellable_task(cancellable)
                }
                (Cancellable::Task(..), None) => live(cancellable),
                (Cancellable::Search(_), _) => false,
            })
    }

    fn cancel_most_recent_task(&mut self) -> CommandResult {
        let Some(index) = self.cancel_target() else {
            return Command::AlertWarn("No active task to cancel".into()).into();
        };
        match &self.cancellables[index] {
            Cancellable::Task(info, batch) => {
                // `uncancellable` also covers a finished task whose terminal progress is in flight.
                if info.uncancellable.load(Ordering::Relaxed) {
                    return Command::AlertInfo(format!("Cannot cancel: {}", info.kind.message()))
                        .into();
                }
                let (batch, message) = (*batch, info.kind.message());
                // A queued task with a cancelled token ends as cancelled without running.
                let mut cancelled = 0;
                for cancellable in &self.cancellables {
                    if let Cancellable::Task(info, of) = cancellable
                        && *of == batch
                        && !info.token.is_cancelled()
                        && !info.uncancellable.load(Ordering::Relaxed)
                    {
                        info.token.cancel();
                        cancelled += 1;
                    }
                }
                // The worker can mark the target uncancellable after the check
                // above, leaving nothing cancelled.
                if cancelled == 0 {
                    return Command::AlertInfo(format!("Cannot cancel: {message}")).into();
                }
                let more = match cancelled - 1 {
                    0 => String::new(),
                    1 => " and 1 more task".to_string(),
                    n => format!(" and {n} more tasks"),
                };
                Command::AlertInfo(format!("Cancelled: {message}{more}")).into()
            }
            Cancellable::Search(token) => {
                // A finished search cancels its own token on exit, and its
                // `ExitedSearch` drops the entry.
                if token.is_cancelled() {
                    return CommandResult::Handled;
                }
                token.cancel();
                self.cancellables.remove(index);
                // Keeps the streamed results; NoticesView relabels the notice as cancelled.
                Command::CancelSearch.into()
            }
        }
    }

    fn check_progress_for_error(&mut self, task: &Task) -> CommandResult {
        if task.is_terminal() {
            self.cancellables.retain(|c| match c {
                Cancellable::Task(info, _) => info.id != task.id(),
                Cancellable::Search(_) => true,
            });
            // The watcher sees only the directory searched, not the results
            // below it that the task may have removed, renamed or changed.
            if self.shown == Shown::Search {
                self.refresh_search();
            }
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
        // Name the path: a broken symlink would otherwise surface a bare io error.
        match fs::canonicalize(&path.path)
            .map_err(|error| anyhow!("Failed to open {}: {error}", compact(&path.path)))
            .and_then(|path| PathInfo::try_from(&path))
        {
            Ok(path) => {
                if path.is_directory() {
                    self.cd(path, true)
                } else {
                    open_in(
                        "open_file",
                        &path,
                        &self.open_file_template,
                        self.command_tx.clone(),
                    )
                    .into()
                }
            }
            Err(err) => err.into(),
        }
    }

    fn open_current_directory(&self) -> CommandResult {
        open_in(
            "open_directory",
            self.current_directory(),
            &self.open_directory_template,
            self.command_tx.clone(),
        )
        .into()
    }

    fn open_new_window(&self) -> CommandResult {
        open_in(
            "open_filectrl_window",
            self.current_directory(),
            &self.open_filectrl_window_template,
            self.command_tx.clone(),
        )
        .into()
    }

    /// Launches an argv the "open with" picker already resolved, without a shell.
    fn open_with(
        &self,
        working_dir: Option<&Path>,
        label: &str,
        path: &Path,
        argv: &[OsString],
    ) -> CommandResult {
        spawn_argv(working_dir, label, path, argv, self.command_tx.clone()).into()
    }

    fn chmod(&mut self, paths: &[PathInfo], mode_str: &str) -> CommandResult {
        let mode = match chmod_mode(paths, mode_str) {
            Ok(mode) => mode,
            Err(error) => return error.into(),
        };
        // Returned with the refresh rather than sent, so they are ordered against it.
        let mut commands: Vec<Command> = paths
            .iter()
            .filter_map(|path| operations::chmod(path, mode).err().map(Into::into))
            .collect();
        commands.extend(self.refresh().into_commands());
        commands.into()
    }

    fn add_bookmark(&mut self, target: &PathInfo, name: &str) -> CommandResult {
        match operations::add_bookmark(&self.bookmarks_dir, target, name) {
            Err(error) => Command::AlertError(error.to_string()).into(),
            Ok(()) => Command::AlertInfo(format!("Bookmark {name:?} added")).into(),
        }
    }

    fn create_directory(&mut self, name: &str) -> CommandResult {
        match operations::create_directory(self.current_directory(), name) {
            Err(error) => error.into(),
            Ok(()) => self.refresh(),
        }
    }

    fn rename(&mut self, path: &PathInfo, new_basename: &str) -> CommandResult {
        match operations::rename(path, new_basename) {
            Err(error) => error.into(),
            Ok(()) => self.refresh(),
        }
    }

    fn refresh(&mut self) -> CommandResult {
        match self.shown {
            Shown::Search => return self.refresh_search(),
            Shown::Bookmarks => return self.show_bookmarks(),
            Shown::Directory => {}
        }
        // A load for this directory is streaming; restarting it under sustained churn
        // would never let it complete. The refresh is re-issued when it completes.
        if self.current_load.is_some() {
            self.reload_pending = true;
            return CommandResult::Handled;
        }
        let commands = self
            .cd(self.current_directory().clone(), false)
            .into_commands();
        if !matches!(commands.as_slice(), [Command::RefreshedDirectory { .. }])
            && let Some(watcher) = &mut self.watcher
        {
            // No longer a readable directory: stop watching, or every change there would
            // retry this reload and report it again.
            watcher.unwatch();
        }
        commands.into()
    }

    /// A new batch number; one cancel keypress stops a whole batch.
    fn next_batch(&mut self) -> u64 {
        self.last_batch += 1;
        self.last_batch
    }

    /// Runs a task in `batch`, registering it for cancel if it starts. Returns
    /// whether it started, and its alerts.
    fn run_task(&mut self, batch: u64, task: TaskCommand) -> (bool, Vec<Command>) {
        let result = task.run(self.command_tx.clone());
        let started = result.cancel_info.is_some();
        if let Some(cancel_info) = result.cancel_info {
            self.cancellables
                .push(Cancellable::Task(cancel_info, batch));
        }
        (started, result.command_result.into_commands())
    }

    /// Starts a paste. Sources run one at a time so that a name already taken
    /// in the destination can be answered for before the next source starts.
    fn start_paste(&mut self, is_move: bool, srcs: &[PathInfo], dest: &PathInfo) -> CommandResult {
        // The conflict prompt captures keys while open, so no paste is waiting.
        debug_assert!(self.pending_paste.is_none(), "a paste is still asking");
        self.pending_paste = Some(PendingPaste::new(is_move, dest, srcs));
        self.paste_batch = self.next_batch();
        self.advance_paste()
    }

    /// Runs queued sources until one needs a conflict answer or the batch is done.
    fn advance_paste(&mut self) -> CommandResult {
        let Some(mut pending) = self.pending_paste.take() else {
            return CommandResult::Handled;
        };
        let mut commands = Vec::new();
        while let Some(src) = pending.remaining.front().cloned() {
            if pending.is_claimed(&src) {
                pending.remaining.pop_front();
                commands.push(Command::AlertError(same_name_refusal(
                    pending.is_move,
                    &src.path,
                    &pending.dest.path,
                )));
                pending.failed.push(src);
                continue;
            }
            match pending.meet(&src) {
                PasteStep::Ask { can_overwrite } => {
                    commands.push(Command::OpenPrompt(PromptAction::Conflict {
                        name: src.display_name.clone(),
                        can_overwrite,
                    }));
                    // The answer pops the source.
                    self.pending_paste = Some(pending);
                    return commands.into();
                }
                PasteStep::Skip => {
                    pending.remaining.pop_front();
                }
                PasteStep::Run { overwrite } => {
                    pending.remaining.pop_front();
                    commands.extend(self.run_paste_task(&mut pending, src, overwrite));
                }
            }
        }
        commands.extend(pending.clipboard_follow_up());
        commands.into()
    }

    /// Applies a conflict answer to the front source, then continues.
    fn resolve_conflict(&mut self, choice: ConflictChoice) -> CommandResult {
        let Some(mut pending) = self.pending_paste.take() else {
            return CommandResult::Handled;
        };
        let Some(src) = pending.remaining.pop_front() else {
            return CommandResult::Handled;
        };
        let mut commands = Vec::new();
        if pending.answer(choice) {
            commands.extend(self.run_paste_task(&mut pending, src, true));
        }
        self.pending_paste = Some(pending);
        commands.extend(self.advance_paste().into_commands());
        commands.into()
    }

    /// Abandons a paste whose conflict prompt was dismissed: unreached sources
    /// rejoin the failures in the clipboard, skipped ones do not. Does nothing
    /// when no paste is waiting.
    fn cancel_paste(&mut self) -> CommandResult {
        let Some(mut pending) = self.pending_paste.take() else {
            return CommandResult::NotHandled;
        };

        let remaining: Vec<PathInfo> = pending.remaining.drain(..).collect();
        pending.failed.extend(remaining);
        pending
            .clipboard_follow_up()
            .map_or(CommandResult::NotHandled, Into::into)
    }

    /// Runs one source of a paste, replacing what holds its name when
    /// `overwrite`. The name is claimed only once the task starts, since a
    /// source that fails validation writes nothing.
    fn run_paste_task(
        &mut self,
        pending: &mut PendingPaste,
        src: PathInfo,
        overwrite: bool,
    ) -> Vec<Command> {
        let (started, commands) = self.run_task(
            self.paste_batch,
            TaskCommand::paste(PasteJob {
                is_move: pending.is_move,
                overwrite,
                dest: pending.dest.clone(),
                source: src.clone(),
            }),
        );
        if started {
            pending.started += 1;
            pending.claim(&src);
        } else {
            pending.failed.push(src);
        }
        commands
    }

    fn search(&mut self, query: &str) -> CommandResult {
        // One search at a time; a stale search's messages are ignored by generation.
        self.cancel_search();
        // The search replaces the listing, so the directory load is stale.
        self.cancel_current_load();
        let generation = self.bump_generation();
        self.current_search_generation = generation;
        self.searching();

        let token = CancellationToken::new();
        self.cancellables.push(Cancellable::Search(token.clone()));

        let tick_token = token.clone();
        let tick_tx = self.command_tx.clone();
        thread::spawn(move || {
            while !tick_token.is_cancelled() {
                thread::sleep(SEARCH_TICK_INTERVAL);
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

        Command::SearchStarted { generation }.into()
    }

    fn send_directory_error(&self, dir: &Path, error: impl Display) {
        let _ = self.command_tx.send(Command::AlertWarn(format!(
            "Failed to read directory {}: {error}",
            compact(dir)
        )));
    }
}

/// The home directory, once it is known to be readable.
pub(crate) fn home_directory() -> Result<PathInfo> {
    let home = directories::UserDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .ok_or_else(|| anyhow!("Cannot determine the home directory"))?;
    let directory = PathInfo::try_from(home.as_path())
        .map_err(|error| anyhow!("Failed to read home directory {}: {error}", compact(&home)))?;
    fs::read_dir(&directory.path)
        .map_err(|error| anyhow!("Failed to read home directory {}: {error}", compact(&home)))?;
    Ok(directory)
}

/// Reads the bookmarks directory, creating it if absent. Unreadable entries are
/// skipped.
pub(super) fn read_bookmarks(dir: &Path) -> Result<Vec<PathInfo>, String> {
    if let Err(error) = fs::create_dir_all(dir) {
        return Err(format!(
            "Failed to create bookmarks directory {}: {error}",
            compact(dir)
        ));
    }
    let entries = fs::read_dir(dir).map_err(|error| {
        format!(
            "Failed to read bookmarks directory {}: {error}",
            compact(dir)
        )
    })?;
    Ok(entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            match PathInfo::try_from(&path) {
                Ok(info) => Some(info),
                Err(error) => {
                    warn!("Skipping unreadable bookmark {}: {error}", path.display());
                    None
                }
            }
        })
        .collect())
}

/// The mode a chmod of `paths` to `mode_str` sets, or the refusal. The chmod
/// prompt checks it before closing, so a typo can be corrected.
pub(crate) fn chmod_mode(paths: &[PathInfo], mode_str: &str) -> Result<u32> {
    parse_octal_mode(mode_str).ok_or_else(|| {
        let object = match paths {
            [path] => compact(&path.path).to_string(),
            _ => format!("{} items", paths.len()),
        };
        anyhow!("Cannot chmod {object}: {mode_str:?} is not an octal mode")
    })
}

/// Parses an octal mode up to `0o7777`. Digits only: `from_str_radix` accepts a
/// leading `+`, which `chmod` reads as symbolic.
fn parse_octal_mode(mode_str: &str) -> Option<u32> {
    if !mode_str.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match u32::from_str_radix(mode_str, 8) {
        Ok(mode) if mode <= 0o7777 => Some(mode),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use test_case::test_case;

    use super::*;
    use crate::{
        app::clipboard::ClipboardEntry, command::handler::CommandHandler, test_support::TempDir,
    };

    fn test_file_system(bookmarks: &TempDir, command_tx: Sender<Command>) -> FileSystem {
        FileSystem {
            bookmarks_dir: bookmarks.path().to_path_buf(),
            cancellables: Vec::new(),
            last_batch: 0,
            paste_batch: 0,
            command_tx,
            directory: None,
            previous_directory: None,
            current_load: None,
            load_started: None,
            reload_pending: false,
            current_search_generation: 0,
            next_generation: 0,
            open_directory_template: String::new(),
            open_file_template: String::new(),
            open_filectrl_window_template: String::new(),
            pending_paste: None,
            search_max_depth: 20,
            search_max_results: 10_000,
            search_results: Vec::new(),
            shown: Shown::Directory,
            watcher: None,
        }
    }

    /// A source directory holding `a.txt` and `b.txt`, and an empty destination
    /// directory, both inside one self-removing temp directory.
    pub(super) struct CopyFixture {
        _dir: TempDir,
        pub(super) src: PathInfo,
        pub(super) other: PathInfo,
        pub(super) dest: PathInfo,
        pub(super) missing: PathInfo,
    }

    impl CopyFixture {
        pub(super) fn new(label: &str) -> Self {
            let dir = TempDir::new(label);
            let src_dir = dir.join("src");
            let dest_dir = dir.join("dest");
            fs::create_dir_all(&src_dir).unwrap();
            fs::create_dir_all(&dest_dir).unwrap();
            fs::write(src_dir.join("a.txt"), b"src").unwrap();
            fs::write(src_dir.join("b.txt"), b"src").unwrap();

            // A vanished source fails the pre-flight re-stat.
            let mut missing = PathInfo::try_from(Path::new("/")).unwrap();
            missing.path = src_dir.join("missing.txt");
            missing.display_name = "missing.txt".to_string();

            Self {
                src: PathInfo::try_from(src_dir.join("a.txt").as_path()).unwrap(),
                other: PathInfo::try_from(src_dir.join("b.txt").as_path()).unwrap(),
                dest: PathInfo::try_from(dest_dir.as_path()).unwrap(),
                missing,
                _dir: dir,
            }
        }

        pub(super) fn occupy(&self, name: &str) {
            fs::write(self.dest.path.join(name), b"dest").unwrap();
        }

        pub(super) fn occupy_with_directory(&self, name: &str) {
            fs::create_dir_all(self.dest.path.join(name)).unwrap();
        }

        fn pasted(&self, name: &str) -> Vec<u8> {
            fs::read(self.dest.path.join(name)).unwrap()
        }
    }

    /// The conflict prompt in `commands`, or a panic naming what was found.
    fn conflict_prompt(commands: &[Command]) -> (&str, bool) {
        commands
            .iter()
            .find_map(|command| match command {
                Command::OpenPrompt(PromptAction::Conflict {
                    name,
                    can_overwrite,
                }) => Some((name.as_str(), *can_overwrite)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("expected a conflict prompt, got {commands:?}"))
    }

    #[test]
    fn a_paste_where_nothing_starts_leaves_the_clipboard_alone() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_paste_all_failed");

        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.missing.clone()],
                dest: fx.dest.clone(),
            })
            .into_commands();

        assert!(
            matches!(commands.as_slice(), [Command::AlertError(_)]),
            "{commands:?}"
        );
        assert!(file_system.cancellables.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_clean_paste_clears_the_clipboard() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_paste_clean");

        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.src.clone()],
                dest: fx.dest.clone(),
            })
            .into_commands();

        assert!(
            matches!(commands.as_slice(), [Command::SetClipboardEntry(None)]),
            "{commands:?}"
        );
        assert_eq!(1, file_system.cancellables.len());
        tasks::await_end(&rx);
    }

    #[test]
    fn a_partial_paste_reduces_the_clipboard_to_what_was_not_pasted() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_partial_clipboard");

        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.missing.clone(), fx.src.clone()],
                dest: fx.dest.clone(),
            })
            .into_commands();

        let [
            Command::AlertError(_),
            Command::SetClipboardEntry(Some(ClipboardEntry::Copy(paths))),
        ] = commands.as_slice()
        else {
            panic!("expected an alert and SetClipboardEntry(Copy), got {commands:?}");
        };
        assert_eq!(&vec![fx.missing.clone()], paths);
        tasks::await_end(&rx);
    }

    #[test]
    fn a_taken_name_opens_the_conflict_prompt_before_anything_runs() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_conflict_prompt");
        fx.occupy("a.txt");

        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.src.clone()],
                dest: fx.dest.clone(),
            })
            .into_commands();

        assert_eq!(("a.txt", true), conflict_prompt(&commands));
        assert!(file_system.cancellables.is_empty());
        assert_eq!(b"dest".to_vec(), fx.pasted("a.txt"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn overwrite_replaces_the_existing_destination() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_conflict_overwrite");
        fx.occupy("a.txt");
        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone()],
            dest: fx.dest.clone(),
        });

        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::Overwrite))
            .into_commands();

        assert!(
            matches!(commands.as_slice(), [Command::SetClipboardEntry(None)]),
            "{commands:?}"
        );
        tasks::await_end(&rx);
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
    }

    #[test]
    fn overwrite_all_replaces_a_later_collision_without_asking() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_conflict_overwrite_all");
        fx.occupy("a.txt");
        fx.occupy("b.txt");
        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.src.clone(), fx.other.clone()],
                dest: fx.dest.clone(),
            })
            .into_commands();
        assert_eq!(("a.txt", true), conflict_prompt(&commands));

        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::OverwriteAll))
            .into_commands();

        assert_eq!(vec![Command::SetClipboardEntry(None)], commands);
        assert_eq!(None, tasks::await_end(&rx).error_message());
        assert_eq!(None, tasks::await_end(&rx).error_message());
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"src".to_vec(), fx.pasted("b.txt"));
    }

    #[test]
    fn a_directory_moved_onto_a_file_is_offered_only_a_skip() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_move_dir_over_file");
        let src_dir = fx.src.path.parent().unwrap().join("adir");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("inner.txt"), b"src").unwrap();
        let src = PathInfo::try_from(src_dir.as_path()).unwrap();
        fs::write(fx.dest.path.join("adir"), b"dest").unwrap();

        let commands = file_system
            .handle_command(&Command::Move {
                srcs: vec![src],
                dest: fx.dest.clone(),
            })
            .into_commands();

        // A directory never replaces a file, like `cp -R` and `mv`.
        assert!(
            matches!(
                commands.as_slice(),
                [Command::OpenPrompt(PromptAction::Conflict {
                    can_overwrite: false,
                    ..
                })]
            ),
            "{commands:?}"
        );

        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::Overwrite))
            .into_commands();
        assert_eq!(
            Some(&Command::AlertError(format!(
                "Cannot move {} into {}: a directory never replaces what holds its name",
                compact(&src_dir),
                compact(&fx.dest.path)
            ))),
            commands.first()
        );
        assert_eq!(
            b"dest".to_vec(),
            fs::read(fx.dest.path.join("adir")).unwrap()
        );
        assert!(src_dir.join("inner.txt").exists());
    }

    #[test]
    fn skip_leaves_the_existing_destination_and_moves_on() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_conflict_skip");
        fx.occupy("a.txt");
        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone(), fx.other.clone()],
            dest: fx.dest.clone(),
        });

        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::Skip))
            .into_commands();

        // Skipping is not a failure, so the clipboard is cleared.
        assert!(
            matches!(commands.as_slice(), [Command::SetClipboardEntry(None)]),
            "{commands:?}"
        );
        tasks::await_end(&rx);
        assert_eq!(b"dest".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"src".to_vec(), fx.pasted("b.txt"));
    }

    #[test]
    fn skip_all_answers_every_later_collision_without_prompting_again() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_conflict_skip_all");
        fx.occupy("a.txt");
        fx.occupy("b.txt");
        let free = fx.src.path.parent().unwrap().join("c.txt");
        fs::write(&free, b"src").unwrap();
        file_system.handle_command(&Command::Copy {
            srcs: vec![
                PathInfo::try_from(free.as_path()).unwrap(),
                fx.src.clone(),
                fx.other.clone(),
            ],
            dest: fx.dest.clone(),
        });

        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::SkipAll))
            .into_commands();

        assert!(
            matches!(commands.as_slice(), [Command::SetClipboardEntry(None)]),
            "{commands:?}"
        );
        assert!(file_system.pending_paste.is_none());
        tasks::await_end(&rx);
        assert_eq!(b"src".to_vec(), fx.pasted("c.txt"));
        assert_eq!(b"dest".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"dest".to_vec(), fx.pasted("b.txt"));
    }

    #[test]
    fn dismissing_the_prompt_returns_the_unreached_sources_to_the_clipboard() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_conflict_cancel");
        fx.occupy("b.txt");
        // Only the second source collides.
        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone(), fx.other.clone()],
            dest: fx.dest.clone(),
        });

        let commands = file_system
            .handle_command(&Command::CancelPrompt)
            .into_commands();

        let [Command::SetClipboardEntry(Some(ClipboardEntry::Copy(paths)))] = commands.as_slice()
        else {
            panic!("expected SetClipboardEntry(Copy), got {commands:?}");
        };
        assert_eq!(&vec![fx.other.clone()], paths);
        tasks::await_end(&rx);
        assert_eq!(b"dest".to_vec(), fx.pasted("b.txt"));
    }

    /// Builds a cancel stack from a compact description: `t` is a file
    /// operation, `x` one already cancelled and still unwinding, `u` one that
    /// can no longer be cancelled, `s` a search, in registration order; `|`
    /// starts the next batch of file operations.
    fn cancellables(kinds: &str) -> Vec<Cancellable> {
        let mut batch = 0;
        let mut stack = Vec::new();
        for kind in kinds.chars() {
            match kind {
                '|' => batch += 1,
                't' | 'x' | 'u' => {
                    let token = CancellationToken::new();
                    if kind == 'x' {
                        token.cancel();
                    }
                    let info = CancelInfo {
                        id: 0,
                        token,
                        kind: crate::command::progress::TaskKind::Delete {
                            path: String::new(),
                        },
                        uncancellable: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                            kind == 'u',
                        )),
                    };
                    stack.push(Cancellable::Task(info, batch));
                }
                _ => stack.push(Cancellable::Search(CancellationToken::new())),
            }
        }
        stack
    }

    #[test_case("" => None ; "nothing running")]
    #[test_case("t" => Some(0) ; "the only task")]
    #[test_case("ttt" => Some(0) ; "the oldest of several queued tasks")]
    #[test_case("s" => Some(0) ; "the only search")]
    #[test_case("ts" => Some(1) ; "a search started after a task")]
    #[test_case("st" => Some(1) ; "the task, not the search beneath it")]
    #[test_case("stt" => Some(1) ; "the oldest task queued after a search")]
    #[test_case("xt" => Some(1) ; "the task queued behind a cancelled one")]
    #[test_case("sx" => Some(0) ; "the search, not a cancelled task above it")]
    #[test_case("x" => None ; "only a cancelled task")]
    #[test_case("ut" => Some(1) ; "the task queued behind one that cannot be cancelled")]
    #[test_case("uu" => Some(0) ; "the oldest when none can be cancelled")]
    #[test_case("tt|tt" => Some(2) ; "the newer batch, from its oldest task")]
    #[test_case("tt|x" => Some(0) ; "the older batch once the newer is cancelled")]
    #[test_case("t|u" => Some(0) ; "past a batch that can no longer be cancelled")]
    #[test_case("ut|t" => Some(2) ; "the newer batch, not the one running")]
    fn the_cancel_key_targets(kinds: &str) -> Option<usize> {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables(kinds);

        file_system.cancel_target()
    }

    #[test]
    fn cancelling_an_unrelated_prompt_is_not_claimed() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);

        let result = file_system.handle_command(&Command::CancelPrompt);

        assert!(matches!(result, CommandResult::NotHandled));
    }

    #[test]
    fn a_paste_that_asked_nothing_keeps_no_state_for_a_later_prompt_to_disturb() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_paste_keeps_nothing");

        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone(), fx.other.clone()],
            dest: fx.dest.clone(),
        });
        assert!(file_system.pending_paste.is_none());

        let result = file_system.handle_command(&Command::CancelPrompt);

        assert!(matches!(result, CommandResult::NotHandled));
        tasks::await_end(&rx);
        tasks::await_end(&rx);
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"src".to_vec(), fx.pasted("b.txt"));
    }

    /// The refusal of a source whose name this paste already started.
    fn same_name(is_move: bool, source: &Path, dest: &Path) -> String {
        let operation = if is_move { "move" } else { "copy" };
        format!(
            "Cannot {operation} {} into {}: another source in this paste already takes that name \
             there",
            compact(source),
            compact(dest)
        )
    }

    fn entry(is_move: bool, sources: Vec<PathInfo>) -> ClipboardEntry {
        if is_move {
            ClipboardEntry::Move(sources)
        } else {
            ClipboardEntry::Copy(sources)
        }
    }

    /// The clipboard entry a paste leaves for `sources` it did not paste.
    fn left_over(is_move: bool, sources: Vec<PathInfo>) -> Command {
        Command::SetClipboardEntry(Some(entry(is_move, sources)))
    }

    /// The first source ran while the prompt was open; its twin is refused before
    /// the disk is checked.
    #[test_case(true ; "a cut")]
    #[test_case(false ; "a copy")]
    fn a_second_source_of_a_taken_name_is_refused_whatever_the_standing_answer(is_move: bool) {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_claimed_twin");
        let elsewhere = fx.dest.path.parent().expect("a parent").join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("a.txt"), b"twin").unwrap();
        let twin = PathInfo::try_from(elsewhere.join("a.txt").as_path()).unwrap();
        fx.occupy("b.txt");
        let srcs = vec![fx.src.clone(), fx.other.clone(), twin.clone()];
        let paste = entry(is_move, srcs).into_paste(fx.dest.clone());

        let commands = file_system.handle_command(&paste).into_commands();
        assert_eq!(("b.txt", true), conflict_prompt(&commands));
        assert_eq!(None, tasks::await_end(&rx).error_message());
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::OverwriteAll))
            .into_commands();
        assert_eq!(None, tasks::await_end(&rx).error_message());

        assert_eq!(
            vec![
                Command::AlertError(same_name(is_move, &twin.path, &fx.dest.path)),
                left_over(is_move, vec![twin.clone()]),
            ],
            commands
        );
        assert_eq!(b"src".to_vec(), fx.pasted("b.txt"));
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"twin".to_vec(), fs::read(&twin.path).unwrap());
        assert_eq!(is_move, !fx.src.path.exists());
    }

    #[test]
    fn a_skipped_source_leaves_its_name_for_a_twin_to_be_asked_about() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_skipped_twin");
        let elsewhere = fx.dest.path.parent().expect("a parent").join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("b.txt"), b"twin").unwrap();
        let twin = PathInfo::try_from(elsewhere.join("b.txt").as_path()).unwrap();
        fx.occupy("b.txt");

        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.other.clone(), twin.clone()],
                dest: fx.dest.clone(),
            })
            .into_commands();
        assert_eq!(("b.txt", true), conflict_prompt(&commands));
        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::Skip))
            .into_commands();

        assert_eq!(
            vec![Command::OpenPrompt(PromptAction::Conflict {
                name: "b.txt".to_string(),
                can_overwrite: true,
            })],
            commands
        );
        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::Skip))
            .into_commands();
        assert_eq!(Vec::<Command>::new(), commands);
        assert_eq!(b"twin".to_vec(), fs::read(&twin.path).unwrap());
    }

    /// Like `cp -f` and `mv -f`, the answer is for the name, not the entry.
    #[test_case(true ; "a cut")]
    #[test_case(false ; "a copy")]
    fn an_entry_replaced_while_the_prompt_was_open_is_replaced(is_move: bool) {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_prompt_changed");
        fx.occupy("a.txt");
        let paste = entry(is_move, vec![fx.src.clone()]).into_paste(fx.dest.clone());
        let commands = file_system.handle_command(&paste).into_commands();
        assert_eq!(("a.txt", true), conflict_prompt(&commands));
        let new = fx.dest.path.join("a.txt");
        fs::remove_file(&new).unwrap();
        fs::write(&new, b"since").unwrap();

        file_system.handle_command(&Command::ResolveConflict(ConflictChoice::Overwrite));

        assert_eq!(None, tasks::await_end(&rx).error_message());
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
        assert_eq!(!is_move, fx.src.path.exists());
    }

    /// It meets the entry really there and is skipped too, not refused as a twin.
    #[test]
    fn a_skipped_source_claims_no_name() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_skipped_claims_nothing");
        let elsewhere = fx.dest.path.parent().expect("a parent").join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("a.txt"), b"twin").unwrap();
        let twin = PathInfo::try_from(elsewhere.join("a.txt").as_path()).unwrap();
        fx.occupy("a.txt");
        fx.occupy("b.txt");

        let commands = file_system
            .handle_command(&Command::Copy {
                srcs: vec![fx.other.clone(), fx.src.clone(), twin],
                dest: fx.dest.clone(),
            })
            .into_commands();
        assert_eq!(("b.txt", true), conflict_prompt(&commands));
        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::SkipAll))
            .into_commands();

        assert_eq!(Vec::<Command>::new(), commands);
        assert_eq!(b"dest".to_vec(), fx.pasted("a.txt"));
    }

    #[test]
    fn a_source_whose_task_never_started_claims_no_name() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_failed_claim");
        let elsewhere = fx.dest.path.parent().expect("a parent").join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("missing.txt"), b"twin").unwrap();
        let twin = PathInfo::try_from(elsewhere.join("missing.txt").as_path()).unwrap();

        let result = file_system.handle_command(&Command::Copy {
            srcs: vec![fx.missing.clone(), twin],
            dest: fx.dest.clone(),
        });

        let commands = result.into_commands();
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                Command::OpenPrompt(PromptAction::Conflict { .. })
            )),
            "unexpected conflict prompt: {commands:?}"
        );
        tasks::await_end(&rx);
        assert_eq!(b"twin".to_vec(), fx.pasted("missing.txt"));
    }

    #[test]
    fn read_bookmarks_creates_the_directory_and_lists_its_entries() {
        let base = TempDir::reserved("read_bookmarks_ok");
        let dir = base.join("bookmarks");

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
            error.starts_with("Failed to create bookmarks directory"),
            "unexpected message: {error}"
        );
    }

    #[test]
    fn get_bookmarks_cancels_the_in_flight_directory_load() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let load_token = CancellationToken::new();
        file_system.current_load = Some((1, load_token.clone()));

        let result = file_system.handle_command(&Command::GetBookmarks);

        let command = Command::try_from(result).expect("expected a derived command");
        assert!(matches!(command, Command::Bookmarks { .. }));
        assert!(load_token.is_cancelled());
        assert!(file_system.current_load.is_none());
        drop(rx);
    }

    #[test]
    fn a_refresh_after_a_search_rereads_its_results_without_listing_the_directory() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_search_refresh");
        let gone = root.file("gone", 1);
        let changed = root.file("changed", 1);
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.current_search_generation = 5;
        file_system.searching();
        file_system.handle_command(&Command::ListingBatch {
            items: vec![gone.clone(), changed.clone()],
            generation: 5,
        });
        fs::remove_file(&gone.path).unwrap();
        fs::write(&changed.path, b"longer").unwrap();

        let result = file_system.handle_command(&Command::RefreshDirectory);

        assert!(matches!(result, CommandResult::Handled));
        assert!(file_system.current_load.is_none());
        let Ok(Command::SearchResultsRefreshed { items, generation }) =
            rx.recv_timeout(Duration::from_secs(5))
        else {
            panic!("expected the results read again");
        };
        assert_eq!(5, generation);
        let read: Vec<_> = items
            .iter()
            .map(|item| (item.path.clone(), item.size))
            .collect();
        assert_eq!(vec![(changed.path.clone(), 6)], read);
    }

    #[test]
    fn a_task_finishing_after_a_search_rereads_its_results() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_search_task");
        let hit = root.file("hit", 1);
        file_system.current_search_generation = 4;
        file_system.searching();
        file_system.search_batch(std::slice::from_ref(&hit), 4);
        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let (active, _, _) = crate::command::progress::ActiveTask::new(
            task_tx,
            crate::command::progress::TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        active.done();
        let finished = task_rx
            .try_iter()
            .find_map(|command| match command {
                Command::Progress(task) if task.is_terminal() => Some(task),
                _ => None,
            })
            .expect("a terminal progress");
        fs::remove_file(&hit.path).unwrap();

        file_system.handle_command(&Command::Progress(finished));

        let Ok(Command::SearchResultsRefreshed { items, generation }) =
            rx.recv_timeout(Duration::from_secs(5))
        else {
            panic!("expected the results read again");
        };
        assert_eq!(4, generation);
        assert!(items.is_empty());
    }

    #[test]
    fn a_refresh_during_a_search_reads_nothing() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_search_running");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.searching();
        file_system
            .cancellables
            .push(Cancellable::Search(CancellationToken::new()));

        let result = file_system.handle_command(&Command::RefreshDirectory);

        assert!(matches!(result, CommandResult::Handled));
        assert!(file_system.current_load.is_none());
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn only_the_current_searchs_batches_are_recorded() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_search_batches");
        let hit = root.file("hit", 1);
        file_system.current_search_generation = 3;
        file_system.searching();

        file_system.handle_command(&Command::ListingBatch {
            items: vec![hit.clone()],
            generation: 2,
        });
        file_system.handle_command(&Command::ListingBatch {
            items: vec![hit.clone()],
            generation: 3,
        });

        assert_eq!(vec![hit.path], file_system.search_results);
    }

    #[test]
    fn the_bookmarks_view_watches_and_reloads_the_bookmarks() {
        let bookmarks = TempDir::new("fs_bookmarks_view");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_bookmarks_cwd");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.watcher = Some(DirectoryWatcher::try_new(100).unwrap());
        file_system.handle_command(&Command::GetBookmarks);
        let watched = file_system.watcher.as_ref().unwrap().watched_directory();
        assert_eq!(Some(bookmarks.path().to_path_buf()), watched);

        std::os::unix::fs::symlink(root.path(), bookmarks.join("added")).unwrap();
        let result = file_system.handle_command(&Command::RefreshDirectory);

        let command = Command::try_from(result).expect("expected a derived command");
        let Command::Bookmarks { bookmarks: listed } = command else {
            panic!("expected Bookmarks, got {command:?}");
        };
        assert_eq!(1, listed.len());
        assert!(file_system.current_load.is_none());

        file_system.handle_command(&Command::ResetView);
        let _ = file_system.handle_command(&Command::RefreshDirectory);
        let watched = file_system.watcher.as_ref().unwrap().watched_directory();
        assert_eq!(Some(root.path().to_path_buf()), watched);
        file_system.cancel_current_load();
    }

    #[test]
    fn a_refresh_during_a_load_waits_for_it_instead_of_restarting_it() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_reload");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        let load_token = CancellationToken::new();
        file_system.current_load = Some((7, load_token.clone()));

        let result = file_system.handle_command(&Command::RefreshDirectory);

        assert!(matches!(result, CommandResult::Handled));
        assert!(!load_token.is_cancelled());
        assert!(file_system.reload_pending);
        drop(rx);
    }

    #[test]
    fn the_deferred_refresh_runs_once_the_load_reports_in() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_reload_complete");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.current_load = Some((7, CancellationToken::new()));
        file_system.handle_command(&Command::RefreshDirectory);

        let result =
            file_system.handle_command(&Command::DirectoryListingComplete { generation: 7 });

        // The change may have landed after the load opened the directory.
        let command = Command::try_from(result).expect("expected a derived command");
        let Command::RefreshedDirectory {
            directory,
            generation,
        } = command
        else {
            panic!("expected RefreshedDirectory, got {command:?}");
        };
        assert_eq!(root.path(), directory.as_path());
        // Reusing the completed generation would let its trailing batches stream in.
        assert_ne!(7, generation);
        assert_eq!(Some(generation), file_system.current_load.map(|(id, _)| id));
        assert!(!file_system.reload_pending);
        drop(rx);
    }

    #[test_case(false ; "renamed away")]
    #[test_case(true ; "replaced by a file")]
    fn a_directory_that_can_no_longer_be_listed_is_no_longer_watched(replaced: bool) {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_refresh_unlistable");
        let sub = root.join("sub");
        fs::create_dir(&sub).unwrap();
        let mut watcher = DirectoryWatcher::try_new(100).unwrap();
        watcher.watch_directory(sub.clone()).unwrap();
        file_system.watcher = Some(watcher);
        file_system.directory = Some(PathInfo::try_from(sub.as_path()).unwrap());
        fs::rename(&sub, root.join("sub.orig")).unwrap();
        if replaced {
            fs::write(&sub, b"not a directory").unwrap();
        }

        let commands = file_system
            .handle_command(&Command::RefreshDirectory)
            .into_commands();

        let [Command::AlertError(message)] = commands.as_slice() else {
            panic!("expected one alert, got {commands:?}");
        };
        // A reload, not a navigation: nothing was changed to.
        assert!(message.starts_with("Failed to read directory"), "{message}");
        let watcher = file_system.watcher.as_ref().unwrap();
        assert_eq!(None, watcher.watched_directory());
    }

    #[test]
    fn a_completion_from_a_superseded_load_is_ignored() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.current_load = Some((9, CancellationToken::new()));
        file_system.reload_pending = true;

        let result =
            file_system.handle_command(&Command::DirectoryListingComplete { generation: 7 });

        assert!(matches!(result, CommandResult::NotHandled));
        assert!(file_system.current_load.is_some());
        assert!(file_system.reload_pending);
        drop(rx);
    }

    #[test]
    fn search_cancels_the_in_flight_directory_load() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        // Not /tmp: the detached walk would traverse every other test's fixtures.
        let root = TempDir::new("fs_search_root");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        let load_token = CancellationToken::new();
        file_system.current_load = Some((1, load_token.clone()));

        let _ = file_system.search("query");

        assert!(load_token.is_cancelled());
        assert!(file_system.current_load.is_none());
        // Nothing stops the detached walk on drop, so stop it before the fixture goes.
        file_system.cancel_search();
        drop(rx);
    }

    #[test]
    fn a_finished_task_leaves_the_cancel_stack_and_the_rest_of_it_alone() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("tts");

        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let (active, initial, _token) = crate::command::progress::ActiveTask::new(
            task_tx,
            crate::command::progress::TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        let finished_id = initial.id();
        active.error("failed".to_string());
        let Ok(Command::Progress(finished)) = task_rx.recv() else {
            panic!("the task should have reported")
        };
        // `cancellables` gives every entry id 0; the first stands for an unrelated task.
        let Cancellable::Task(second, _) = &mut file_system.cancellables[1] else {
            panic!("expected a task")
        };
        second.id = finished_id;

        file_system.check_progress_for_error(&finished);

        assert_eq!(2, file_system.cancellables.len());
        assert!(matches!(
            file_system.cancellables[0],
            Cancellable::Task(CancelInfo { id: 0, .. }, _)
        ));
        assert!(matches!(
            file_system.cancellables[1],
            Cancellable::Search(_)
        ));
    }

    #[test]
    fn only_the_current_search_exiting_clears_the_search_state() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.current_search_generation = 4;
        file_system.cancellables = cancellables("ts");

        file_system.on_search_exited(3);
        assert_eq!(2, file_system.cancellables.len());

        file_system.on_search_exited(4);
        assert!(matches!(
            file_system.cancellables.as_slice(),
            [Cancellable::Task(..)]
        ));
    }

    fn task_info(file_system: &FileSystem, index: usize) -> &CancelInfo {
        match &file_system.cancellables[index] {
            Cancellable::Task(info, _) => info,
            Cancellable::Search(_) => panic!("expected a task at {index}"),
        }
    }

    #[test]
    fn a_cancelled_task_counts_until_its_terminal_progress() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("t");
        let (task_tx, task_rx) = std::sync::mpsc::channel();
        let (active, initial, _token) = crate::command::progress::ActiveTask::new(
            task_tx,
            crate::command::progress::TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        let Cancellable::Task(info, _) = &mut file_system.cancellables[0] else {
            panic!("expected a task")
        };
        info.id = initial.id();

        file_system.handle_command(&Command::CancelTask);

        assert_eq!(1, file_system.task_count());
        assert_eq!(None, file_system.cancel_target());

        active.cancelled();
        let Ok(Command::Progress(ended)) = task_rx.recv() else {
            panic!("the task should have reported")
        };
        file_system.check_progress_for_error(&ended);
        assert_eq!(0, file_system.task_count());
    }

    #[test]
    fn the_cancel_key_cancels_the_running_task_and_keeps_it_until_it_ends() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("tt");
        let running = task_info(&file_system, 0).token.clone();
        let queued = task_info(&file_system, 1).token.clone();

        let commands = file_system
            .handle_command(&Command::CancelTask)
            .into_commands();

        let [Command::AlertInfo(message)] = commands.as_slice() else {
            panic!("expected one notice, got {commands:?}");
        };
        assert!(message.starts_with("Cancelled: "), "{message}");
        assert!(message.ends_with(" and 1 more task"), "{message}");
        assert!(running.is_cancelled());
        assert!(queued.is_cancelled());
        assert_eq!(2, file_system.cancellables.len());
    }

    #[test]
    fn the_cancel_key_cancels_only_the_newest_batch() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("tt|tt");
        let tokens: Vec<_> = (0..4)
            .map(|index| task_info(&file_system, index).token.clone())
            .collect();

        file_system.handle_command(&Command::CancelTask);

        assert_eq!(
            vec![false, false, true, true],
            tokens
                .iter()
                .map(CancellationToken::is_cancelled)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn an_uncancellable_task_does_not_hold_back_its_batch() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("utt");
        let tokens: Vec<_> = (0..3)
            .map(|index| task_info(&file_system, index).token.clone())
            .collect();

        let commands = file_system
            .handle_command(&Command::CancelTask)
            .into_commands();

        assert_eq!(
            vec![false, true, true],
            tokens
                .iter()
                .map(CancellationToken::is_cancelled)
                .collect::<Vec<_>>()
        );
        let [Command::AlertInfo(message)] = commands.as_slice() else {
            panic!("expected one notice, got {commands:?}");
        };
        assert!(message.ends_with(" and 1 more task"), "{message}");
    }

    #[test]
    fn one_keypress_cancels_a_whole_delete() {
        let fx = TempDir::new("fs_cancel_batch");
        let paths: Vec<PathInfo> = ["a", "b", "c"]
            .iter()
            .map(|name| {
                let path = fx.join(name);
                std::fs::write(&path, b"keep").unwrap();
                PathInfo::try_from(path.as_path()).unwrap()
            })
            .collect();
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let gate = tasks::hold_worker();

        file_system.handle_command(&Command::Delete(paths.clone()));
        file_system.handle_command(&Command::CancelTask);
        drop(gate);

        for _ in 0..3 {
            let task = tasks::await_end(&rx);
            assert!(task.is_cancelled(), "{task:?}");
        }
        for path in &paths {
            assert!(path.path.exists(), "{:?} was removed", path.path);
        }
    }

    #[test]
    fn a_task_past_the_point_of_cancelling_stays_and_says_so() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("t");
        let info = task_info(&file_system, 0);
        info.uncancellable.store(true, Ordering::Relaxed);
        let token = info.token.clone();

        let commands = file_system
            .handle_command(&Command::CancelTask)
            .into_commands();

        let [Command::AlertInfo(message)] = commands.as_slice() else {
            panic!("expected one notice, got {commands:?}");
        };
        assert!(message.starts_with("Cannot cancel: "), "{message}");
        assert!(!token.is_cancelled());
        assert_eq!(1, file_system.cancellables.len());
    }

    #[test]
    fn a_search_that_already_finished_is_left_for_its_exit_to_clear() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("s");
        // A finished search cancels its own token on the way out.
        let Cancellable::Search(token) = &file_system.cancellables[0] else {
            panic!("expected a search");
        };
        token.cancel();

        let result = file_system.handle_command(&Command::CancelTask);

        assert!(matches!(result, CommandResult::Handled));
        assert_eq!(1, file_system.cancellables.len());
    }

    #[test]
    fn starting_a_search_replaces_the_previous_one() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_search_replace");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.cancellables = cancellables("s");
        let Cancellable::Search(previous) = &file_system.cancellables[0] else {
            panic!("expected a search");
        };
        let previous = previous.clone();

        let commands = file_system.search("query").into_commands();

        let [Command::SearchStarted { generation }] = commands.as_slice() else {
            panic!("expected SearchStarted, got {commands:?}");
        };
        assert_eq!(*generation, file_system.current_search_generation);
        assert!(previous.is_cancelled());
        let [Cancellable::Search(current)] = file_system.cancellables.as_slice() else {
            panic!("expected only the new search to be registered");
        };
        // The new search may already have cancelled its own token on an empty root.
        assert!(!current.is_same(&previous));
        file_system.cancel_search();
        drop(rx);
    }

    #[test]
    fn chmod_refuses_a_mode_that_is_not_octal() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_chmod_invalid");
        let file = root.join("a.txt");
        fs::write(&file, b"a").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();

        let commands = file_system
            .handle_command(&Command::Chmod {
                paths: vec![PathInfo::try_from(file.as_path()).unwrap()],
                mode: "rwx".to_string(),
            })
            .into_commands();

        let [Command::AlertError(message)] = commands.as_slice() else {
            panic!("expected one alert, got {commands:?}");
        };
        assert_eq!(
            format!(
                "Cannot chmod {}: \"rwx\" is not an octal mode",
                compact(&file)
            ),
            *message
        );
        assert_eq!(
            0o600,
            fs::metadata(&file).unwrap().permissions().mode() & 0o7777
        );
    }

    #[test]
    fn chmod_reports_a_failed_entry_and_still_changes_the_rest() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_chmod_partial");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        let file = root.join("a.txt");
        fs::write(&file, b"a").unwrap();
        let present = PathInfo::try_from(file.as_path()).unwrap();
        let mut gone = present.clone();
        gone.path = root.join("gone.txt");

        let commands = file_system
            .handle_command(&Command::Chmod {
                paths: vec![gone, present],
                mode: "640".to_string(),
            })
            .into_commands();

        let [
            Command::AlertError(message),
            Command::RefreshedDirectory { .. },
        ] = commands.as_slice()
        else {
            panic!("expected the failure and the refresh, got {commands:?}");
        };
        assert!(message.starts_with("Failed to chmod"), "{message}");
        assert_eq!(
            0o640,
            fs::metadata(&file).unwrap().permissions().mode() & 0o7777
        );
        file_system.cancel_current_load();
        drop(rx);
    }

    #[test]
    fn changing_to_a_directory_that_cannot_be_read_stays_where_it_was() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_cd_unreadable");
        let here = PathInfo::try_from(root.path()).unwrap();
        file_system.cd(here.clone(), true);
        let mut missing = here.clone();
        missing.path = root.join("missing");

        let commands = file_system.cd(missing, true).into_commands();

        let [Command::AlertError(message)] = commands.as_slice() else {
            panic!("expected one alert, got {commands:?}");
        };
        assert!(
            message.starts_with("Failed to change to directory"),
            "{message}"
        );
        assert_eq!(here.path, file_system.current_directory().path);
        assert_eq!(None, previous_path(&file_system));
        file_system.cancel_current_load();
    }

    #[test]
    fn going_to_the_parent_navigates_there() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_parent");
        fs::create_dir(root.join("sub")).unwrap();
        file_system.cd(
            PathInfo::try_from(root.join("sub").as_path()).unwrap(),
            true,
        );

        let commands = file_system
            .handle_command(&Command::GoToParentDirectory)
            .into_commands();

        let [Command::NavigatedDirectory { directory, .. }] = commands.as_slice() else {
            panic!("expected NavigatedDirectory, got {commands:?}");
        };
        assert_eq!(root.path(), directory.path);
        assert_eq!(Some(root.join("sub")), previous_path(&file_system));
        file_system.cancel_current_load();
    }

    #[test]
    fn opening_a_directory_navigates_into_it() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_open_directory");
        fs::create_dir(root.join("sub")).unwrap();

        let commands = file_system
            .handle_command(&Command::Open(
                PathInfo::try_from(root.join("sub").as_path()).unwrap(),
            ))
            .into_commands();

        let [Command::NavigatedDirectory { directory, .. }] = commands.as_slice() else {
            panic!("expected NavigatedDirectory, got {commands:?}");
        };
        assert_eq!(root.join("sub").canonicalize().unwrap(), directory.path);
        file_system.cancel_current_load();
    }

    #[test]
    fn navigating_away_drops_a_refresh_deferred_for_the_old_directory() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_reload_dropped");
        fs::create_dir(root.join("sub")).unwrap();
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.current_load = Some((7, CancellationToken::new()));
        file_system.handle_command(&Command::RefreshDirectory);

        file_system.cd(
            PathInfo::try_from(root.join("sub").as_path()).unwrap(),
            true,
        );

        assert!(!file_system.reload_pending);
        file_system.cancel_current_load();
    }

    #[test]
    fn navigating_records_where_it_came_from_unless_it_stayed_put() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_previous");
        fs::create_dir_all(root.join("sub")).unwrap();
        let first = PathInfo::try_from(root.path()).unwrap();
        let second = PathInfo::try_from(root.join("sub").as_path()).unwrap();

        file_system.cd(first.clone(), true);
        assert_eq!(None, previous_path(&file_system));

        file_system.cd(second.clone(), true);
        assert_eq!(Some(first.path.clone()), previous_path(&file_system));

        file_system.cd(second, true);
        assert_eq!(Some(first.path), previous_path(&file_system));
    }

    /// The token is registered by hand: a real walk may cancel its own token before
    /// the navigation lands.
    #[test]
    fn navigating_away_stops_a_running_search() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_navigate_search");
        fs::create_dir(root.join("sub")).unwrap();
        file_system.directory = Some(PathInfo::try_from(root.join("sub").as_path()).unwrap());
        file_system.cancellables = cancellables("ts");
        let Cancellable::Search(search) = &file_system.cancellables[1] else {
            panic!("expected a search");
        };
        let search = search.clone();

        file_system.handle_command(&Command::GoToParentDirectory);

        assert!(search.is_cancelled());
        assert!(matches!(
            file_system.cancellables.as_slice(),
            [Cancellable::Task(..)]
        ));
        file_system.cancel_current_load();
    }

    #[test]
    fn a_reload_leaves_a_running_search_alone() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_reload_search");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        file_system.cancellables = cancellables("s");
        let Cancellable::Search(search) = &file_system.cancellables[0] else {
            panic!("expected a search");
        };
        let search = search.clone();

        let commands = file_system
            .handle_command(&Command::RefreshDirectory)
            .into_commands();

        assert!(matches!(
            commands.as_slice(),
            [Command::RefreshedDirectory { .. }]
        ));
        assert!(!search.is_cancelled());
        assert_eq!(1, file_system.cancellables.len());
        file_system.cancel_current_load();
    }

    fn previous_path(file_system: &FileSystem) -> Option<PathBuf> {
        file_system
            .previous_directory
            .as_ref()
            .map(|info| info.path.clone())
    }

    #[test_case("644" => Some(0o644) ; "three digits")]
    #[test_case("755" => Some(0o755) ; "three digits with the execute bit")]
    #[test_case("0" => Some(0o0) ; "a single zero")]
    #[test_case("7777" => Some(0o7777) ; "the largest accepted value")]
    #[test_case("4755" => Some(0o4755) ; "a setuid bit")]
    #[test_case("10000" => None ; "one past the largest")]
    #[test_case("77777" => None ; "five digits")]
    #[test_case("888" => None ; "digits outside octal")]
    #[test_case("0o644" => None ; "a rust literal prefix")]
    #[test_case("rwx" => None ; "symbolic notation")]
    #[test_case("" => None ; "empty")]
    #[test_case("-1" => None ; "negative")]
    #[test_case("+644" => None ; "a leading plus")]
    fn parse_octal_mode_accepts_only_a_mode(mode: &str) -> Option<u32> {
        parse_octal_mode(mode)
    }
}
