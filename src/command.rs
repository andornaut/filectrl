pub mod handler;
pub mod result;

use crate::{app::focus::Focus, file_system::path::HumanPath};
use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

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

    // Content/Prompt commands
    CancelPrompt,
    SubmitPrompt(String),

    // FileSystem commands
    ChangeDir(HumanPath),
    DeletePath(HumanPath),
    Dir(HumanPath, Vec<HumanPath>),
    OpenFile(HumanPath),
    RefreshDir,
    RenamePath(HumanPath, String),
    UpDir,
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
                | (KeyCode::Char('q'), _)
                | (KeyCode::Char('Q'), _) => Command::Quit,
                (KeyCode::Tab, _) => Self::NextFocus,
                (KeyCode::BackTab, _) => Self::PreviousFocus,
                (KeyCode::Backspace, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                    Command::UpDir
                }
                (KeyCode::Char('c'), _) => Self::ClearErrors,
                (KeyCode::Char('r'), _) | (KeyCode::F(5), _) => Self::RefreshDir,
                (_, _) => self,
            },
            _ => self,
        }
    }
}

pub fn unwrap_or_error_command(result: Result<Command>) -> Command {
    result.unwrap_or_else(|err| Command::AddError(err.to_string()))
}

pub fn unwrap_option_or_error_command(result: Result<Option<Command>>) -> Option<Command> {
    result.unwrap_or_else(|err| Some(Command::AddError(err.to_string())))
}
