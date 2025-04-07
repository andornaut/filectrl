use std::fmt::{Display, Formatter};

use anyhow::{anyhow, Error};
use clipboard::{ClipboardContext, ClipboardProvider};
use log::warn;

use crate::{command::Command, file_system::path_info::PathInfo};

/// A clipboard that caches its content to avoid requiring mutable access for read operations.
///
/// The clipboard is a shared system resource that requires synchronization when reading.
/// By caching the content, we can avoid requiring mutable access for read operations
/// like `is_copied` and `is_cut`, while still maintaining correctness by updating
/// the cache when writing.
/// This ensures that is_copied and is_cut can be called without holding a mutable reference to the Clipboard.
/// n.b. maybe_command is not cached, and therefore requires mutable access to the Clipboard.
pub(super) struct Clipboard {
    backend: ClipboardBackend,
    cached_content: Option<(ClipboardCommand, String)>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            backend: ClipboardBackend::new().expect("Can access the clipboard"),
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

    pub(super) fn clear(&mut self) {
        self.cached_content = None;
        if let Err(e) = self.backend.clear() {
            warn!("Failed to clear clipboard: {}", e);
        }
    }

    fn set_clipboard(&mut self, command: ClipboardCommand, from: &str) {
        let text = format!("{command} {from}");
        if let Err(e) = self.backend.set_text(&text) {
            warn!("Failed to set clipboard text: {}", e);
        }
        self.cached_content = Some((command, from.to_string()));
    }
}

pub(super) struct ClipboardPasteContext<'a> {
    clipboard: &'a mut Clipboard,
    destination: PathInfo,
}

impl<'a> ClipboardPasteContext<'a> {
    pub(super) fn new(clipboard: &'a mut Clipboard, destination: PathInfo) -> Self {
        Self {
            clipboard,
            destination,
        }
    }
}

impl TryFrom<ClipboardPasteContext<'_>> for Command {
    type Error = Error;

    fn try_from(context: ClipboardPasteContext) -> Result<Self, Self::Error> {
        if let Ok(text) = context.clipboard.backend.get_text() {
            if let Some((command, from)) = parse_clipboard_text(&text) {
                if let Ok(from_path) = PathInfo::try_from(from.as_str()) {
                    return Ok(command.as_command(from_path, context.destination));
                }
            }
        }
        Err(anyhow!("No valid command in clipboard"))
    }
}

fn parse_clipboard_text(text: &str) -> Option<(ClipboardCommand, String)> {
    let mut parts = text.splitn(2, ' ');
    let command_str = parts.next()?;
    let path = parts.next()?.to_string();
    let command = ClipboardCommand::try_from(command_str).ok()?;
    Some((command, path))
}

#[derive(PartialEq)]
enum ClipboardCommand {
    Copy,
    Move,
}

impl Display for ClipboardCommand {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
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

struct ClipboardBackend {
    clipboard: ClipboardContext,
}

impl ClipboardBackend {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            clipboard: ClipboardProvider::new()
                .map_err(|e| anyhow!("Failed to initialize clipboard: {}", e))?,
        })
    }

    fn get_text(&mut self) -> Result<String, Error> {
        self.clipboard
            .get_contents()
            .map_err(|e| anyhow!("Failed to get clipboard contents: {}", e))
    }

    fn set_text(&mut self, text: &str) -> Result<(), Error> {
        self.clipboard
            .set_contents(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {}", e))
    }

    fn clear(&mut self) -> Result<(), Error> {
        self.clipboard
            .set_contents("".to_string())
            .map_err(|e| anyhow!("Failed to clear clipboard: {}", e))
    }
}
