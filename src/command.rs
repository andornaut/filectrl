pub mod handler;
pub mod result;
pub mod sorting;

use crate::file_system::human::HumanPath;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use self::sorting::SortColumn;

#[derive(Clone, Debug, Default)]
pub enum PromptKind {
    #[default]
    Filter,
    Rename,
}

#[derive(Clone, Debug)]
pub enum Command {
    AddError(String),
    BackDir,
    ClearErrors,
    ClosePrompt,
    DeletePath(HumanPath),
    Key(KeyCode, KeyModifiers),
    Open(HumanPath),
    OpenPrompt(PromptKind),
    Quit,
    RefreshDir,
    RenamePath(HumanPath, String),
    Resize(u16, u16), // w,h
    SetDirectory(HumanPath, Vec<HumanPath>),
    SetFilter(String),
    SetSelected(Option<HumanPath>),
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
            Event::Mouse(_) => None,
            Event::Resize(w, h) => Some(Self::Resize(w, h)),
            _ => None,
        }
    }

    pub fn needs_focus(&self) -> bool {
        match self {
            Self::Key(_, _) => true,
            _ => false,
        }
    }

    pub fn translate_non_prompt_key_command(self) -> Command {
        match self {
            Command::Key(code, modifiers) => match (code, modifiers) {
                (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::F(5), _) => {
                    Self::RefreshDir
                }
                (_, _) => match code {
                    KeyCode::Char('q') => Command::Quit,
                    KeyCode::Backspace
                    | KeyCode::Left
                    | KeyCode::Char('b')
                    | KeyCode::Char('h') => Command::BackDir,
                    KeyCode::Char('e') => Self::ClearErrors,
                    KeyCode::Char('?') => Self::ToggleHelp,
                    _ => self,
                },
            },
            _ => self,
        }
    }
}

impl PartialEq<&str> for SortColumn {
    fn eq(&self, other: &&str) -> bool {
        let other = other.to_lowercase();
        match self {
            Self::Modified => "modified" == other,
            Self::Name => "name" == other,
            Self::Size => "size" == other,
        }
    }
}
