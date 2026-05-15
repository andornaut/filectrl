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

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum PromptAction {
    Chmod { paths: Vec<PathInfo>, mode: String },
    #[default]
    CreateDirectory,
    Delete(usize),
    Filter(String),
    Rename { path: PathInfo, name: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    // Terminal input events
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Resize { width: u16, height: u16 },

    // Navigation — handled by FileSystem
    Back,
    Open(PathInfo),
    Refresh,

    // External commands — handled by FileSystem (shell out via open_in)
    OpenCurrentDirectory,
    OpenNewWindow,

    // Navigation results — emitted by FileSystem
    NavigatedDirectory { directory: PathInfo, children: Vec<PathInfo> },
    RefreshedDirectory { directory: PathInfo, children: Vec<PathInfo> },

    // File operations
    Chmod { paths: Vec<PathInfo>, mode: String },
    Copy { srcs: Vec<PathInfo>, dest: PathInfo },
    CreateDirectory(String),
    Delete(Vec<PathInfo>),
    Move { srcs: Vec<PathInfo>, dest: PathInfo },
    Rename { path: PathInfo, name: String },

    // File operation intents
    ConfirmDelete,   // Intent: resolved by TableView into Delete
    Paste(PathInfo), // Intent: resolved by App into Copy or Move

    // Prompt
    OpenPrompt(PromptAction),
    CancelPrompt,

    // Clipboard
    ClearClipboard,
    SetClipboard(ClipboardEntry),
    ReadFromClipboard,         // Intent: resolved by App into TextFromClipboard
    TextFromClipboard(String), // Result of ReadFromClipboard; handled by PromptView
    WriteToClipboard(String),  // Handled by App; writes text to system clipboard

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
