pub mod handler;
pub mod mode;
pub mod result;
pub mod task;

use self::{result::CommandResult, task::Task};
use crate::file_system::human::HumanPath;
use anyhow::{anyhow, Error};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

#[derive(Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum PromptKind {
    #[default]
    Filter,
    Rename,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Command {
    AddError(String),
    ClipboardCopy(HumanPath),
    ClipboardCut(HumanPath),
    ClosePrompt,
    Copy(HumanPath, HumanPath),
    DeletePath(HumanPath),
    Key(KeyCode, KeyModifiers),
    Mouse(MouseEvent),
    Move(HumanPath, HumanPath),
    Open(HumanPath),
    OpenCustom(HumanPath),
    OpenPrompt(PromptKind),
    Progress(Task),
    Quit,
    RenamePath(HumanPath, String),
    Resize(u16, u16), // w,h
    SetDirectory(HumanPath, Vec<HumanPath>),
    SetFilter(String),
    SetSelected(Option<HumanPath>),
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
        Self::AddError(value.to_string())
    }
}

impl TryFrom<CommandResult> for Command {
    type Error = Error;

    fn try_from(value: CommandResult) -> Result<Self, Self::Error> {
        match value {
            CommandResult::Handled(option) => match option {
                Some(command) => Ok(command.clone()),
                None => Err(anyhow!(
                    "Cannot convert to Command, because CommandResult::Handled is None"
                )),
            },
            _ => Err(anyhow!(
                "Cannot convert to Command, because CommandResult is not Handled"
            )),
        }
    }
}
