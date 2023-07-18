use super::focus::Focus;
use crate::file_system::path_display::PathDisplay;
use anyhow::Result;
use crossterm::event::read;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use std::option;
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
    ChangeDir(PathDisplay),
    OpenFile(PathDisplay),
    UpdateCurrentDir(PathDisplay, Vec<PathDisplay>),
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
                return Some(match (code, modifiers) {
                    (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        Command::Quit
                    }
                    (KeyCode::Tab, _) => Self::NextFocus,
                    (KeyCode::BackTab, _) => Self::PreviousFocus,
                    (KeyCode::Char(' '), KeyModifiers::CONTROL) => Self::ClearErrors,
                    (_, _) => Self::Key(code, modifiers),
                });
            }
            Event::Mouse(_) => None,
            Event::Resize(w, h) => Some(Self::Resize(w, h)),
            _ => None,
        }
    }
}
pub trait CommandHandler {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![]
    }

    fn handle_command(&mut self, _command: &Command) -> CommandResult {
        CommandResult::NotHandled
    }

    fn is_focussed(&self, _focus: &Focus) -> bool {
        false
    }
}

pub fn as_error_command(result: Result<Command>) -> Command {
    result.unwrap_or_else(|err| Command::Error(err.to_string()))
}

pub fn as_error_option_command(result: Result<Option<Command>>) -> Option<Command> {
    result.unwrap_or_else(|err| Some(Command::Error(err.to_string())))
}

#[derive(Clone, Debug)]
pub enum CommandResult {
    Handled(Option<Command>),
    NotHandled,
}

impl CommandResult {
    pub fn option(optional_command: Option<Command>) -> Self {
        if let Some(derived_command) = optional_command {
            CommandResult::some(derived_command)
        } else {
            CommandResult::none()
        }
    }

    pub fn none() -> Self {
        Self::Handled(None)
    }

    pub fn some(command: Command) -> Self {
        Self::Handled(Some(command))
    }
}

pub fn receive_commands(rx: &Receiver<Command>) -> Vec<Command> {
    let mut commands = Vec::new();
    loop {
        // Non-blocking
        let command = rx.try_recv();
        if command.is_err() {
            // Return when there are no more commands in the channel
            break;
        }
        let command = command.expect("Can receive a command from the rx channel");
        commands.push(command);
    }
    commands
}

pub fn spawn_command_sender(tx: Sender<Command>) {
    thread::spawn(move || loop {
        // Blocking read
        // Ref. https://docs.rs/crossterm/latest/crossterm/event/fn.read.html
        let event = read().expect("Can read events");
        if let Some(command) = Command::maybe_from(event) {
            tx.send(command).expect("Can send events");
        }
    });
}
