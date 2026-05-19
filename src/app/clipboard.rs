use std::{
    fmt::{Debug, Display, Formatter},
    sync::{Arc, Mutex},
};

use crate::file_system::path_info::PathInfo;
use anyhow::{Error, Result, anyhow};
use arboard::Clipboard as ArboardClipboard;
use log::warn;

#[derive(Clone, Debug)]
pub struct Clipboard {
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
    pub fn clear(&self) -> Result<(), Error> {
        match &self.backend {
            Some(backend) => backend.clear(),
            None => {
                warn!("No clipboard backend available");
                Ok(())
            }
        }
    }

    pub fn get_clipboard_entry(&self) -> Option<ClipboardEntry> {
        self.get_text()?.as_str().try_into().ok()
    }

    pub fn get_text(&self) -> Option<String> {
        let backend = match &self.backend {
            Some(b) => b,
            None => {
                warn!("No clipboard backend available");
                return None;
            }
        };
        match backend.get_string() {
            Ok(t) => Some(t),
            Err(e) => {
                warn!("Failed to read clipboard: {e}");
                None
            }
        }
    }

    pub fn set_text(&self, text: &str) {
        match &self.backend {
            Some(backend) => {
                let _ = backend.set_string(text);
            }
            None => warn!("No clipboard backend available"),
        }
    }

    pub fn set_clipboard_entry(&self, entry: &ClipboardEntry) -> Result<(), Error> {
        match &self.backend {
            Some(backend) => {
                let text = entry.to_string();
                backend.set_string(&text)
            }
            None => {
                warn!("No clipboard backend available");
                Ok(())
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ClipboardEntry {
    Copy(Vec<PathInfo>),
    Move(Vec<PathInfo>),
}

impl ClipboardEntry {
    pub fn paths(&self) -> &[PathInfo] {
        match self {
            Self::Copy(paths) | Self::Move(paths) => paths,
        }
    }
}

/// Serialized as `"cp '/path/one' '/path/two'"` in the system clipboard.
/// Paths are quoted with `shell_words::quote` so filenames containing spaces,
/// newlines, or other shell metacharacters round-trip correctly.
impl Display for ClipboardEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Copy(_) => "cp",
            Self::Move(_) => "mv",
        };
        write!(f, "{name}")?;
        for path in self.paths() {
            write!(f, " {}", shell_words::quote(&path.path.to_string_lossy()))?;
        }
        Ok(())
    }
}

impl TryFrom<&str> for ClipboardEntry {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let parts =
            shell_words::split(value).map_err(|e| anyhow!("Invalid clipboard format: {e}"))?;

        if parts.len() < 2 {
            return Err(anyhow!("Missing command or path in clipboard"));
        }

        parse_clipboard_parts(&parts)
    }
}

fn parse_clipboard_parts(parts: &[String]) -> Result<ClipboardEntry> {
    let command_str = &parts[0];
    let paths: Vec<_> = parts[1..]
        .iter()
        .map(|p| PathInfo::try_from(p.as_str()))
        .collect::<Result<Vec<_>, _>>()?;
    match command_str.as_str() {
        "cp" => Ok(ClipboardEntry::Copy(paths)),
        "mv" => Ok(ClipboardEntry::Move(paths)),
        _ => Err(anyhow!("Invalid ClipboardEntry: {command_str}")),
    }
}

#[derive(Clone)]
struct ClipboardBackend {
    clipboard: Arc<Mutex<ArboardClipboard>>,
    /// The last text this process wrote to the system clipboard, if any.
    /// Used by `clear` so a window only clears the clipboard when it was the
    /// last writer (its written content still matches the current content).
    /// Multiple filectrl windows are separate processes, each with its own
    /// tracker, so only the most recent writer will clear.
    last_written: Arc<Mutex<Option<String>>>,
}

impl Debug for ClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBackend")
            .field("clipboard", &"<arboard::Clipboard>")
            .finish()
    }
}

impl ClipboardBackend {
    fn try_new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            clipboard: Arc::new(Mutex::new(ArboardClipboard::new()?)),
            last_written: Arc::new(Mutex::new(None)),
        })
    }

    fn get_string(&self) -> Result<String, Error> {
        let mut clipboard = self
            .clipboard
            .lock()
            .map_err(|e| anyhow!("Failed to lock clipboard: {}", e))?;
        clipboard
            .get_text()
            .map_err(|e| anyhow!("Failed to get clipboard contents: {}", e))
    }

    fn set_string(&self, text: &str) -> Result<(), Error> {
        let mut clipboard = self
            .clipboard
            .lock()
            .map_err(|e| anyhow!("Failed to lock clipboard: {}", e))?;
        clipboard
            .set_text(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {}", e))?;
        *self
            .last_written
            .lock()
            .map_err(|e| anyhow!("Failed to lock clipboard tracker: {}", e))? =
            Some(text.to_string());
        Ok(())
    }

    fn clear(&self) -> Result<(), Error> {
        let last_written = self
            .last_written
            .lock()
            .map_err(|e| anyhow!("Failed to lock clipboard tracker: {}", e))?
            .clone();

        let Some(prev) = last_written else {
            // This window never wrote to the clipboard; leave it untouched so
            // we don't clobber content set by another app or filectrl window.
            return Ok(());
        };
        if prev.is_empty() {
            // This window already cleared the clipboard; nothing to do (and
            // avoids a redundant empty write that would make arboard reacquire
            // X11 selection ownership).
            return Ok(());
        }
        // Only clear if the clipboard still holds exactly what this window
        // wrote. If it differs (or is empty/unreadable), another window or app
        // owns it now and we must not overwrite it.
        match self.get_string() {
            Ok(current) if current == prev => self.set_string(""),
            _ => Ok(()),
        }
    }
}
