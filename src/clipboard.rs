use std::{
    fmt::{Debug, Display, Formatter},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Error};
use cli_clipboard::{ClipboardContext, ClipboardProvider};
use log::warn;
use rat_widget::text::clipboard::{Clipboard as RatClipboard, ClipboardError};

use crate::{command::Command, file_system::path_info::PathInfo};

#[derive(Clone, Debug)]
pub(super) struct Clipboard {
    backend: Option<ClipboardBackend>,
}

impl Default for Clipboard {
    fn default() -> Self {
        let backend = match ClipboardBackend::try_new() {
            Ok(backend) => Some(backend),
            Err(err) => {
                warn!("Failed to initialize clipboard: {}", err);
                None
            }
        };

        Self { backend }
    }
}

impl Clipboard {
    pub(super) fn as_rat_clipboard(self) -> Box<dyn RatClipboard> {
        match self.backend {
            Some(backend) => Box::new(backend),
            None => Box::new(NoopClipboardBackend),
        }
    }

    pub(super) fn copy_file(&self, path: &str) -> Result<(), Error> {
        let path = PathInfo::try_from(path)?;
        self.set_clipboard_command(ClipboardCommand::Copy(path))
    }

    pub(super) fn cut_file(&self, path: &str) -> Result<(), Error> {
        let path = PathInfo::try_from(path)?;
        self.set_clipboard_command(ClipboardCommand::Move(path))
    }

    pub(super) fn clear(&self) -> Result<(), Error> {
        match &self.backend {
            Some(backend) => backend.clear(),
            None => Ok(()),
        }
    }

    pub fn get_clipboard_command(&self) -> Option<ClipboardCommand> {
        self.backend.as_ref().and_then(|backend| {
            backend
                .get_string()
                .ok()
                .and_then(|text| text.as_str().try_into().ok())
        })
    }

    pub fn get_command(&self, destination: PathInfo) -> Option<Command> {
        self.get_clipboard_command()
            .map(|command| command.to_command(destination))
    }

    pub fn is_enabled(&self) -> bool {
        self.backend.is_some()
    }

    fn set_clipboard_command(&self, command: ClipboardCommand) -> Result<(), Error> {
        match &self.backend {
            Some(backend) => {
                let text = command.to_string();
                backend.set_string(&text)
            }
            None => Ok(()),
        }
    }
}

#[derive(Clone, Debug)]
pub enum ClipboardCommand {
    Copy(PathInfo),
    Move(PathInfo),
}

impl Display for ClipboardCommand {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let (name, path) = match self {
            Self::Copy(path) => ("cp", path),
            Self::Move(path) => ("mv", path),
        };
        write!(f, "{} {}", name, path)
    }
}

impl TryFrom<&str> for ClipboardCommand {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut parts = value.splitn(2, ' ');

        let Some(command_str) = parts.next() else {
            return Err(anyhow!("Missing command"));
        };

        let Some(path_str) = parts.next() else {
            return Err(anyhow!("Missing path"));
        };

        let path = PathInfo::try_from(path_str)?;

        match command_str {
            "cp" => Ok(Self::Copy(path)),
            "mv" => Ok(Self::Move(path)),
            _ => Err(anyhow!("Invalid ClipboardCommand: {command_str}")),
        }
    }
}

impl ClipboardCommand {
    fn to_command(&self, to: PathInfo) -> Command {
        match self {
            Self::Copy(path) => Command::Copy(path.clone(), to),
            Self::Move(path) => Command::Move(path.clone(), to),
        }
    }
}

#[derive(Clone)]
struct ClipboardBackend {
    clipboard: Arc<Mutex<ClipboardContext>>,
}

impl Debug for ClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBackend")
            .field("clipboard", &"<ClipboardContext>")
            .finish()
    }
}

impl ClipboardBackend {
    fn try_new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            clipboard: Arc::new(Mutex::new(ClipboardProvider::new()?)),
        })
    }

    fn get_string(&self) -> Result<String, Error> {
        let mut clipboard = self
            .clipboard
            .lock()
            .map_err(|e| anyhow!("Failed to lock clipboard: {}", e))?;
        clipboard
            .get_contents()
            .map_err(|e| anyhow!("Failed to get clipboard contents: {}", e))
    }

    fn set_string(&self, text: &str) -> Result<(), Error> {
        let mut clipboard = self
            .clipboard
            .lock()
            .map_err(|e| anyhow!("Failed to lock clipboard: {}", e))?;
        clipboard
            .set_contents(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {}", e))
    }

    fn clear(&self) -> Result<(), Error> {
        self.set_string("")
    }
}

impl RatClipboard for ClipboardBackend {
    fn get_string(&self) -> Result<String, ClipboardError> {
        self.get_string().map_err(|_| ClipboardError)
    }

    fn set_string(&self, s: &str) -> Result<(), ClipboardError> {
        self.set_string(s).map_err(|_| ClipboardError)
    }
}

#[derive(Clone, Debug)]
struct NoopClipboardBackend;

impl RatClipboard for NoopClipboardBackend {
    fn get_string(&self) -> Result<String, ClipboardError> {
        Ok(String::new())
    }

    fn set_string(&self, _s: &str) -> Result<(), ClipboardError> {
        Ok(())
    }
}
