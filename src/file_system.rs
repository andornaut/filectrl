mod conflicts;
mod debounce;
mod handler;
pub mod open_with;
mod operations;
pub mod path_info;
mod search;
mod shell;
mod stream;
mod tasks;
mod watch;

use std::{
    collections::{HashMap, VecDeque},
    ffi::OsString,
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
    conflicts::Conflicts,
    operations::{open_in, spawn_argv},
    path_info::{PathInfo, compact},
    search::Limits,
    tasks::{CancelInfo, TaskCommand},
    watch::DirectoryWatcher,
};
use crate::{
    app::{clipboard::ClipboardEntry, config::Config},
    command::{
        Command, ConflictChoice, PromptAction,
        progress::{CancellationToken, Task},
        result::CommandResult,
    },
};

/// A cancellable in-flight action. File operations and searches share one list,
/// in registration order; `cancel_target` decides which entry a cancel keypress
/// aims at.
enum Cancellable {
    Task(CancelInfo),
    Search(CancellationToken),
}

/// A paste running one source at a time, so a name that is already taken in the
/// destination can be answered for before the next source starts. Held only
/// while the conflict prompt is open: `advance_paste` takes it, and puts it back
/// only when it needs an answer.
struct PendingPaste {
    /// `Move` when true, `Copy` when false. Decides both the task kind and
    /// which clipboard entry an unfinished paste leaves behind.
    is_move: bool,
    dest: PathInfo,
    /// Sources not yet processed. The one being asked about stays at the front
    /// until the answer pops it.
    remaining: VecDeque<PathInfo>,
    /// Sources that could not be started, kept so a retry carries only them.
    failed: Vec<PathInfo>,
    /// How many tasks actually started, which decides whether the clipboard is
    /// cleared, reduced, or left alone.
    started: usize,
    /// The paste's standing `*All` answer, shared with the workers so one given
    /// here also settles a name another process takes inside a tree already
    /// being copied.
    conflicts: Conflicts,
    /// Destination names already spoken for by sources queued earlier in this
    /// paste. Their work is queued but may not have run, so the filesystem does
    /// not show them yet; without this, two marked sources sharing a basename
    /// (which search results make easy) would both see a free name and the
    /// second would fail at the copy instead of being asked about.
    claimed: HashMap<OsString, Occupant>,
}

/// What already holds a source's name in the destination directory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Occupant {
    /// A directory, which is never replaced: removing it would take its
    /// contents with it, and it is never merged into either.
    Directory,
    /// A file, symlink, or other non-directory, which the user may replace.
    Replaceable,
}

/// What a paste should do with the source at the front of its queue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PasteStep {
    /// Stop and ask. `can_overwrite` is false for a directory, whose
    /// replacement is never offered.
    Ask { can_overwrite: bool },
    /// Drop the source without running anything.
    Skip,
    /// Run the source, replacing what is at its destination when `overwrite`.
    Run { overwrite: bool },
}

impl PendingPaste {
    /// What holds `src`'s destination name, counting names an earlier source in
    /// this same paste has already claimed. Replacing a claimed name is safe
    /// because the worker runs the sources in order, so it removes what the
    /// earlier source wrote rather than racing it.
    fn occupant(&self, src: &PathInfo) -> Option<Occupant> {
        existing_destination(&self.dest, src).or_else(|| {
            let name = src.path.file_name()?;
            self.claimed.get(name).copied()
        })
    }

    /// Records that `src`'s destination name is spoken for, once its work is
    /// actually running. The claim carries the source's own kind: that is what
    /// its work will leave at the name, so a directory claimed here is no more
    /// replaceable than one already on disk.
    fn claim(&mut self, src: &PathInfo) {
        if let Some(name) = src.path.file_name() {
            let occupant = if src.is_directory() {
                Occupant::Directory
            } else {
                Occupant::Replaceable
            };
            self.claimed.insert(name.to_os_string(), occupant);
        }
    }

    /// What to do with the source at the front of the queue, given what is
    /// already at its destination. Pure, so the whole answer matrix can be
    /// exercised without a filesystem or a worker.
    fn step(&self, occupant: Option<Occupant>) -> PasteStep {
        step(self.conflicts.standing(), occupant)
    }

    /// Records the answer to the collision in front of the user. Returns
    /// whether the answered source runs, replacing its destination. An `*All`
    /// also reaches the sources already handed to a worker.
    fn answer(&mut self, choice: ConflictChoice) -> bool {
        self.conflicts.answer(choice);
        conflicts::replaces(choice)
    }

