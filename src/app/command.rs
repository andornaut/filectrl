use crate::file_system::Path;
use crossterm::event::read;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use std::{
    sync::mpsc::{Receiver, Sender},
    thread,
};

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
    Quit,
    Resize(u16, u16), // w,h

    // FileSystem commands
    _ChangeDir(Path),
    UpdateCurrentDir(Path, Vec<Path>),
}

impl Command {
    pub fn maybe_from(event: Event) -> Option<Self> {
        match event {
            Event::Key(key) => {
                let KeyEvent {
                    code, modifiers, ..
                } = key;
                if let (KeyCode::Char('c'), KeyModifiers::CONTROL) = (code, modifiers) {
                    return Some(Self::Quit);
                }
                match code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => Some(Self::Quit),
                    _ => None,
                }
            }
            Event::Mouse(_) => None,
            Event::Resize(w, h) => Some(Self::Resize(w, h)),
            _ => None,
        }
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
        // Non-blocking read
        // Ref. https://docs.rs/crossterm/latest/crossterm/event/fn.read.html
        let event = read().expect("Can read events");
        if let Some(command) = Command::maybe_from(event) {
            tx.send(command).unwrap();
        }
    });
}
