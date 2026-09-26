pub mod handler;
pub mod progress;
pub mod result;

use anyhow::Error;
#[cfg(test)]
use anyhow::anyhow;
use std::ffi::OsString;
use std::path::PathBuf;

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};

use self::progress::Task;
#[cfg(test)]
use self::result::CommandResult;
use crate::app::clipboard::ClipboardEntry;
use crate::file_system::path_info::PathInfo;

/// A program that takes the terminal over to show an entry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ForegroundProgram {
    Editor,
    Pager,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum InputMode {
    Prompt,
    #[default]
    Normal,
}

/// What an open prompt is collecting input for. Carries the prompt's initial
/// state; `PromptView::submit` resolves it into a `Command`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum PromptAction {
    Chmod {
        paths: Vec<PathInfo>,
        mode: String,
    },
    AddBookmark {
        directory: PathInfo,
        name: String,
    },
    #[default]
    CreateDirectory,
    Delete(usize),
    Filter(String),
    Goto {
        directory: PathBuf,
    },
    Rename {
        path: PathInfo,
        name: String,
    },
    Search(String),
    /// Confirm a paste of a clipboard entry this window did not write.
    ConfirmPaste {
        entry: ClipboardEntry,
        dest: PathInfo,
    },
    /// Quit while this many file operations are running or queued.
    ConfirmQuit(usize),
    /// A paste found `name` already present. `can_overwrite` is false when
    /// either side is a directory.
    Conflict {
        name: String,
        can_overwrite: bool,
    },
}

impl PromptAction {
    /// True for the prompts that take a single keypress rather than text.
    pub fn is_confirmation(&self) -> bool {
        matches!(
            self,
            PromptAction::Delete(_)
                | PromptAction::Conflict { .. }
                | PromptAction::ConfirmPaste { .. }
                | PromptAction::ConfirmQuit(_)
        )
    }
}

/// How a paste resolves a destination that already exists. The `*All` variants
/// also answer for the rest of the batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ConflictChoice {
    Overwrite,
    OverwriteAll,
    Skip,
    SkipAll,
}

