pub mod handler;
pub mod result;
pub mod sorting;

use crate::file_system::human::HumanPath;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use self::sorting::SortColumn;

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Focus {
    Header,
    Prompt,
    #[default]
    Table,
}

#[derive(Clone, Debug, Default)]
pub enum PromptKind {
    Filter,
    #[default]
    Rename,
}

#[derive(Clone, Debug)]
pub enum Command {
    AddError(String),
    ClearErrors,
    Key(KeyCode, KeyModifiers),
    NextFocus,
    PreviousFocus,
    Quit,
    Resize(u16, u16), // w,h
    SetFocus(Focus),
    ToggleHelp,

    // Content & Prompt commands
    BackDir,
    ChangeDir(HumanPath),
    ClosePrompt,
    DeletePath(HumanPath),
    OpenFile(HumanPath),
    OpenPrompt(PromptKind),
    RefreshDir,
    RenamePath(HumanPath, String),
    SetDirectory(HumanPath, Vec<HumanPath>),
    SetFilter(String),
    SetSelected(Option<HumanPath>),
}

impl Command {
    pub fn needs_focus(&self) -> bool {
        match self {
            Self::Key(_, _) => true,
            _ => false,
        }
    }

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

    pub fn translate_non_prompt_key_command(self) -> Command {
        match self {
            Command::Key(code, modifiers) => match (code, modifiers) {
                (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::F(5), _) => {
                    Self::RefreshDir
                }
                (_, _) => match code {
                    KeyCode::Char('q') => Command::Quit,
                    KeyCode::Tab => Self::NextFocus,
                    KeyCode::BackTab => Self::PreviousFocus,
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
