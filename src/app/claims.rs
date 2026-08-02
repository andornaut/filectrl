//! The command-claim invariant: every broadcast command must be claimed by at
//! least one handler.
//!
//! `App::run` treats an unclaimed command as fatal (`must_not_contain_unhandled`),
//! but several handler arms mutate state and then return `NotHandled`: they act
//! on a command without claiming it. For some commands that leaves exactly one
//! claiming arm in the entire tree (`CancelPrompt` is claimed only by
//! `RootView`, `SetClipboardEntry` only by `Handlers` itself), and nothing at
//! those call sites says so. This drives the real handler tree so that removing
//! a sole claimant fails here rather than exiting the app mid-session.

use std::{path::PathBuf, sync::mpsc};

use super::*;
use crate::{
    app::{
        clipboard::ClipboardEntry,
        config::{Openers, RuntimeEnv},
    },
    command::{
        PromptAction,
        progress::{ActiveTask, TaskKind},
    },
    file_system::path_info::PathInfo,
};

/// A temp tree containing the working directory, so navigating to the parent
/// stays inside the fixture instead of walking into the real temp directory.
struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("filectrl_claims_{}_{seq}", std::process::id()));
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        std::fs::write(root.join("cwd").join("file.txt"), b"x").unwrap();
        Self { root }
    }

    fn cwd(&self) -> PathBuf {
        self.root.join("cwd")
    }

    fn directory(&self) -> PathInfo {
        PathInfo::try_from(self.cwd().as_path()).unwrap()
    }

    fn file(&self) -> PathInfo {
        PathInfo::try_from(self.cwd().join("file.txt").as_path()).unwrap()
    }

    /// A path that does not exist, so file operations fail their pre-flight
    /// validation: the arm is still exercised, but no worker thread starts and
    /// nothing on disk changes.
    fn missing(&self) -> PathInfo {
        let mut info = self.file();
        info.path = self.cwd().join("missing.txt");
        info.display_name = "missing.txt".to_string();
        info
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The real handler tree, minus the two things that reach outside the process.
/// `FileSystem` takes its config by reference and copies out what it needs, so
/// blanking the openers is enough to stop any command from shelling out
/// (`open_in` returns early on an empty template) and redirecting `config_dir`
/// keeps bookmark reads inside the fixture.
fn test_handlers(tx: Sender<Command>, fixture: &Fixture) -> Handlers {
    let mut config = Config::load(RuntimeEnv::default(), None, vec![]).unwrap();
    config.config_dir = fixture.root.clone();
    config.openers = Openers {
        open_current_directory: String::new(),
        open_in_terminal: String::new(),
        open_new_window: String::new(),
        open_selected_file: String::new(),
    };
    let file_system = FileSystem::new(&config, tx);
    // The views read the process-global Config; the first init wins.
    Config::init(Config::load(RuntimeEnv::default(), None, vec![]).unwrap());
    Handlers {
        clipboard: Clipboard::disabled(),
        #[cfg(debug_assertions)]
        debug: debug::DebugHandler,
        file_system,
        root: RootView::new(),
    }
}

/// One instance of every `Command` variant that a handler must claim.
///
/// Deliberately absent:
/// - `Key`, `Mouse`, `Resize`: terminal input that may go unbound, which
///   `is_ignorable_unhandled` exempts.
/// - `Quit`: it must stay *unclaimed*. `App::run` detects it in the unhandled
///   list and returns before `must_not_contain_unhandled` runs, so a handler
///   that claimed it would stop the app from ever exiting.
///
/// See `every_variant_is_accounted_for` for why this list cannot silently fall
/// behind the enum.
fn claimable_commands(fixture: &Fixture, tx: &Sender<Command>) -> Vec<Command> {
    let (_active, task, _token) = ActiveTask::new(
        tx.clone(),
        TaskKind::Delete {
            path: "/x".to_string(),
        },
        100,
    );
    vec![
        Command::OpenCurrentDirectory,
        Command::OpenNewWindow,
        // Claimed by RootView, which enumerates the applications that can open
        // the path. That reads the host's MIME and desktop entry databases, but
        // it is read-only, bounded, and spawns nothing, so host variance cannot
        // affect whether the command is claimed.
        Command::OpenWithPrompt(fixture.file()),
        // The empty-argv backstop, so no process is spawned.
        Command::OpenWith {
            argv: Vec::new(),
            label: "app".to_string(),
            working_dir: None,
        },
        Command::GoToPreviousDirectory,
        Command::Open(fixture.directory()),
        Command::NavigatedDirectory {
            directory: fixture.directory(),
            generation: 1,
        },
        Command::RefreshDirectory,
        Command::RefreshedDirectory {
            directory: fixture.directory(),
            generation: 2,
        },
        Command::ListingBatch {
            items: vec![fixture.file()],
            generation: 2,
        },
        Command::DirectoryListingComplete { generation: 2 },
        Command::Chmod {
            paths: vec![fixture.file()],
            mode: "644".to_string(),
        },
        Command::Copy {
            srcs: vec![fixture.missing()],
            dest: fixture.directory(),
        },
        Command::Move {
            srcs: vec![fixture.missing()],
            dest: fixture.directory(),
        },
        Command::Paste(fixture.directory()),
        Command::CreateDirectory("created".to_string()),
        Command::ConfirmDelete,
        Command::Delete(vec![fixture.missing()]),
        Command::Rename {
            path: fixture.missing(),
            name: "renamed".to_string(),
        },
        Command::AddBookmark {
            directory: fixture.directory(),
            name: "bookmark".to_string(),
        },
        Command::GetBookmarks,
        Command::Bookmarks {
            bookmarks: vec![fixture.file()],
        },
        Command::CancelPrompt,
        Command::OpenPrompt(PromptAction::CreateDirectory),
        Command::SetClipboardEntry(Some(ClipboardEntry::Copy(vec![fixture.file()]))),
        Command::SetClipboardEntry(None),
        Command::GetClipboardText,
        Command::ClipboardText("text".to_string()),
        Command::SetClipboardText("text".to_string()),
        Command::CancelSearch,
        Command::ExitedSearch { generation: 3 },
        Command::SearchStarted { generation: 3 },
        Command::SearchTick,
        // The empty-query backstop, so no search or tick thread is spawned.
        Command::StartSearch(String::new()),
        Command::FilterChanged("f".to_string()),
        Command::SelectionChanged {
            selected: Some(fixture.file()),
            mark_count: 0,
        },
        Command::ResetView,
        Command::AlertError("e".to_string()),
        Command::AlertInfo("i".to_string()),
        Command::AlertWarn("w".to_string()),
        Command::CancelTask,
        Command::Progress(task),
        // Navigating out of the working directory comes last so the commands
        // above all run against the fixture's cwd.
        Command::GoToParentDirectory,
    ]
}

/// Exhaustive on purpose, and the reason `claimable_commands` cannot silently
/// fall behind: adding a `Command` variant fails to compile here until it is
/// either listed above or explicitly exempted.
#[allow(dead_code)]
fn every_variant_is_accounted_for(command: &Command) {
    match command {
        // Exempt (see `claimable_commands`).
        Command::Key(_, _) | Command::Mouse(_) | Command::Resize { .. } | Command::Quit => {}
        // Must be claimed.
        Command::OpenCurrentDirectory
        | Command::OpenNewWindow
        | Command::OpenWith { .. }
        | Command::OpenWithPrompt(_)
        | Command::GoToParentDirectory
        | Command::GoToPreviousDirectory
        | Command::Open(_)
        | Command::NavigatedDirectory { .. }
        | Command::RefreshDirectory
        | Command::RefreshedDirectory { .. }
        | Command::ListingBatch { .. }
        | Command::DirectoryListingComplete { .. }
        | Command::Chmod { .. }
        | Command::Copy { .. }
        | Command::Move { .. }
        | Command::Paste(_)
        | Command::CreateDirectory(_)
        | Command::ConfirmDelete
        | Command::Delete(_)
        | Command::Rename { .. }
        | Command::AddBookmark { .. }
        | Command::GetBookmarks
        | Command::Bookmarks { .. }
        | Command::CancelPrompt
        | Command::OpenPrompt(_)
        | Command::SetClipboardEntry(_)
        | Command::GetClipboardText
        | Command::ClipboardText(_)
        | Command::SetClipboardText(_)
        | Command::CancelSearch
        | Command::ExitedSearch { .. }
        | Command::SearchStarted { .. }
        | Command::SearchTick
        | Command::StartSearch(_)
        | Command::FilterChanged(_)
        | Command::SelectionChanged { .. }
        | Command::ResetView
        | Command::AlertError(_)
        | Command::AlertInfo(_)
        | Command::AlertWarn(_)
        | Command::CancelTask
        | Command::Progress(_) => {}
    }
}

#[test]
fn every_command_variant_is_claimed_by_a_handler() {
    let fixture = Fixture::new();
    let (tx, _rx) = mpsc::channel();
    let mut handlers = test_handlers(tx.clone(), &fixture);
    // Navigation and file operations resolve against a current directory.
    handlers.file_system.run_once(Some(fixture.cwd())).unwrap();

    for command in claimable_commands(&fixture, &tx) {
        // Re-read the mode: OpenPrompt changes it partway through.
        let mode = handlers.root.mode();
        let mut derived = Vec::new();
        assert!(
            recursively_handle_command(&mut derived, &command, &mode, &mut handlers),
            "no handler claims {command:?}, which `App::run` treats as fatal"
        );
    }
}

#[test]
fn quit_is_deliberately_unclaimed() {
    let fixture = Fixture::new();
    let (tx, _rx) = mpsc::channel();
    let mut handlers = test_handlers(tx, &fixture);

    let mut derived = Vec::new();
    let handled = recursively_handle_command(
        &mut derived,
        &Command::Quit,
        &InputMode::Normal,
        &mut handlers,
    );

    // `should_quit` reads the unhandled list, so claiming Quit anywhere would
    // stop the app from exiting.
    assert!(!handled);
    assert!(derived.is_empty());
}
