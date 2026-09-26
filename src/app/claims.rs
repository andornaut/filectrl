//! Every broadcast command must be claimed by at least one handler, and some
//! have exactly one claimant (`CancelPrompt`: `RootView`; `SetClipboardEntry`:
//! `Handlers`). Drives the real handler tree so losing a sole claimant fails
//! here rather than ending a session.

use super::*;
use crate::{
    app::{clipboard::ClipboardEntry, config::Openers},
    command::{
        ConflictChoice, PromptAction,
        progress::{ActiveTask, TaskKind},
    },
    file_system::path_info::PathInfo,
    test_support::TempDir,
};

/// A temp tree containing the working directory, so navigating to the parent
/// stays inside the fixture.
pub(super) struct Fixture {
    root: TempDir,
}

impl Fixture {
    pub(super) fn new() -> Self {
        let root = TempDir::new("claims");
        std::fs::create_dir_all(root.join("cwd")).unwrap();
        std::fs::write(root.join("cwd").join("file.txt"), b"x").unwrap();
        Self { root }
    }

    pub(super) fn bookmarks(&self) -> PathBuf {
        self.root.join("bookmarks")
    }

    pub(super) fn cwd(&self) -> PathBuf {
        self.root.join("cwd")
    }

    pub(super) fn directory(&self) -> PathInfo {
        PathInfo::try_from(self.cwd().as_path()).unwrap()
    }

    pub(super) fn file(&self) -> PathInfo {
        PathInfo::try_from(self.cwd().join("file.txt").as_path()).unwrap()
    }

    /// A path that does not exist, so file operations fail validation without
    /// starting a worker.
    fn missing(&self) -> PathInfo {
        let mut info = self.file();
        info.path = self.cwd().join("missing.txt");
        info.display_name = "missing.txt".to_string();
        info
    }
}

/// The real handler tree with blank openers (so nothing shells out) and
/// `config_dir` inside the fixture.
pub(super) fn test_handlers(tx: Sender<Command>, fixture: &Fixture) -> Handlers {
    let mut config = Config::builtin();
    config.config_dir = fixture.root.path().to_path_buf();
    config.openers = Openers {
        open_directory: String::new(),
        open_file: String::new(),
        open_filectrl_window: String::new(),
        run_in_terminal: String::new(),
    };
    let file_system = FileSystem::new(&config, tx);
    Config::init_test();
    Handlers {
        clipboard: Clipboard::disabled(),
        #[cfg(debug_assertions)]
        debug: debug::DebugHandler,
        file_system,
        root: RootView::new(Config::global()),
    }
}

/// One instance of every `Command` variant that a handler must claim.
///
/// Absent: terminal input (`Key`, `PasteText`, `Mouse`, `Resize`), and `Quit`
/// and `RunInForeground`, which must stay unclaimed for `App::run`.
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
        // Reads the host's MIME databases, read-only; spawns nothing.
        Command::OpenWithPrompt(fixture.file()),
        Command::OpenWith {
            argv: Vec::new(),
            label: "app".to_string(),
            path: fixture.file().path,
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
        Command::SearchResultsRefreshed {
            items: vec![fixture.file()],
            generation: 2,
        },
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
        Command::ResolveConflict(ConflictChoice::Skip),
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
        Command::StartSearch("query".to_string()),
        Command::FilterChanged("f".to_string()),
        Command::FilterEdited("f".to_string()),
        Command::SelectionChanged {
            selected: Some(fixture.file()),
            mark_count: 0,
            range: false,
        },
        Command::ResetView,
        Command::AlertError("e".to_string()),
        Command::AlertInfo("i".to_string()),
        Command::AlertWarn("w".to_string()),
        Command::CancelTask,
        Command::Progress(task),
        // Last, so the commands above run against the fixture's cwd.
        Command::GoToParentDirectory,
    ]
}

/// Exhaustive, so a new `Command` variant fails to compile until it is listed
/// in `claimable_commands` or exempted.
#[allow(dead_code)]
// The empty arms are kept separate: one lists exemptions, the other claims.
#[allow(clippy::match_same_arms)]
fn every_variant_is_accounted_for(command: &Command) {
    match command {
        Command::Key(_, _)
        | Command::PasteText(_)
        | Command::Mouse(_)
        | Command::Resize { .. }
        | Command::Quit
        | Command::RunInForeground { .. } => {}
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
        | Command::SearchResultsRefreshed { .. }
        | Command::Chmod { .. }
        | Command::Copy { .. }
        | Command::Move { .. }
        | Command::Paste(_)
        | Command::ResolveConflict(_)
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
        | Command::FilterEdited(_)
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
    handlers.file_system.run_once(Some(fixture.cwd())).unwrap();

    for command in claimable_commands(&fixture, &tx) {
        // OpenPrompt changes the mode partway through.
        let mode = handlers.root.mode();
        let mut derived = Vec::new();
        assert!(
            recursively_handle_command(&mut derived, &command, mode, &mut handlers),
            "no handler claims {command:?}, which `App::run` treats as fatal"
        );
    }
}

#[test]
fn running_in_the_foreground_is_left_to_the_app() {
    let fixture = Fixture::new();
    let (tx, _rx) = mpsc::channel();
    let mut handlers = test_handlers(tx, &fixture);

    let handled = recursively_handle_command(
        &mut Vec::new(),
        &Command::RunInForeground {
            program: crate::command::ForegroundProgram::Editor,
            path: fixture.file(),
        },
        InputMode::Normal,
        &mut handlers,
    );

    assert!(!handled);
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
        InputMode::Normal,
        &mut handlers,
    );

    assert!(!handled);
    assert!(derived.is_empty());
}
