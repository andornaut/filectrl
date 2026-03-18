pub mod handler;
pub mod mode;
pub mod progress;
pub mod result;

use anyhow::{Error, anyhow};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};

use self::{result::CommandResult, progress::Task};
use crate::app::clipboard::ClipboardEntry;
use crate::file_system::path_info::PathInfo;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum PromptKind {
    #[default]
    Filter,
    Rename,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    AlertError(String),
    AlertInfo(String),
    AlertWarn(String),
    ClearClipboard,
    ClosePrompt,
    Copy { srcs: Vec<PathInfo>, dest: PathInfo },
    Delete(Vec<PathInfo>),
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Move { srcs: Vec<PathInfo>, dest: PathInfo },
    NavigateDirectory(PathInfo, Vec<PathInfo>),
    Open(PathInfo),
    OpenCustom(PathInfo),
    OpenPrompt(PromptKind, String),
    /// Intent to paste the clipboard entry into the given directory.
    /// Emitted by TableView; resolved by App into Copy or Move using AppState::clipboard_entry.
    Paste(PathInfo),
    Progress(Task),
    Quit,
    Refresh,
    RefreshDirectory(PathInfo, Vec<PathInfo>),
    RenamePath(PathInfo, String),
    /// Intent to rename the currently selected item to the given name.
    /// Emitted by PromptView on submit; resolved by App into RenamePath using AppState::selected.
    RenameSelected(String),
    Resize { width: u16, height: u16 },
    SetClipboard(ClipboardEntry),
    SetFilter(String),
    SetSelected(Option<PathInfo>),
    ToggleHelp,
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
            Event::Resize(w, h) => Some(Self::Resize { width: w, height: h }),
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
            CommandResult::Handled => Err(anyhow!(
                "expected HandledWith, got Handled"
            )),
            CommandResult::NotHandled => Err(anyhow!(
                "expected HandledWith, got NotHandled"
            )),
        }
    }
}
