pub mod handler;
pub mod mode;
pub mod result;
pub mod sorting;

use crate::file_system::human::HumanPath;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Debug, Default)]
pub enum PromptKind {
    #[default]
    Filter,
    Rename,
}

#[derive(Clone, Debug)]
pub enum Command {
    AddError(String),
    ClosePrompt,
    DeletePath(HumanPath),
    Key(KeyCode, KeyModifiers),
    Open(HumanPath),
    OpenPrompt(PromptKind),
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
            Event::Mouse(_) => None,
            Event::Resize(w, h) => Some(Self::Resize(w, h)),
            _ => None,
        }
    }
}
