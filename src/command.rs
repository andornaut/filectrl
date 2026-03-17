pub mod handler;
pub mod mode;
pub mod result;
pub mod task;

use anyhow::{Error, anyhow};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind,
};

use self::{result::CommandResult, task::Task};
use crate::clipboard::ClipboardEntry;
use crate::file_system::path_info::PathInfo;

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
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
    Copy(PathInfo, PathInfo),
    DeletePath(PathInfo),
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Move(PathInfo, PathInfo),
    NavigateDirectory(PathInfo, Vec<PathInfo>),
    Open(PathInfo),
    OpenCustom(PathInfo),
    OpenPrompt(PromptKind),
    Progress(Task),
    Quit,
    Refresh,
    RefreshDirectory(PathInfo, Vec<PathInfo>),
    RenamePath(PathInfo, String),
    Resize(u16, u16), // w,h
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
                if mouse_event.kind == MouseEventKind::Moved
                    || matches!(mouse_event.kind, MouseEventKind::Up(_))
                {
                    None
                } else {
                    Some(Self::Mouse(mouse_event))
                }
            }
            Event::Resize(w, h) => Some(Self::Resize(w, h)),
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
                "Cannot convert CommandResult::Handled to Command, because it was not handled with a command"
            )),
            CommandResult::NotHandled => Err(anyhow!(
                "Cannot convert CommandResult::NotHandled to Command, because the CommandResult is not handled"
            )),
        }
    }
}
