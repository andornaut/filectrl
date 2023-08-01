pub mod handler;
pub mod result;

use crate::file_system::path::HumanPath;
use anyhow::Result;
use crossterm::event::{read, Event, KeyCode, KeyEvent, KeyModifiers};
use std::{
    sync::mpsc::{Receiver, Sender},
    thread,
};

#[derive(Clone, Debug)]
pub enum Command {
    // App commands
    ClearErrors,
    Error(String),
    Quit,
    NextFocus,
    PreviousFocus,
    Resize(u16, u16), // w,h

    // Generic key event. Will be specialized during `app.broadcast()`
    Key(KeyCode, KeyModifiers),

    // FileSystem commands
    BackDir,
    ChangeDir(HumanPath),
    OpenFile(HumanPath),
    UpdateCurrentDir(HumanPath, Vec<HumanPath>),
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
                    Command::BackDir
                }
                (KeyCode::Char('c'), _) => Self::ClearErrors,
                (_, _) => self,
            },
            _ => self,
        }
    }
}

pub fn as_error_command(result: Result<Command>) -> Command {
    result.unwrap_or_else(|err| Command::Error(err.to_string()))
}

pub fn as_error_option_command(result: Result<Option<Command>>) -> Option<Command> {
    result.unwrap_or_else(|err| Some(Command::Error(err.to_string())))
}