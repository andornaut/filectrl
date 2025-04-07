use std::fmt::Display;
use std::process::Command;

use anyhow::{anyhow, Error};
use arboard::Clipboard as Arboard;
use log::{debug, warn};

use crate::{command::Command as FileCommand, file_system::path_info::PathInfo};

trait ClipboardBackend {
    fn set_text(&mut self, text: &str) -> Result<(), Error>;
    fn clear(&mut self) -> Result<(), Error>;
}

struct ArboardBackend {
    clipboard: Arboard,
}

impl ArboardBackend {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            clipboard: Arboard::new()?,
        })
    }
}

impl ClipboardBackend for ArboardBackend {
    fn set_text(&mut self, text: &str) -> Result<(), Error> {
        self.clipboard.set_text(text)?;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Error> {
        self.clipboard.clear()?;
        Ok(())
    }
}

struct WlCopyBackend;

impl WlCopyBackend {
    fn new() -> Result<Self, Error> {
        Ok(Self)
    }
}

impl ClipboardBackend for WlCopyBackend {
    fn set_text(&mut self, text: &str) -> Result<(), Error> {
        let mut child = Command::new("wl-copy")
            .arg(text)
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn wl-copy: {}", e))?;

        child
            .wait()
            .map_err(|e| anyhow!("Failed to wait for wl-copy: {}", e))?;
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Error> {
        let mut child = Command::new("wl-copy")
            .arg("--clear")
            .spawn()
            .map_err(|e| anyhow!("Failed to spawn wl-copy --clear: {}", e))?;

        child
            .wait()
            .map_err(|e| anyhow!("Failed to wait for wl-copy --clear: {}", e))?;
        Ok(())
    }
}

/// A clipboard that caches its content to avoid requiring mutable access for read operations.
///
/// The clipboard is a shared system resource that requires synchronization when reading.
/// By caching the content, we can avoid requiring mutable access for read operations
/// like `is_copied` and `is_cut`, while still maintaining correctness by updating
/// the cache when writing.
/// This ensures that is_copied and is_cut can be called without holding a mutable reference to the Clipboard.
pub(super) struct Clipboard {
    backend: Box<dyn ClipboardBackend>,
    cached_content: Option<(ClipboardCommand, String)>,
}

impl Default for Clipboard {
    fn default() -> Self {
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
        debug!("Running under Wayland: {}", is_wayland);

        // Try wl-copy first if running under Wayland
        let backend: Box<dyn ClipboardBackend> = if is_wayland {
            match WlCopyBackend::new() {
                Ok(backend) => Box::new(backend),
                Err(e) => {
                    warn!("Failed to initialize wl-copy backend: {}", e);
                    Box::new(ArboardBackend::new().expect("Can access the clipboard"))
                }
            }
        } else {
            Box::new(ArboardBackend::new().expect("Can access the clipboard"))
        };

        Self {
            backend,
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

    pub(super) fn maybe_command(&self, to: PathInfo) -> Option<FileCommand> {
        self.cached_content.as_ref().and_then(|(command, from)| {
            PathInfo::try_from(from.as_str())
                .map(|from| command.as_command(from, to))
                .ok()
        })
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
    fn as_command(&self, from: PathInfo, to: PathInfo) -> FileCommand {
        match self {
            Self::Copy => FileCommand::Copy(from, to),
            Self::Move => FileCommand::Move(from, to),
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
