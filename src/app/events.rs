use std::{
    sync::mpsc::{Receiver, Sender},
    thread,
};

use ratatui::crossterm::event::read;

use crate::command::Command;

pub(super) fn receive_commands(rx: &Receiver<Command>) -> Vec<Command> {
    // Block (zero CPU) until the first command arrives
    let Ok(first) = rx.recv() else {
        return Vec::new();
    };
    let mut commands = vec![first];
    // Drain any additional commands already queued
    while let Ok(command) = rx.try_recv() {
        commands.push(command);
    }
    commands
}

pub(super) fn spawn_command_sender(tx: Sender<Command>) {
    thread::spawn(move || loop {
        // Blocking read
        // Ref. https://docs.rs/crossterm/latest/crossterm/event/fn.read.html
        let event = read().expect("Can read events");
        if let Some(command) = Command::maybe_from(event) {
            tx.send(command).expect("Can send events");
        }
    });
}