    /// The clipboard follow-up once the paste is finished or abandoned. Nothing
    /// started leaves the clipboard untouched so the paste can be retried
    /// as-is; a clean run clears it; a partial run reduces it to what was not
    /// pasted, because a full retry would collide with the destinations just
    /// created.
    fn clipboard_follow_up(self) -> Option<Command> {
        if self.started == 0 {
            return None;
        }
        if self.failed.is_empty() {
            return Some(Command::SetClipboardEntry(None));
        }
        let entry = if self.is_move {
            ClipboardEntry::Move(self.failed)
        } else {
            ClipboardEntry::Copy(self.failed)
        };
        Some(Command::SetClipboardEntry(Some(entry)))
    }
}

/// What to do with a source, given the paste's standing answer and what already
/// holds its destination name. Pure, so the whole answer matrix can be
/// exercised without a filesystem or a worker.
///
/// Shared with the workers, which apply it to a name another process takes
/// inside a tree they are already copying. `Ask` is the one outcome a worker
/// cannot act on, so it records the collision instead.
fn step(standing: Option<ConflictChoice>, occupant: Option<Occupant>) -> PasteStep {
    let Some(occupant) = occupant else {
        return PasteStep::Run { overwrite: false };
    };
    let can_overwrite = occupant == Occupant::Replaceable;
    match standing {
        Some(ConflictChoice::SkipAll) => PasteStep::Skip,
        // "Overwrite all" cannot answer for a directory, so that collision is
        // still asked about.
        Some(ConflictChoice::OverwriteAll) if can_overwrite => PasteStep::Run { overwrite: true },
        _ => PasteStep::Ask { can_overwrite },
    }
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
    /// The in-flight streamed directory load: its generation, and the token
    /// that stops it. Cancelled when a new load starts so stale batches don't
    /// bleed across, and cleared when the load reports itself complete.
    current_load: Option<(u64, CancellationToken)>,
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
                "Failed to change to directory {}: {error}",
                compact(&directory.path)
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
        self.current_load = Some((generation, token.clone()));
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
        if std::mem::take(&mut self.reload_pending) {
            return self.refresh();
        }
        CommandResult::NotHandled
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

    /// The entry a cancel keypress targets, so it always aims at work that is
    /// actually running rather than at whatever was registered last.
    ///
    /// A search runs alongside everything else, so when one is the most recent
    /// thing started it is what the keypress means. File operations share a
    /// single worker and run in the order they were queued, so the *oldest*
    /// registered one is the one running: a batch registers every source at
    /// once, and cancelling the newest would stop work that has not started
    /// while the copy the user is watching carries on.
    fn cancel_target(&self) -> Option<usize> {
        match self.cancellables.last()? {
            Cancellable::Search(_) => Some(self.cancellables.len() - 1),
            Cancellable::Task(_) => self
                .cancellables
                .iter()
                .position(|cancellable| matches!(cancellable, Cancellable::Task(_))),
        }
    }

    fn cancel_most_recent_task(&mut self) -> CommandResult {
        let Some(index) = self.cancel_target() else {
            return Command::AlertWarn("No active task to cancel".into()).into();
        };
        match self.cancellables.remove(index) {
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
                    self.cancellables.insert(index, Cancellable::Task(info));
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
                    self.cancellables.insert(index, Cancellable::Search(token));
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
    fn open_with(
        &self,
        working_dir: Option<&Path>,
        label: &str,
        argv: &[OsString],
    ) -> CommandResult {
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
                operations::chmod(path, mode).err().map(|error| {
                    anyhow!(
                        "Failed to chmod {} to {mode_str}: {error}",
                        compact(&path.path)
                    )
                    .into()
                })
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
            Err(error) => anyhow!(
                "Failed to rename {} to {new_basename:?}: {error}",
                compact(&path.path)
            )
            .into(),
            Ok(_) => self.refresh(),
        }
    }

    fn refresh(&mut self) -> CommandResult {
        // A load for this directory is already streaming and will pick the
        // change up. Restarting it would cancel it before it can finalize, and
        // under sustained churn (a build writing into the directory being
        // viewed) it would never finalize at all: both the sort and the end of
        // the loading state hang off the completion. The refresh is re-issued
        // when the load reports in.
        if self.current_load.is_some() {
            self.reload_pending = true;
            return CommandResult::Handled;
        }
        self.cd(self.current_directory().clone(), false)
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
        let result = task.run(
            self.command_tx.clone(),
            conflicts,
            self.buffer_min_bytes,
            self.buffer_max_bytes,
        );
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
            claimed: HashMap::new(),
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
    /// keeps going.
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
    /// one is actually waiting on an answer: dismissing a rename or a filter
    /// leaves a running paste alone. The standing answer is left as it is, so
    /// an `*All` already given still covers the sources already handed out.
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
    /// The destination name is claimed only once the task is running. A source
    /// that failed validation writes nothing, so claiming its name would make a
    /// later source of the same name collide with something that is never going
    /// to be there.
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

    fn send_directory_error(&self, dir: &Path, error: impl Display) {
        let _ = self.command_tx.send(Command::AlertWarn(format!(
            "Failed to read directory {}: {error}",
            compact(dir)
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
            "Cannot create bookmarks directory {}: {error}",
            compact(dir)
        ));
    }
    let entries = fs::read_dir(dir)
        .map_err(|error| format!("Cannot read bookmarks directory {}: {error}", compact(dir)))?;
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

/// What already holds `src`'s name in `dest`, or `None` when the name is free.
/// Links are not followed, so a symlink to a directory reports `Replaceable`
/// and is replaced as a link rather than treated as the directory it points at.
fn existing_destination(dest: &PathInfo, src: &PathInfo) -> Option<Occupant> {
    let name = src.path.file_name()?;
    let destination = dest.path.join(name);
    let metadata = destination.symlink_metadata().ok()?;
    // Pasting into the source's own directory finds the source itself. That is
    // not a collision to ask about: the operation is refused outright, so
    // offering to replace it would promise something that cannot happen and
    // would let an "overwrite all" answer stand on a collision that was never
    // real. Compared canonically, since either path may reach the entry
    // through a symlinked parent.
    if is_same_entry(&destination, &src.path) {
        return None;
    }
    Some(if metadata.is_dir() {
        Occupant::Directory
    } else {
        Occupant::Replaceable
    })
}

/// Whether both paths name the same directory entry once symlinks are resolved.
/// False when either cannot be resolved, which for an existing path means only
/// a dangling link or an unreadable parent.
fn is_same_entry(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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

    use test_case::test_case;

    use super::*;
    use crate::{command::handler::CommandHandler, test_support::TempDir};

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
    struct CopyFixture {
        _dir: TempDir,
        src: PathInfo,
        other: PathInfo,
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
        fn occupy(&self, name: &str) {
            fs::write(self.dest.path.join(name), b"dest").unwrap();
        }

        /// Puts a directory at `name` in the destination: a collision that is
        /// never replaced, whatever the user answers.
        fn occupy_with_directory(&self, name: &str) {
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

    // ── the paste decision, with no filesystem and no worker ─────────────────

    fn pending(standing: Option<ConflictChoice>) -> PendingPaste {
        let conflicts = Conflicts::default();
        if let Some(standing) = standing {
            conflicts.answer(standing);
        }
        PendingPaste {
            is_move: false,
            dest: PathInfo::try_from(Path::new("/")).unwrap(),
            remaining: VecDeque::new(),
            failed: Vec::new(),
            started: 0,
            conflicts,
            claimed: HashMap::new(),
        }
    }

    #[test_case(None, None => PasteStep::Run { overwrite: false } ; "a free name just runs")]
    #[test_case(None, Some(Occupant::Replaceable) => PasteStep::Ask { can_overwrite: true } ; "a file asks, offering overwrite")]
    #[test_case(None, Some(Occupant::Directory) => PasteStep::Ask { can_overwrite: false } ; "a directory asks, withholding overwrite")]
    #[test_case(Some(ConflictChoice::SkipAll), None => PasteStep::Run { overwrite: false } ; "skip all does not skip a free name")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Replaceable) => PasteStep::Skip ; "skip all skips a file")]
    #[test_case(Some(ConflictChoice::SkipAll), Some(Occupant::Directory) => PasteStep::Skip ; "skip all skips a directory")]
    #[test_case(Some(ConflictChoice::OverwriteAll), None => PasteStep::Run { overwrite: false } ; "overwrite all does not force a free name")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Replaceable) => PasteStep::Run { overwrite: true } ; "overwrite all replaces a file")]
    #[test_case(Some(ConflictChoice::OverwriteAll), Some(Occupant::Directory) => PasteStep::Ask { can_overwrite: false } ; "overwrite all still asks about a directory")]
    fn the_paste_step_matrix(
        standing: Option<ConflictChoice>,
        occupant: Option<Occupant>,
    ) -> PasteStep {
        step(standing, occupant)
    }

    #[test]
    fn a_name_claimed_earlier_in_the_paste_counts_as_taken() {
        let fx = CopyFixture::new("fs_claimed");
        let mut pending = pending(None);
        pending.dest = fx.dest.clone();
        // Two marked sources can share a basename when the marks span
        // directories, which search results make easy.
        let mut twin = fx.src.clone();
        twin.path = fx
            .dest
            .path
            .parent()
            .unwrap()
            .join("elsewhere")
            .join("a.txt");

        assert_eq!(None, pending.occupant(&twin));
        pending.claim(&fx.src);

        // The first source's work is only queued, so the filesystem still
        // shows the name as free. Without the claim the second source would
        // fail at the copy instead of being asked about.
        assert_eq!(Some(Occupant::Replaceable), pending.occupant(&twin));
    }

    #[test]
    fn a_directory_claimed_earlier_in_the_paste_is_never_offered_a_replace() {
        let fx = CopyFixture::new("fs_claimed_directory");
        let parent = fx.dest.path.parent().unwrap().to_path_buf();
        let mut pending = pending(None);
        pending.dest = fx.dest.clone();
        // A directory source, and a second marked source of the same name.
        let source = PathInfo::try_from(parent.join("src").as_path()).unwrap();
        let mut twin = source.clone();
        twin.path = parent.join("elsewhere").join("src");

        pending.claim(&source);

        // The claim carries the source's kind, so this is the same collision
        // as a directory already on disk: replacing it would mean removing the
        // one the earlier source is busy creating.
        assert_eq!(Some(Occupant::Directory), pending.occupant(&twin));
        assert_eq!(
            PasteStep::Ask {
                can_overwrite: false
            },
            pending.step(pending.occupant(&twin))
        );
    }

    #[test]
    fn a_later_all_answer_replaces_the_standing_one() {
        let mut pending = pending(Some(ConflictChoice::OverwriteAll));

        pending.answer(ConflictChoice::SkipAll);

        // Deliberate: "skip all" is answered for the whole batch, so it
        // supersedes an earlier "overwrite all". A directory collision is the
        // way this comes up, since it reopens the prompt with only the skip
        // choices; answering it with the single-entry `s` leaves the standing
        // answer alone.
        assert_eq!(Some(ConflictChoice::SkipAll), pending.conflicts.standing());
        pending.answer(ConflictChoice::Skip);
        assert_eq!(Some(ConflictChoice::SkipAll), pending.conflicts.standing());
    }

    #[test_case(ConflictChoice::Skip => (false, None) ; "skip runs nothing and does not stand")]
    #[test_case(ConflictChoice::Overwrite => (true, None) ; "overwrite runs and does not stand")]
    #[test_case(ConflictChoice::SkipAll => (false, Some(ConflictChoice::SkipAll)) ; "skip all runs nothing and stands")]
    #[test_case(ConflictChoice::OverwriteAll => (true, Some(ConflictChoice::OverwriteAll)) ; "overwrite all runs and stands")]
    fn an_answer_decides_the_source_and_whether_it_stands(
        choice: ConflictChoice,
    ) -> (bool, Option<ConflictChoice>) {
        let mut pending = pending(None);
        let runs = pending.answer(choice);
        (runs, pending.conflicts.standing())
    }

    /// A paste with `started` tasks started and `failed` sources that could not.
    fn finished(is_move: bool, started: usize, failed: Vec<PathInfo>) -> Option<Command> {
        PendingPaste {
            is_move,
            dest: PathInfo::try_from(Path::new("/")).unwrap(),
            remaining: VecDeque::new(),
            failed,
            started,
            conflicts: Conflicts::default(),
            claimed: HashMap::new(),
        }
        .clipboard_follow_up()
    }

    #[test]
    fn nothing_started_leaves_the_clipboard_alone() {
        let src = PathInfo::try_from(Path::new("/")).unwrap();
        // The paste is retried as-is, so the clipboard must not be touched.
        assert_eq!(None, finished(false, 0, Vec::new()));
        assert_eq!(None, finished(false, 0, vec![src]));
    }

    #[test]
    fn a_clean_run_clears_the_clipboard() {
        assert_eq!(
            Some(Command::SetClipboardEntry(None)),
            finished(false, 2, Vec::new())
        );
    }

    #[test]
    fn a_partial_run_reduces_the_clipboard_to_what_was_not_pasted() {
        let src = PathInfo::try_from(Path::new("/")).unwrap();
        // A full retry would collide with the destinations just created, so
        // only what was not pasted is kept, under the original operation.
        assert_eq!(
            Some(Command::SetClipboardEntry(Some(ClipboardEntry::Copy(
                vec![src.clone()]
            )))),
            finished(false, 1, vec![src.clone()])
        );
        assert_eq!(
            Some(Command::SetClipboardEntry(Some(ClipboardEntry::Move(
                vec![src.clone()]
            )))),
            finished(true, 1, vec![src])
        );
    }

    #[test]
    fn a_destination_is_classified_by_what_holds_the_name() {
        let fx = CopyFixture::new("fs_occupant");
        assert_eq!(None, existing_destination(&fx.dest, &fx.src));

        fx.occupy("a.txt");
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src)
        );

        fx.occupy_with_directory("b.txt");
        assert_eq!(
            Some(Occupant::Directory),
            existing_destination(&fx.dest, &fx.other)
        );
    }

    #[test]
    fn pasting_into_the_source_directory_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_self");
        let src_dir = PathInfo::try_from(fx.src.path.parent().unwrap()).unwrap();

        // The destination name resolves to the source itself, which the
        // operation refuses outright. Reporting a collision would offer to
        // replace the very file being pasted, and an "overwrite all" answer
        // would then stand for the rest of the batch on the strength of it.
        assert_eq!(None, existing_destination(&src_dir, &fx.src));
    }

    #[test]
    fn pasting_into_a_symlink_to_the_source_directory_is_not_a_collision() {
        let fx = CopyFixture::new("fs_occupant_self_link");
        let src_dir = fx.src.path.parent().unwrap();
        let link = fx.dest.path.parent().unwrap().join("link");
        std::os::unix::fs::symlink(src_dir, &link).unwrap();
        let aliased = PathInfo::try_from(link.as_path()).unwrap();

        // Same entry, reached through a symlinked parent.
        assert_eq!(None, existing_destination(&aliased, &fx.src));
    }

    #[test]
    fn a_symlinked_directory_in_the_way_is_replaceable() {
        let fx = CopyFixture::new("fs_occupant_symlink");
        fx.occupy_with_directory("target");
        std::os::unix::fs::symlink(fx.dest.path.join("target"), fx.dest.path.join("a.txt"))
            .unwrap();

        // Replacing it unlinks the link, which does not touch the directory it
        // points at, so the overwrite choices stay available.
        assert_eq!(
            Some(Occupant::Replaceable),
            existing_destination(&fx.dest, &fx.src)
        );
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
    fn overwrite_moves_a_directory_over_an_existing_file() {
        let bookmarks = TempDir::reserved("fs_bookmarks");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut file_system = test_file_system(&bookmarks, tx);
        let fx = CopyFixture::new("fs_move_dir_over_file");
        let src_dir = fx.src.path.parent().unwrap().join("adir");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("inner.txt"), b"src").unwrap();
        let src = PathInfo::try_from(src_dir.as_path()).unwrap();
        // A plain file is `Occupant::Replaceable`, so the prompt offers to
        // replace it whatever the source's type is.
        fs::write(fx.dest.path.join("adir"), b"dest").unwrap();
        file_system.handle_command(&Command::Move {
            srcs: vec![src],
            dest: fx.dest.clone(),
        });

        file_system.handle_command(&Command::ResolveConflict(ConflictChoice::Overwrite));

        // `rename` refuses to replace a file with a directory (ENOTDIR), so
        // granting overwrite would promise a replacement the move could not
        // deliver unless the destination is cleared first.
        await_terminal_task(&rx);
        assert_eq!(
            b"src".to_vec(),
            fs::read(fx.dest.path.join("adir").join("inner.txt")).unwrap()
        );
        assert!(!src_dir.exists(), "the source should have been moved");
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
        file_system.handle_command(&Command::Copy {
            srcs: vec![fx.src.clone(), fx.other.clone()],
            dest: fx.dest.clone(),
        });

        let commands = file_system
            .handle_command(&Command::ResolveConflict(ConflictChoice::SkipAll))
            .into_commands();

        // The second collision must not prompt, and nothing started, so the
        // clipboard is left alone.
        assert!(commands.is_empty(), "{commands:?}");
        assert!(file_system.pending_paste.is_none());
        assert_eq!(b"dest".to_vec(), fx.pasted("a.txt"));
        assert_eq!(b"dest".to_vec(), fx.pasted("b.txt"));
        assert!(rx.try_recv().is_err());
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
    /// operation, `s` a search, in registration order.
    fn cancellables(kinds: &str) -> Vec<Cancellable> {
        kinds
            .chars()
            .map(|kind| match kind {
                't' => Cancellable::Task(CancelInfo {
                    id: 0,
                    token: CancellationToken::new(),
                    kind: crate::command::progress::TaskKind::Delete {
                        path: String::new(),
                    },
                    uncancellable: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                }),
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
        assert!(matches!(command, Command::RefreshedDirectory { .. }));
        assert!(!file_system.reload_pending);
        drop(rx);
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
