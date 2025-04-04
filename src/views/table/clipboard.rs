use std::fmt::Display;

use anyhow::{anyhow, Error};
use arboard::Clipboard as Arboard;

use crate::{command::Command, file_system::path_info::PathInfo};

/// A clipboard that caches its content to avoid requiring mutable access for read operations.
///
/// The clipboard is a shared system resource that requires synchronization when reading.
/// By caching the content, we can avoid requiring mutable access for read operations
/// like `is_copied` and `is_cut`, while still maintaining correctness by updating
/// the cache when writing.
/// This ensures that is_copied and is_cut can be called without holding a mutable reference to the Clipboard.
pub(super) struct Clipboard {
    arboard: Arboard,
    cached_content: Option<(ClipboardCommand, String)>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            arboard: Arboard::new().expect("Can access the clipboard"),
            cached_content: None,
        }
    }
}

impl Clipboard {
    pub(super) fn is_copied(&self, path: &PathInfo) -> bool {
        self.is_command_type(path, ClipboardCommand::Copy)
    }

    pub(super) fn is_cut(&self, path: &PathInfo) -> bool {
        self.is_command_type(path, ClipboardCommand::Move)
    }

    fn is_command_type(&self, path: &PathInfo, expected_command: ClipboardCommand) -> bool {
        self.cached_content
            .as_ref()
            .filter(|(command, _)| *command == expected_command)
            .and_then(|(_, cached_path)| {
                PathInfo::try_from(cached_path.as_str())
                    .ok()
                    .map(|cached_path| cached_path == *path)
            })
            .unwrap_or(false)
    }

    pub(super) fn copy(&mut self, path: &str) {
        self.set_clipboard(ClipboardCommand::Copy, path);
    }

    pub(super) fn cut(&mut self, path: &str) {
        self.set_clipboard(ClipboardCommand::Move, path);
    }

    pub(super) fn maybe_command(&self, to: PathInfo) -> Option<Command> {
        self.cached_content.as_ref().and_then(|(command, from)| {
            PathInfo::try_from(from.as_str())
                .map(|from| command.as_command(from, to))
                .ok()
        })
    }

    fn set_clipboard(&mut self, command: ClipboardCommand, from: &str) {
        self.arboard
            .set_text(format!("{command} {from}"))
            .expect("Can write to the clipboard");
        self.cached_content = Some((command, from.to_string()));
    }
}

#[derive(PartialEq)]
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
    fn as_command(&self, from: PathInfo, to: PathInfo) -> Command {
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
