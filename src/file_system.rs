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
    collections::HashSet,
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
    conflicts::Conflicts,
    operations::{open_in, spawn_argv},
    paste::{PasteStep, PendingPaste},
    path_info::{PathInfo, compact},
    search::Limits,
    tasks::{CancelInfo, TaskCommand},
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

/// How often a running search wakes the event loop to redraw its loading
/// indicator. Only a heartbeat: the position comes from elapsed time, so this
/// bounds how coarse the motion gets over a stretch of the walk that finds
/// nothing, and does nothing at all while batches are arriving to redraw.
const SEARCH_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// A cancellable in-flight action. File operations and searches share one list,
/// in registration order; `cancel_target` decides which entry a cancel keypress
/// aims at.
enum Cancellable {
    Task(CancelInfo),
    Search(CancellationToken),
}

/// A file operation already told to stop, waiting for its terminal progress.
fn is_cancelled_task(cancellable: &Cancellable) -> bool {
    matches!(cancellable, Cancellable::Task(info) if info.token.is_cancelled())
}

pub struct FileSystem {
    /// Directory holding the bookmark symlinks, resolved from the config once
    /// so bookmark reads do not depend on the process-global `Config`.
    bookmarks_dir: PathBuf,
    cancellables: Vec<Cancellable>,
    command_tx: Sender<Command>,
    directory: Option<PathInfo>,
    previous_directory: Option<PathInfo>,
    /// The in-flight streamed directory load: its generation, and the token
    /// that stops it. Cancelled when a new load starts so stale batches don't
    /// bleed across, and cleared when the load reports itself complete.
    current_load: Option<(u64, CancellationToken)>,
    /// When the in-flight load started, which paces the watcher by how long a
    /// listing of this directory takes.
    load_started: Option<Instant>,
    /// Set when a refresh arrives while a load is already streaming, so the
    /// load runs to completion and the refresh is re-issued afterwards.
    reload_pending: bool,
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
    /// The paste awaiting a conflict answer, if any. The only thing that ever
    /// asks: a worker resolves what it finds from the paste's standing answer
    /// or records it, so there is never a second prompt to route around.
    pending_paste: Option<PendingPaste>,
    search_max_depth: u32,
    search_max_results: u32,
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
            watcher,
        }
    }

    pub fn run_once(&mut self, directory: Option<PathBuf>) -> Result<Vec<Command>> {
        if let Some(watcher) = &mut self.watcher {
            watcher.run_once(&self.command_tx);
        }

        // Already canonical: the command line's directory is canonicalized when
        // it is validated, and `getcwd` returns a path with no symlink, `.` or
        // `..` component.
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

        // Fall back to the home directory when the startup directory cannot
        // be opened: every navigation command requires a current directory,
        // so continuing without one is not an option. If home cannot be
        // opened either, exit rather than run in a broken state.
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
        // Cheap readability pre-flight so we don't switch into a directory we
        // cannot open (e.g. permission denied). The full per-entry read happens
        // asynchronously in `stream_cd` below.
        if let Err(error) = fs::read_dir(&directory.path) {
            return anyhow!(
                "Failed to change to directory {}: {error}",
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
        // A navigation replaces the search results, so the walk's remaining
        // work is wasted. A reload leaves the listing, and any search, alone.
        if navigate {
            self.cancel_search();
        }
        self.directory = Some(directory.clone());
        let path_buf = directory.path.clone();
        if let Some(watcher) = &mut self.watcher
            && let Err(e) = watcher.watch_directory(path_buf.clone())
        {
            // The listing itself may load fine: only the automatic refresh is
            // lost.
            let _ = self.command_tx.send(Command::AlertWarn(format!(
                "Failed to watch directory {}: {e}",
                compact(&path_buf)
            )));
        }

        // Cancel any in-flight load so its batches don't bleed into this one,
        // then start streaming the new directory's entries.
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

    /// The next stream generation. Shared by directory loads and searches so
    /// a generation is never ambiguous between the two.
    fn bump_generation(&mut self) -> u64 {
        self.next_generation += 1;
        self.next_generation
    }

    /// Cancels the in-flight streamed directory load, if any. No-op when
    /// nothing is streaming.
    fn cancel_current_load(&mut self) {
        // Whatever the pending refresh was going to re-read has been replaced,
        // so it goes with the load.
        self.reload_pending = false;
        if let Some((_, token)) = self.current_load.take() {
            token.cancel();
        }
    }

    /// Handles a streamed load reporting itself complete. Clears the in-flight
    /// load and re-issues a refresh that arrived while it was running.
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

    /// Full search teardown (Esc / `ResetView`): cancel and drop every search
    /// entry. No-op if `cancel_task` already cancelled it. The thread still
    /// emits one last `ExitedSearch`, which a superseded generation makes every
    /// consumer ignore, and which is otherwise a no-op because the entry and
    /// notice state is already cleared.
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

    /// How many file operations are running or queued. A task stays counted
    /// until its terminal progress arrives, cancelled or not: a cancelled copy
    /// is still finishing the directories it entered.
    pub(crate) fn task_count(&self) -> usize {
        self.cancellables
            .iter()
            .filter(|cancellable| matches!(cancellable, Cancellable::Task(_)))
            .count()
    }

    /// The entry a cancel keypress targets, so it always aims at work that is
    /// actually running rather than at whatever was registered last.
    ///
    /// A search runs alongside everything else, so the most recent one started
    /// is what the keypress means. File operations share a single worker and run
    /// in queue order, so the *oldest* registered one is the one running:
    /// cancelling the newest of a batch would stop work that has not started
    /// while the copy the user is watching carries on.
    ///
    /// A cancelled task stays registered until its terminal progress arrives,
    /// but is never targeted again, so the next keypress reaches the work after
    /// it.
    fn cancel_target(&self) -> Option<usize> {
        let newest = self
            .cancellables
            .iter()
            .rposition(|cancellable| !is_cancelled_task(cancellable))?;
        match self.cancellables[newest] {
            Cancellable::Search(_) => Some(newest),
            Cancellable::Task(_) => self.cancellables.iter().position(|cancellable| {
                matches!(cancellable, Cancellable::Task(info) if !info.token.is_cancelled())
            }),
        }
    }

    fn cancel_most_recent_task(&mut self) -> CommandResult {
        let Some(index) = self.cancel_target() else {
            return Command::AlertWarn("No active task to cancel".into()).into();
        };
        match &self.cancellables[index] {
            Cancellable::Task(info) => {
                // Either way the task stays on the stack until its terminal
                // Progress prunes it: a cancelled one is still unwinding, and
                // quit counts it. `uncancellable` covers both a task in an
                // uninterruptible stage and one already finished whose terminal
                // Progress is in flight; the wording fits the first and is
                // momentarily imprecise for the second.
                if info.uncancellable.load(Ordering::Relaxed) {
                    return Command::AlertInfo(format!("Cannot cancel: {}", info.kind.message()))
                        .into();
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
                    return CommandResult::Handled;
                }
                token.cancel();
                self.cancellables.remove(index);
                // Non-destructive: keep streamed results and the notice,
                // which NoticesView relabels as cancelled.
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
        // Name the path in the error: a broken symlink (e.g. a bookmark whose
        // target was removed) would otherwise surface a bare io error.
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

    /// Launch an application the "open with" picker already resolved into an
    /// argv, so no template substitution or shell is involved here.
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
        // Return the failures alongside the refresh instead of sending them
        // separately, so they are ordered against it rather than racing the
        // channel drain.
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
        // A load for this directory is already streaming and will pick the
        // change up. Restarting it would cancel it before it can finalize, and
        // under sustained churn (a build writing into the viewed directory) it
        // never would: the sort and the end of the loading state both hang off
        // that completion. The refresh is re-issued when the load reports in.
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
            // The path no longer names a directory that can be read: renamed
            // away, replaced by a file, or made unreadable. The watch is still
            // on what it named, and each change there would retry this reload
            // and report its failure again.
            watcher.unwatch();
        }
        commands.into()
    }

    /// Runs a task, registering it on the cancel stack when it starts. Returns
    /// whether it started, together with the alerts it produced (a task that
    /// fails validation produces one and starts nothing). Started tasks send
    /// their initial progress snapshot themselves, before queueing their work.
    fn run_task(
        &mut self,
        task: TaskCommand,
        conflicts: Option<&Conflicts>,
    ) -> (bool, Vec<Command>) {
        let result = task.run(self.command_tx.clone(), conflicts);
        let started = result.cancel_info.is_some();
        if let Some(cancel_info) = result.cancel_info {
            self.cancellables.push(Cancellable::Task(cancel_info));
        }
        (started, result.command_result.into_commands())
    }

    /// Starts a paste. Sources run one at a time so that a name already taken
    /// in the destination can be answered for before the next source starts.
    fn start_paste(&mut self, is_move: bool, srcs: &[PathInfo], dest: &PathInfo) -> CommandResult {
        self.pending_paste = Some(PendingPaste {
            is_move,
            dest: dest.clone(),
            remaining: srcs.iter().cloned().collect(),
            failed: Vec::new(),
            started: 0,
            conflicts: Conflicts::default(),
            claimed: HashSet::new(),
        });
        self.advance_paste()
    }

    /// Runs queued sources until one collides with a destination that has not
    /// been answered for, or until the batch is done. Returns the alerts the
    /// tasks produced, plus either the conflict prompt or the clipboard
    /// follow-up.
    fn advance_paste(&mut self) -> CommandResult {
        let Some(mut pending) = self.pending_paste.take() else {
            return CommandResult::Handled;
        };
        let mut commands = Vec::new();
        while let Some(src) = pending.remaining.front().cloned() {
            if pending.is_claimed(&src) {
                pending.remaining.pop_front();
                commands.push(Command::AlertError(claimed_message(&pending, &src)));
                pending.failed.push(src);
                continue;
            }
            match pending.step(pending.occupant(&src)) {
                PasteStep::Ask { can_overwrite } => {
                    commands.push(Command::OpenPrompt(PromptAction::Conflict {
                        name: src.display_name.clone(),
                        can_overwrite,
                    }));
                    // The source stays at the front of the queue: the answer is
                    // what pops it.
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

    /// Applies a conflict answer to the source at the front of the queue, then
    /// keeps going. The front source is never a claimed one, which
    /// `advance_paste` refuses before asking.
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

    /// Abandons a paste whose conflict prompt was dismissed. Sources never
    /// reached rejoin the failures in the clipboard, so a retry carries exactly
    /// what was not pasted. Deliberately skipped sources are not among them:
    /// skipping was a choice, not a failure.
    ///
    /// Every dismissed prompt arrives here, so a paste is abandoned only when
    /// one is waiting on an answer: dismissing a rename or a filter leaves a
    /// running paste alone. The standing answer stands, so an `*All` already
    /// given still covers the sources already handed out.
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

    /// Runs one source of a paste, recording whether it started so the
    /// clipboard follow-up can tell a clean run from a partial one.
    ///
    /// The name is claimed only once the task is running: a source that failed
    /// validation writes nothing, so claiming it would make a later source of
    /// the same name collide with something that will never be there.
    fn run_paste_task(
        &mut self,
        pending: &mut PendingPaste,
        src: PathInfo,
        overwrite: bool,
    ) -> Vec<Command> {
        let task = if pending.is_move {
            TaskCommand::Move(src.clone(), pending.dest.clone(), overwrite)
        } else {
            TaskCommand::Copy(src.clone(), pending.dest.clone(), overwrite)
        };
        let (started, commands) = self.run_task(task, Some(&pending.conflicts));
        if started {
            pending.started += 1;
            pending.claim(&src);
        } else {
            pending.failed.push(src);
        }
        commands
    }

    fn search(&mut self, query: &str) -> CommandResult {
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

        // Tell consumers which generation is now current, so they can ignore
        // messages from superseded searches.
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

/// Read every entry in the bookmarks directory, creating it if absent.
/// Synchronous: one small directory of symlinks, no streaming. Returns the
/// failure message rather than a command, so the caller can tell success from
/// failure before cancelling the listing the bookmarks would replace. An
/// unreadable entry is skipped, not fatal.
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

/// The refusal for a source whose destination name an earlier source of the
/// same paste already took.
fn claimed_message(pending: &PendingPaste, src: &PathInfo) -> String {
    let operation = if pending.is_move { "move" } else { "copy" };
    format!(
        "Cannot {operation} {} into {}: another source in this paste has the same name",
        compact(&src.path),
        compact(&pending.dest.path)
    )
}

/// The mode a chmod of `paths` to `mode_str` sets, or the refusal naming what
/// it was for. The chmod prompt checks this before it closes, so that a typo
/// can be corrected rather than typed again.
pub(crate) fn chmod_mode(paths: &[PathInfo], mode_str: &str) -> Result<u32> {
    parse_octal_mode(mode_str).ok_or_else(|| {
        let object = match paths {
            [path] => compact(&path.path).to_string(),
            _ => format!("{} items", paths.len()),
        };
        anyhow!("Cannot chmod {object}: {mode_str:?} is not an octal mode")
    })
}

/// Parses a chmod-style octal mode string. Returns `None` for non-octal input
/// or values exceeding `0o7777` (the permission + setuid/setgid/sticky bits).
/// Digits only: `from_str_radix` alone would take a leading `+`, which `chmod`
/// reads as symbolic notation.
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
            // A temp path, so bookmark reads never touch the real config dir.
            bookmarks_dir: bookmarks.path().to_path_buf(),
            cancellables: Vec::new(),
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

            // A vanished source fails the pre-flight re-stat, so its task
            // never starts.
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

        /// Puts a file at `name` in the destination, so pasting the matching
        /// source collides with it.
        pub(super) fn occupy(&self, name: &str) {
            fs::write(self.dest.path.join(name), b"dest").unwrap();
        }

        /// Puts a directory at `name` in the destination: a collision that is
        /// never replaced, whatever the user answers.
        pub(super) fn occupy_with_directory(&self, name: &str) {
            fs::create_dir_all(self.dest.path.join(name)).unwrap();
        }

        fn pasted(&self, name: &str) -> Vec<u8> {
            fs::read(self.dest.path.join(name)).unwrap()
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

        // Nothing was pasted, so the clipboard must survive untouched for the
        // paste to be retried as-is.
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
        await_terminal_task(&rx);
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

        // The failed source's alert rides the same broadcast as the clipboard
        // follow-up, which keeps the operation and only the failed source, so
        // a retry carries just what was not pasted.
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
        // Nothing may run until the collision is answered, and the existing
        // file must still be intact.
        assert!(file_system.cancellables.is_empty());
        assert_eq!(b"dest".to_vec(), fx.pasted("a.txt"));
        assert!(rx.try_recv().is_err());
    }

    // ── the paste loop, which drives the decisions above ─────────────────────

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
        await_terminal_task(&rx);
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
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

        // An overwrite the prompt did not offer is refused before it starts.
        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::Overwrite))
            .into_commands();
        assert!(
            matches!(commands.first(), Some(Command::AlertError(message)) if message.ends_with("never replaces what holds its name")),
            "{commands:?}"
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

        // The skipped source is not a failure, so the clipboard is cleared
        // rather than reduced to it; the non-colliding source still pasted.
        assert!(
            matches!(commands.as_slice(), [Command::SetClipboardEntry(None)]),
            "{commands:?}"
        );
        await_terminal_task(&rx);
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

        // The second collision must not prompt. Skipping is a choice, not a
        // failure, so the clipboard is cleared rather than reduced to the
        // skipped sources.
        assert!(
            matches!(commands.as_slice(), [Command::SetClipboardEntry(None)]),
            "{commands:?}"
        );
        assert!(file_system.pending_paste.is_none());
        await_terminal_task(&rx);
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
        // The first source pastes cleanly; the second collides and is the one
        // the prompt is asking about.
        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone(), fx.other.clone()],
            dest: fx.dest.clone(),
        });

        let commands = file_system
            .handle_command(&Command::CancelPrompt)
            .into_commands();

        // A retry must carry exactly what was not pasted, so the source the
        // prompt was asking about goes back to the clipboard.
        let [Command::SetClipboardEntry(Some(ClipboardEntry::Copy(paths)))] = commands.as_slice()
        else {
            panic!("expected SetClipboardEntry(Copy), got {commands:?}");
        };
        assert_eq!(&vec![fx.other.clone()], paths);
        await_terminal_task(&rx);
        assert_eq!(b"dest".to_vec(), fx.pasted("b.txt"));
    }

    /// Builds a cancel stack from a compact description: `t` is a file
    /// operation, `x` one already cancelled and still unwinding, `s` a search,
    /// in registration order.
    fn cancellables(kinds: &str) -> Vec<Cancellable> {
        kinds
            .chars()
            .map(|kind| match kind {
                't' | 'x' => {
                    let token = CancellationToken::new();
                    if kind == 'x' {
                        token.cancel();
                    }
                    Cancellable::Task(CancelInfo {
                        id: 0,
                        token,
                        kind: crate::command::progress::TaskKind::Delete {
                            path: String::new(),
                        },
                        uncancellable: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                            false,
                        )),
                    })
                }
                _ => Cancellable::Search(CancellationToken::new()),
            })
            .collect()
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
    fn the_cancel_key_targets(kinds: &str) -> Option<usize> {
        // File operations share one worker and run in queue order, so the
        // oldest is the one actually running. Cancelling the newest would stop
        // work that has not started while the copy on screen carries on.
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

        // Every prompt broadcasts CancelPrompt on Esc, so with no paste waiting
        // this must fall through to the view that owns the prompt.
        let result = file_system.handle_command(&Command::CancelPrompt);

        assert!(matches!(result, CommandResult::NotHandled));
    }

    #[test]
    fn a_paste_that_asked_nothing_keeps_no_state_for_a_later_prompt_to_disturb() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_paste_keeps_nothing");

        // Nothing collides, so the queue hands out every source and is done.
        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone(), fx.other.clone()],
            dest: fx.dest.clone(),
        });
        assert!(file_system.pending_paste.is_none());

        // The copies may still be running, but nothing here is waiting on an
        // answer, so dismissing a rename or a filter must not reach them.
        let result = file_system.handle_command(&Command::CancelPrompt);

        assert!(matches!(result, CommandResult::NotHandled));
        await_terminal_task(&rx);
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
    }

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
        let srcs = vec![fx.other.clone(), fx.src.clone(), twin.clone()];
        let paste = if is_move {
            Command::Move {
                srcs,
                dest: fx.dest.clone(),
            }
        } else {
            Command::Copy {
                srcs,
                dest: fx.dest.clone(),
            }
        };

        let commands = file_system.handle_command(&paste).into_commands();
        assert_eq!(("b.txt", true), conflict_prompt(&commands));
        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::OverwriteAll))
            .into_commands();
        await_terminal_task(&rx);
        await_terminal_task(&rx);

        // The standing "overwrite all" covers only what was in the destination
        // before the paste: the twin would replace the file `src/a.txt` just
        // put there, which for a cut is its only copy.
        let operation = if is_move { "move" } else { "copy" };
        let refusal = format!(
            "Cannot {operation} {} into {}: another source in this paste has the same name",
            compact(&twin.path),
            compact(&fx.dest.path)
        );
        assert!(
            commands.contains(&Command::AlertError(refusal)),
            "{commands:?}"
        );
        assert_eq!(b"src".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"twin".to_vec(), fs::read(&twin.path).unwrap());
        assert_eq!(is_move, !fx.src.path.exists());
        // The refused source stays on the clipboard, so it can be pasted
        // somewhere else.
        let entry = if is_move {
            ClipboardEntry::Move(vec![twin])
        } else {
            ClipboardEntry::Copy(vec![twin])
        };
        assert_eq!(
            Some(&Command::SetClipboardEntry(Some(entry))),
            commands.last()
        );
    }

    #[test]
    fn a_source_whose_task_never_started_claims_no_name() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_failed_claim");
        // A second marked source of the same name, which search results make
        // easy to end up with. This one exists.
        let elsewhere = fx.dest.path.parent().expect("a parent").join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("missing.txt"), b"twin").unwrap();
        let twin = PathInfo::try_from(elsewhere.join("missing.txt").as_path()).unwrap();

        let result = file_system.handle_command(&Command::Copy {
            srcs: vec![fx.missing.clone(), twin],
            dest: fx.dest.clone(),
        });

        // The vanished source writes nothing, so the name stays free and the
        // second source just runs. Claiming a name for a task that never
        // started would ask about a collision that does not exist, and for a
        // directory it would offer no way to proceed at all.
        let commands = result.into_commands();
        assert!(
            !commands.iter().any(|command| matches!(
                command,
                Command::OpenPrompt(PromptAction::Conflict { .. })
            )),
            "unexpected conflict prompt: {commands:?}"
        );
        await_terminal_task(&rx);
        assert_eq!(b"twin".to_vec(), fx.pasted("missing.txt"));
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
    fn a_refresh_during_a_load_waits_for_it_instead_of_restarting_it() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let root = TempDir::new("fs_reload");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        let load_token = CancellationToken::new();
        file_system.current_load = Some((7, load_token.clone()));

        let result = file_system.handle_command(&Command::RefreshDirectory);

        // Restarting would cancel the load before it can finalize, and the
        // sort and the end of the loading state both hang off that completion.
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

        // The change that triggered the refresh may have landed after the load
        // opened the directory, so it still has to be re-read; deferring it is
        // not dropping it.
        let command = Command::try_from(result).expect("expected a derived command");
        let Command::RefreshedDirectory {
            directory,
            generation,
        } = command
        else {
            panic!("expected RefreshedDirectory, got {command:?}");
        };
        assert_eq!(root.path(), directory.as_path());
        // A generation of its own. Reusing the one that just completed would
        // let the finished load's trailing batches stream into the reload.
        assert_ne!(7, generation);
        assert_eq!(Some(generation), file_system.current_load.map(|(id, _)| id));
        assert!(!file_system.reload_pending);
        drop(rx);
    }

    /// The directory being viewed stops being one that can be listed. Its path
    /// is left empty or given a file.
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

        assert!(
            matches!(commands.as_slice(), [Command::AlertError(_)]),
            "{commands:?}"
        );
        // Otherwise every change to the directory, wherever it went, would
        // repeat the error.
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

        // An older load finishing must not clear the current one or consume
        // the refresh waiting on it.
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
        // A fixture rather than the real temp directory: `search` spawns a
        // detached walk, and rooting it at /tmp would traverse every other
        // test's fixtures to the full production depth.
        let root = TempDir::new("fs_search_root");
        file_system.directory = Some(PathInfo::try_from(root.path()).unwrap());
        let load_token = CancellationToken::new();
        file_system.current_load = Some((1, load_token.clone()));

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
    fn a_finished_task_leaves_the_cancel_stack_and_the_rest_of_it_alone() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        file_system.cancellables = cancellables("tts");

        // A real task, so its terminal update carries the id the stack entry
        // has to be matched on.
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
        // `cancellables` gives every entry id 0, so the second takes the real
        // one and the first stands for an unrelated operation still running.
        let Cancellable::Task(second) = &mut file_system.cancellables[1] else {
            panic!("expected a task")
        };
        second.id = finished_id;

        file_system.check_progress_for_error(&finished);

        // Only the finished task goes. Dropping the others would leave a
        // running operation with nothing for the cancel key to target, and the
        // search entry is not a task at all.
        assert_eq!(2, file_system.cancellables.len());
        assert!(matches!(
            file_system.cancellables[0],
            Cancellable::Task(CancelInfo { id: 0, .. })
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

        // A superseded search exiting must not drop the entry belonging to the
        // one that replaced it, or the cancel key would find nothing to stop.
        file_system.on_search_exited(3);
        assert_eq!(2, file_system.cancellables.len());

        // The file operation is not the search's to clear.
        file_system.on_search_exited(4);
        assert!(matches!(
            file_system.cancellables.as_slice(),
            [Cancellable::Task(_)]
        ));
    }

    fn task_info(file_system: &FileSystem, index: usize) -> &CancelInfo {
        match &file_system.cancellables[index] {
            Cancellable::Task(info) => info,
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
        let Cancellable::Task(info) = &mut file_system.cancellables[0] else {
            panic!("expected a task")
        };
        info.id = initial.id();

        file_system.handle_command(&Command::CancelTask);

        // Still unwinding, so quit must still ask; but not a target again.
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
        assert!(running.is_cancelled());
        assert!(!queued.is_cancelled());
        assert_eq!(2, file_system.cancellables.len());
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

        // Its terminal update is what removes it, so it stays on the stack.
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

        // Silent, and still registered: its ExitedSearch is already on the
        // way and drops the entry.
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
        // Only the new search's exit may clear the search state.
        assert_eq!(*generation, file_system.current_search_generation);
        assert!(previous.is_cancelled());
        let [Cancellable::Search(current)] = file_system.cancellables.as_slice() else {
            panic!("expected only the new search to be registered");
        };
        // Not `!current.is_cancelled()`: the new search's thread cancels its
        // own token when it finishes, which on an empty root can be at once.
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

        // The failure rides the same broadcast as the refresh, ahead of it.
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

        // A navigation rather than a refresh, so "-" can return to sub.
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

        // The new load reads the new directory; re-reading it once that
        // completes would be a second load for nothing.
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

        // Re-entering the directory already shown is not a move, so recording
        // it would make "-" toggle back to where the user already is.
        file_system.cd(second, true);
        assert_eq!(Some(first.path), previous_path(&file_system));
    }

    /// The search's token is registered by hand rather than by a real walk,
    /// which cancels its own token when it finishes and could do so before
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

        // Left registered, it would stay the cancel key's target after its
        // results were replaced.
        assert!(search.is_cancelled());
        assert!(matches!(
            file_system.cancellables.as_slice(),
            [Cancellable::Task(_)]
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