/// The app's single message type, broadcast to all `CommandHandler`s. An
/// "Intent" is resolved by another component into a follow-up command; a
/// "Result" answers an intent.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    // Terminal input events
    Key(KeyCode, KeyModifiers),
    /// A bracketed paste: a line break in it is text, not Enter.
    PasteText(String),
    Mouse(MouseEvent),
    Resize {
        width: u16,
        height: u16,
    },

    // External commands, handled by FileSystem (shell out via open_in)
    OpenCurrentDirectory,
    OpenNewWindow,
    // FileSystem spawns `argv` detached; `label` and `path` name it in a
    // failure. An empty `argv` is a no-op.
    OpenWith {
        argv: Vec<OsString>,
        label: String,
        path: PathBuf,
        working_dir: Option<PathBuf>,
    },

    // Navigation
    GoToParentDirectory, // Intent: resolved by FileSystem into NavigatedDirectory
    GoToPreviousDirectory, // Intent: resolved by FileSystem into NavigatedDirectory
    Open(PathInfo),      // Intent: FileSystem -> NavigatedDirectory (dir) or external open (file)
    OpenWithPrompt(PathInfo), // Intent: RootView shows the "open with" picker for this path
    NavigatedDirectory {
        // Result: entries stream in afterward as ListingBatch.
        directory: PathInfo,
        generation: u64,
    },
    RefreshDirectory, // Intent: resolved by FileSystem into RefreshedDirectory
    RefreshedDirectory {
        // Result: entries stream in as ListingBatch.
        directory: PathInfo,
        generation: u64,
    },
    // Result: streamed entries of a listing or search. A batch whose
    // `generation` is superseded is ignored.
    ListingBatch {
        items: Vec<PathInfo>,
        generation: u64,
    },
    DirectoryListingComplete {
        generation: u64,
    },
    // Result: ended search results reread by path on a refresh.
    SearchResultsRefreshed {
        items: Vec<PathInfo>,
        generation: u64,
    },

    // File operations
    Chmod {
        paths: Vec<PathInfo>,
        mode: String,
    },
    Copy {
        srcs: Vec<PathInfo>,
        dest: PathInfo,
    },
    Move {
        srcs: Vec<PathInfo>,
        dest: PathInfo,
    },
    Paste(PathInfo), // Intent: resolved by App into Copy or Move
    // Intent: answers FileSystem's conflict prompt.
    ResolveConflict(ConflictChoice),
    CreateDirectory(String),
    ConfirmDelete, // Intent: resolved by TableView into Delete
    Delete(Vec<PathInfo>),
    Rename {
        path: PathInfo,
        name: String,
    },

    // Bookmarks
    AddBookmark {
        directory: PathInfo,
        name: String,
    },
    GetBookmarks, // Intent: resolved by FileSystem into Bookmarks
    Bookmarks {
        bookmarks: Vec<PathInfo>,
    },

    // Prompt
    CancelPrompt, // Closes the prompt without submitting; returns to Normal mode
    OpenPrompt(PromptAction),

    // Clipboard
    SetClipboardEntry(Option<ClipboardEntry>), // None clears the clipboard
    GetClipboardText,                          // Intent: resolved by App into ClipboardText
    ClipboardText(String),                     // Result: of GetClipboardText; handled by PromptView
    SetClipboardText(String), // Handled by App; writes text to the system clipboard

    // Search
    // Messages from a superseded `generation` are ignored.
    CancelSearch, // Intent: stop the search thread non-destructively (keep results and notice)
    ExitedSearch {
        generation: u64,
    }, // Result: search thread has exited (completed or after CancelSearch)
    SearchStarted {
        generation: u64,
    }, // Result: FileSystem spawned the search thread
    SearchTick,
    StartSearch(String), // Intent: spawns the search thread; streams ListingBatch

    // View state notifications, emitted by TableView
    FilterChanged(String),
    // The filter prompt's text after each edit; the prompt stays open.
    FilterEdited(String),
    SelectionChanged {
        selected: Option<PathInfo>,
        mark_count: usize,
        range: bool,
    },
    ResetView, // Returns to Normal mode; clears clipboard, filter, marks, and help

    // Alerts
    AlertError(String),
    AlertInfo(String),
    AlertWarn(String),

    // Tasks
    CancelTask,     // Intent: cancel the running task
    Progress(Task), // Result: progress update for the running task

    // Global
    Quit,
    // Handled by `App`, which holds the terminal it suspends.
    RunInForeground {
        program: ForegroundProgram,
        path: PathInfo,
    },
}

impl Command {
    pub fn maybe_from(event: &Event) -> Option<Self> {
        match event {
            Event::Key(key) => {
                let KeyEvent {
                    code, modifiers, ..
                } = key;
                Some(Self::Key(*code, *modifiers))
            }
            Event::Mouse(mouse_event) => {
                // No handler uses Moved events.
                if mouse_event.kind == MouseEventKind::Moved {
                    None
                } else {
                    Some(Self::Mouse(*mouse_event))
                }
            }
            Event::Paste(text) => Some(Self::PasteText(text.clone())),
            Event::Resize(w, h) => Some(Self::Resize {
                width: *w,
                height: *h,
            }),
            _ => None,
        }
    }
}

impl From<Error> for Command {
    fn from(value: Error) -> Self {
        // `{:#}` keeps the cause chain on the alert's one line.
        Self::AlertError(format!("{value:#}"))
    }
}

/// Test-only downcast to exactly one derived command.
#[cfg(test)]
impl TryFrom<CommandResult> for Command {
    type Error = Error;

    fn try_from(value: CommandResult) -> Result<Self, Self::Error> {
        match value {
            CommandResult::HandledWith(command) => Ok(*command),
            CommandResult::Handled => Err(anyhow!("expected HandledWith, got Handled")),
            CommandResult::HandledWithMany(_) => {
                Err(anyhow!("expected HandledWith, got HandledWithMany"))
            }
            CommandResult::NotHandled => Err(anyhow!("expected HandledWith, got NotHandled")),
        }
    }
}
