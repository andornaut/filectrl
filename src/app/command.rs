use crossterm::event::read;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use std::{
    sync::mpsc::{Receiver, Sender},
    thread,
};

use crate::file_system::Path;

pub trait CommandHandler {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        vec![]
    }

    fn handle_command(&mut self, _command: &Command) -> Option<Command> {
        None
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    // App commands
    Continue,
    Quit,
    Resize(u16, u16), // w,h

    // FileSystem commands
    _ChangeDir(Path),
    UpdateCurrentDir(Path, Vec<Path>),
}

impl Command {
    pub fn is_actionable(&self) -> bool {
        if let Command::Continue = self {
            return false;
        }
        true
    }
}

impl From<Event> for Command {
    fn from(event: Event) -> Self {
        match event {
            Event::Key(key) => {
                let KeyEvent {
                    code, modifiers, ..
                } = key;
                if let (KeyCode::Char('c'), KeyModifiers::CONTROL) = (code, modifiers) {
                    return Self::Quit;
                }
                match code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => Self::Quit,
                    _ => Self::Continue,
                }
            }
            Event::Mouse(_) => Self::Continue,
            Event::Resize(w, h) => Self::Resize(w, h),
            _ => Self::Continue,
        }
    }
}

pub fn receive_commands(rx: &Receiver<Command>) -> Vec<Command> {
    let mut commands = Vec::new();
    loop {
        let command = rx.try_recv();
        if command.is_err() {
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
        let event = read().expect("Can read events");
        let command = Command::from(event);
        if let Command::Continue = command {
            continue;
        }
        tx.send(command).unwrap();
    });
}
