pub mod handler;
pub mod progress;
pub mod result;

use anyhow::{Error, anyhow};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};

use self::{progress::Task, result::CommandResult};
use crate::app::clipboard::ClipboardEntry;
use crate::file_system::path_info::PathInfo;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum InputMode {
    Prompt,
    #[default]
    Normal,
}

/// What an open prompt is collecting input for.
///
/// Each variant is opened via `Command::OpenPrompt` and, on submit, resolves
/// into a corresponding `Command` (see `PromptView::submit`). Some map to a
/// same-named command (`Rename` -> `Command::Rename`); others to a different
/// one (`Delete` -> `Command::ConfirmDelete`, `Filter` -> `Command::FilterChanged`,
/// `Goto` -> `Command::Open`, `Search` -> `Command::StartSearch`).
///
/// The payloads differ by lifecycle stage: a `PromptAction` carries the prompt's
/// *initial* state (e.g. `Rename.name` is the pre-filled text, `Delete(usize)` is
/// a count for the confirmation message), whereas the resolved `Command` carries
/// the *submitted* result (e.g. `Rename.name` is the entered text,
/// `Delete(Vec<PathInfo>)` is the resolved paths). They are intentionally not
/// merged for this reason.
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
        directory: String,
    },
    Rename {
        path: PathInfo,
        name: String,
    },
    Search(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    // Terminal input events
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Resize {
        width: u16,
        height: u16,
    },

    // Navigation — handled by FileSystem
    GoToParentDirectory,
    GoToPreviousDirectory,
    Open(PathInfo),
    Refresh,

    // External commands — handled by FileSystem (shell out via open_in)
    OpenCurrentDirectory,
    OpenNewWindow,

    // Navigation results — emitted by FileSystem
    NavigatedDirectory {
        directory: PathInfo,
        children: Vec<PathInfo>,
    },
    RefreshedDirectory {
        directory: PathInfo,
        children: Vec<PathInfo>,
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
    AddBookmark {
        directory: PathInfo,
        name: String,
    },
    GetBookmarks, // Intent: resolved by FileSystem into Bookmarks
    Bookmarks {
        bookmarks: Vec<PathInfo>,
    },
    CreateDirectory(String),
    Delete(Vec<PathInfo>),
    Move {
        srcs: Vec<PathInfo>,
        dest: PathInfo,
    },
    Rename {
        path: PathInfo,
        name: String,
    },

    // File operation intents
    CancelTask,
    ConfirmDelete,   // Intent: resolved by TableView into Delete
    Paste(PathInfo), // Intent: resolved by App into Copy or Move

    // Prompt
    CancelPrompt,
    OpenPrompt(PromptAction),

    // Clipboard
    ClearClipboard,
    SetClipboardEntry(ClipboardEntry),
    GetClipboardText,         // Intent: resolved by App into ClipboardText
    ClipboardText(String),    // Result of GetClipboardText; handled by PromptView
    SetClipboardText(String), // Handled by App; writes text to the system clipboard

    // Search
    CancelSearch, // Non-destructive: stop the search thread but keep results and notice
    ExitSearch,
    SearchResult(PathInfo),
    SearchTick,
    StartSearch(String),

    // View state notifications — emitted by TableView
    FilterChanged(String),
    MarkCountChanged(usize),
    SelectionChanged(Option<PathInfo>),
    ResetView, // Returns to Normal mode; clears clipboard, filter, marks, and help
    ResetHelpScroll,

    // Alerts
    AlertError(String),
    AlertInfo(String),
    AlertWarn(String),

    Progress(Task),
    Quit,
}

impl Command {
    pub fn maybe_from(event: Event) -> Option<Self> {
        match event {
            Event::Key(key) => {
                let KeyEvent {
                    code, modifiers, ..
                } = key;
                Some(Self::Key(code, modifiers))
            }
            Event::Mouse(mouse_event) => {
                // Suppress Move events — they are too noisy and no handler uses them.
                // Up events are kept: the scrollbar needs them to clear its drag state.
                if mouse_event.kind == MouseEventKind::Moved {
                    None
                } else {
                    Some(Self::Mouse(mouse_event))
                }
            }
            Event::Resize(w, h) => Some(Self::Resize {
                width: w,
                height: h,
            }),
            _ => None,
        }
    }
}

impl From<Error> for Command {
    fn from(value: Error) -> Self {
        Self::AlertError(value.to_string())
    }
}

impl TryFrom<CommandResult> for Command {
    type Error = Error;

    fn try_from(value: CommandResult) -> Result<Self, Self::Error> {
        match value {
            CommandResult::HandledWith(command) => Ok(*command),
            CommandResult::Handled => Err(anyhow!("expected HandledWith, got Handled")),
            CommandResult::NotHandled => Err(anyhow!("expected HandledWith, got NotHandled")),
        }
    }
}
