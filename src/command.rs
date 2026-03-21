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

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum PromptAction {
    Delete(usize),
    Filter(String),
    Rename(PathInfo, String),
}

impl Default for PromptAction {
    fn default() -> Self {
        Self::Delete(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    // Terminal input events
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Resize { width: u16, height: u16 },

    // Navigation intents — resolved by FileSystem
    Back,
    Open(PathInfo),
    OpenCurrentDirectory,
    OpenNewWindow,
    Refresh,

    // Navigation results — emitted by FileSystem
    NavigateDirectory(PathInfo, Vec<PathInfo>),
    RefreshDirectory(PathInfo, Vec<PathInfo>),

    // File operations
    Copy { srcs: Vec<PathInfo>, dest: PathInfo },
    Move { srcs: Vec<PathInfo>, dest: PathInfo },
    Delete(Vec<PathInfo>),
    RenamePath(PathInfo, String),

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

    // View state
    SetFilter(String),
    SetMarkCount(usize),
    SetSelected(Option<PathInfo>),
    Reset, // If HelpView is open, then closes it, otherwise clears clipboard, filter, and marks
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
