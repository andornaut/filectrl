mod debounce;
mod handler;
mod operations;
pub mod path_info;
mod search;
mod tasks;
mod watch;

use std::{fmt::Display, fs, path::PathBuf, sync::mpsc::Sender, thread, time::Duration};

use anyhow::{Result, anyhow};
use log::warn;

use self::{
    operations::open_in,
    path_info::PathInfo,
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
    buffer_max_bytes: u64,
    buffer_min_bytes: u64,
    cancellables: Vec<Cancellable>,
    command_tx: Sender<Command>,
    directory: Option<PathInfo>,
    previous_directory: Option<PathInfo>,
    open_current_directory_template: String,
    open_new_window_template: String,
    open_selected_file_template: String,
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
            buffer_max_bytes: config.file_system.buffer_max_bytes,
            buffer_min_bytes: config.file_system.buffer_min_bytes,
            cancellables: Vec::new(),
            command_tx,
            directory: None,
            previous_directory: None,
            open_current_directory_template: config.openers.open_current_directory.clone(),
            open_new_window_template: config.openers.open_new_window.clone(),
            open_selected_file_template: config.openers.open_selected_file.clone(),
            watcher,
        }
    }

    pub fn run_once(&mut self, directory: Option<PathBuf>) -> Result<Command> {
        if let Some(watcher) = &mut self.watcher {
            watcher.run_once(&self.command_tx);
        }

        let directory = directory
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

        self.cd(directory, true).try_into()
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
        match operations::cd(&directory) {
            Ok((children, error_count)) => {
                if error_count > 0 {
                    let _ = self.command_tx.send(Command::AlertWarn(format!(
                        "{error_count} entries in {directory:?} could not be read"
                    )));
                }
                // Track the directory we're leaving so "-" can toggle back to it.
                if navigate
                    && let Some(current) = &self.directory
                    && current.path != directory.path
                {
                    self.previous_directory = Some(current.clone());
                }
                self.directory = Some(directory.clone());
                let path_buf = PathBuf::from(&directory.path);
                if let Some(watcher) = &mut self.watcher
                    && let Err(e) = watcher.watch_directory(path_buf.clone())
                {
                    self.send_directory_error(&path_buf, e);
                }
                if navigate {
                    Command::NavigatedDirectory {
                        directory,
                        children,
                    }
                } else {
                    Command::RefreshedDirectory {
                        directory,
                        children,
                    }
                }
            }
            Err(error) => anyhow!("Failed to change to directory {directory:?}: {error}").into(),
        }
        .into()
    }

    /// Full search teardown (Esc / `ResetView`): cancel and drop every search
    /// entry. No-op if the search was already cancelled via `cancel_task`.
    fn cancel_search(&mut self) {
        self.cancellables.retain(|c| match c {
            Cancellable::Search(token) => {
                token.cancel();
                false
            }
            Cancellable::Task(_) => true,
        });
    }

    fn cancel_most_recent_task(&mut self) -> CommandResult {
        // LIFO across file operations and search: cancel whichever was started most recently.
        while let Some(cancellable) = self.cancellables.last() {
            match cancellable {
                Cancellable::Task((_, token, _)) => {
                    if !token.is_cancelled() {
                        token.cancel();
                        let Some(Cancellable::Task((_, _, kind))) = self.cancellables.pop() else {
                            unreachable!()
                        };
                        return Command::AlertInfo(format!("Cancelled: {}", kind.message())).into();
                    }
                    self.cancellables.pop();
                }
                Cancellable::Search(token) => {
                    token.cancel();
                    self.cancellables.pop();
                    // Non-destructive: keep streamed results and the notice;
                    // NoticesView relabels it to "Cancelled: [Searching] <query>".
                    return Command::CancelSearch.into();
                }
            }
        }
        Command::AlertWarn("No active task to cancel".into()).into()
    }

    fn check_progress_for_error(&mut self, task: &Task) -> CommandResult {
        if task.is_terminal() {
            self.cancellables.retain(|c| match c {
                Cancellable::Task((id, _, _)) => *id != task.id(),
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
                    open_in(
                        &path,
                        &self.open_selected_file_template,
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
            self.current_directory(),
            &self.open_current_directory_template,
            self.command_tx.clone(),
        )
        .into()
    }

    fn open_new_window(&self) -> CommandResult {
        open_in(
            self.current_directory(),
            &self.open_new_window_template,
            self.command_tx.clone(),
        )
        .into()
    }

    fn chmod(&mut self, paths: &[PathInfo], mode_str: &str) -> CommandResult {
        let Some(mode) = parse_octal_mode(mode_str) else {
            return anyhow!("Invalid octal mode: {mode_str:?}").into();
        };
        for path in paths {
            if let Err(error) = operations::chmod(path, mode) {
                let _ = self
                    .command_tx
                    .send(anyhow!("Failed to chmod {path:?} to {mode_str}: {error}").into());
            }
        }
        self.refresh()
    }

    fn add_bookmark(&mut self, target: &PathInfo, name: &str) -> CommandResult {
        match operations::add_bookmark(target, name) {
            Err(error) => Command::AlertError(error.to_string()).into(),
            Ok(_) => Command::AlertInfo(format!("Bookmark {name:?} added")).into(),
        }
    }

    /// Read every entry in the bookmarks directory and return them as a single
    /// `Bookmarks` command. Synchronous: one small directory of symlinks, no
    /// streaming.
    fn get_bookmarks(&self) -> CommandResult {
        let dir = Config::global().bookmarks_dir();
        if let Err(error) = fs::create_dir_all(&dir) {
            return Command::AlertError(format!(
                "Cannot create bookmarks directory {dir:?}: {error}"
            ))
            .into();
        }
        match fs::read_dir(&dir) {
            Ok(entries) => Command::Bookmarks {
                bookmarks: entries
                    .flatten()
                    .filter_map(|entry| PathInfo::try_from(&entry.path()).ok())
                    .collect(),
            }
            .into(),
            Err(error) => {
                Command::AlertError(format!("Cannot read bookmarks directory {dir:?}: {error}"))
                    .into()
            }
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

    fn run_batch(&mut self, tasks: impl Iterator<Item = TaskCommand>) {
        for task in tasks {
            let result = task.run(
                self.command_tx.clone(),
                self.buffer_min_bytes,
                self.buffer_max_bytes,
            );
            if let Some(cancel_info) = result.cancel_info {
                self.cancellables.push(Cancellable::Task(cancel_info));
            }
            // Send initial progress commands through the channel so NoticesView picks them up
            if let CommandResult::HandledWith(cmd) = result.command_result {
                let _ = self.command_tx.send(*cmd);
            }
        }
    }

    fn search(&mut self, query: &str) {
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
            self.current_directory().clone(),
            query.to_string(),
            self.command_tx.clone(),
            token,
        );
    }

    fn send_directory_error(&self, dir: &PathBuf, error: impl Display) {
        let _ = self.command_tx.send(Command::AlertWarn(format!(
            "Failed to read directory {dir:?}: {error}"
        )));
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
    use super::*;

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
