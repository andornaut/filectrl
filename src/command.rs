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

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum InputMode {
    Prompt,
    #[default]
    Normal,
}

/// What an open prompt is collecting input for.
///
/// Each variant is opened via `Command::OpenPrompt` and resolves on submit into
/// a `Command` (see `PromptView::submit`), sometimes the same-named one
/// (`Rename`), sometimes not (`Delete` -> `ConfirmDelete`, `Filter` ->
/// `FilterChanged`, `Goto` -> `Open`, `Search` -> `StartSearch`).
///
/// The payloads differ by lifecycle stage, which is why the two are not merged:
/// a `PromptAction` carries the prompt's *initial* state (`Rename.name` is the
/// pre-filled text, `Delete(usize)` a count for the message), and the resolved
/// `Command` the *submitted* result (`Rename.name` is what was typed,
/// `Delete(Vec<PathInfo>)` the resolved paths).
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
    /// A paste found `name` already present in the destination directory.
    /// `can_overwrite` is false when the existing entry is a directory, which
    /// is never replaced, so the prompt offers only the skip choices.
    Conflict {
        name: String,
        can_overwrite: bool,
    },
}

impl PromptAction {
    /// True for the prompts that take a single keypress rather than text, and
    /// so render as a full-width label with no input area.
    pub fn is_confirmation(&self) -> bool {
        matches!(
            self,
            PromptAction::Delete(_) | PromptAction::Conflict { .. }
        )
    }
}

/// How a paste resolves a destination that already exists. The `*All` variants
/// answer for the rest of the batch as well as for the collision in front of
/// the user, so a paste of many sources need not be answered many times.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ConflictChoice {
    Overwrite,
    OverwriteAll,
    Skip,
    SkipAll,
}

/// The single message type for the whole app: terminal input, navigation,
/// file operations, view-state notifications, and alerts. Commands are
/// broadcast to all `CommandHandler`s (see `app::recursively_handle_command`).
///
/// Lifecycle conventions used in the annotations below:
/// - **Intent**: a request that another component resolves into a follow-up
///   command (e.g. `Paste` -> `Copy`/`Move`). Annotated `// Intent: …`.
/// - **Result**: emitted in response to an intent, carrying data the
///   originator could not produce itself (e.g. `Bookmarks`,
///   `NavigatedDirectory`). Annotated `// Result: …`.
/// - Everything else is a terminal event, a direct action, or a view-state
///   notification.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    // Terminal input events
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Resize {
        width: u16,
        height: u16,
    },

    // External commands, handled by FileSystem (shell out via open_in)
    OpenCurrentDirectory,
    OpenNewWindow,
    // Intent: the "open with" picker resolved a chosen application into a
    // concrete argv; FileSystem spawns it detached. `label` names the
    // application in the failure alert. An empty `argv` is a no-op.
    OpenWith {
        argv: Vec<OsString>,
        label: String,
        working_dir: Option<PathBuf>,
    },

    // Navigation
    GoToParentDirectory, // Intent: resolved by FileSystem into NavigatedDirectory
    GoToPreviousDirectory, // Intent: resolved by FileSystem into NavigatedDirectory
    Open(PathInfo),      // Intent: FileSystem -> NavigatedDirectory (dir) or external open (file)
    OpenWithPrompt(PathInfo), // Intent: RootView shows the "open with" picker for this path
    NavigatedDirectory {
        // Result: of GoToParentDirectory / GoToPreviousDirectory / Open (emitted by FileSystem).
        // The entries are not included; they stream in afterward as ListingBatch.
        directory: PathInfo,
        generation: u64,
    },
    RefreshDirectory, // Intent: resolved by FileSystem into RefreshedDirectory
    RefreshedDirectory {
        // Result: of RefreshDirectory. Entries stream in as ListingBatch.
        directory: PathInfo,
        generation: u64,
    },
    // Result: a batch of streamed entries (directory load or search hits),
    // appended in read order by TableView. `generation` matches the command that
    // started the stream (Navigated/RefreshedDirectory or SearchStarted), so a
    // superseded stream's batches are ignored. Both draw from one counter, so a
    // generation is never ambiguous.
    ListingBatch {
        items: Vec<PathInfo>,
        generation: u64,
    },
    DirectoryListingComplete {
        // Result: the streamed listing finished; TableView sorts and restores selection.
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
    // Intent: answers the conflict prompt FileSystem opened for the source at
    // the front of the paste it is holding; resolved by FileSystem into the
    // next task, the next prompt, or the clipboard follow-up.
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
        // Result: of GetBookmarks
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
    // `generation` is a monotonic id stamped by FileSystem when a search
    // starts. Consumers ignore results/exits from superseded generations, so
    // a cancelled search's final messages cannot disturb its replacement.
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
    SelectionChanged {
        // Snapshot of the table's cursor and mark count, taken whenever either
        // may have changed. StatusView reads `selected`; NoticesView reads
        // `mark_count`.
        selected: Option<PathInfo>,
        mark_count: usize,
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
                // Suppress Move events: they are too noisy and no handler uses them.
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
        // `{:#}` flattens the cause chain onto one line, joined by ": ". An
        // alert is a single line, so `to_string` would show only the outermost
        // message and drop the cause that names what actually went wrong.
        Self::AlertError(format!("{value:#}"))
    }
}

/// Test-only downcast to exactly one derived command. Production code must
/// consume every derived command via `CommandResult::into_commands`, which
/// cannot silently drop siblings.
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
