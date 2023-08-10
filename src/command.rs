pub mod handler;
pub mod result;
pub mod sorting;

use crate::{app::focus::Focus, file_system::human::HumanPath};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use self::sorting::SortColumn;

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
    Focus(Focus),
    Key(KeyCode, KeyModifiers),
    NextFocus,
    PreviousFocus,
    Quit,
    Resize(u16, u16), // w,h
    ToggleHelp,

    // Content & Prompt commands
    CancelPrompt,
    OpenPrompt(PromptKind),
    SetSelected(Option<HumanPath>),
    SubmitPrompt(String),
    Sort(SortColumn),

    // FileSystem commands
    BackDir,
    ChangeDir(HumanPath),
    DeletePath(HumanPath),
    OpenFile(HumanPath),
    RefreshDir,
    RenamePath(HumanPath, String),
    SetDirectory(HumanPath, Vec<HumanPath>),
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
                (KeyCode::Esc, _)
                | (KeyCode::Char('c'), KeyModifiers::CONTROL)
                | (KeyCode::Char('q'), _) => Command::Quit,
                (KeyCode::Tab, _) => Self::NextFocus,
                (KeyCode::BackTab, _) => Self::PreviousFocus,
                (KeyCode::Backspace, _)
                | (KeyCode::Left, _)
                | (KeyCode::Char('b'), _)
                | (KeyCode::Char('h'), _) => Command::BackDir,
                (KeyCode::Char('c'), _) => Self::ClearErrors,
                (KeyCode::Char('?'), _) => Self::ToggleHelp,
                (KeyCode::Char('r'), KeyModifiers::CONTROL) | (KeyCode::F(5), _) => {
                    Self::RefreshDir
                }
                (_, _) => self,
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
