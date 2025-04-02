use crate::{command::Command, file_system::human::HumanPath};
use anyhow::{anyhow, Error};
use arboard::Clipboard as Arboard;
use std::fmt::Display;

pub(super) struct Clipboard(Arboard);

impl Default for Clipboard {
    fn default() -> Self {
        Self(Arboard::new().expect("Can access the clipboard"))
    }
}

impl Clipboard {
    pub(super) fn copy(&mut self, path: &str) {
        self.set_clipboard(ClipboardCommand::Copy, path);
    }

    pub(super) fn cut(&mut self, path: &str) {
        self.set_clipboard(ClipboardCommand::Move, path);
    }

    pub(super) fn maybe_command(&mut self, to: HumanPath) -> Option<Command> {
        self.0
            .get_text()
            .map(|message| {
                split_clipboard_message(&message).and_then(|(command, from)| {
                    HumanPath::try_from(from)
                        .map(|from| {
                            ClipboardCommand::try_from(command)
                                .map(|command| command.as_command(from, to))
                                .ok()
                        })
                        .ok()
                        .flatten()
                })
            })
            .ok()
            .flatten()
    }

    fn set_clipboard(&mut self, command: ClipboardCommand, from: &str) {
        self.0
            .set_text(format!("{command} {from}"))
            .expect("Can write to the clipboard");
    }
}

enum ClipboardCommand {
    Copy,
    Move,
}

impl Display for ClipboardCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Copy => "cp",
            Self::Move => "mv",
        };
        write!(f, "{}", name)
    }
}

impl ClipboardCommand {
    fn as_command(&self, from: HumanPath, to: HumanPath) -> Command {
        match self {
            Self::Copy => Command::Copy(from, to),
            Self::Move => Command::Move(from, to),
        }
    }
}

impl TryFrom<&str> for ClipboardCommand {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "cp" => Ok(Self::Copy),
            "mv" => Ok(Self::Move),
            _ => Err(anyhow!("Invalid ClipboardCommand")),
        }
    }
}

fn split_clipboard_message(message: &str) -> Option<(ClipboardCommand, &str)> {
    match message.split_once(' ') {
        Some((command, path)) => match ClipboardCommand::try_from(command) {
            Err(_) => None,
            Ok(command) => Some((command, path)),
        },
        None => Some((ClipboardCommand::Copy, message)),
    }
}
