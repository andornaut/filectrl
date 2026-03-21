use std::{
    fmt::{Debug, Display, Formatter},
    sync::{Arc, Mutex},
};

use anyhow::{anyhow, Error};
use cli_clipboard::{ClipboardContext, ClipboardProvider};
use log::warn;
use crate::file_system::path_info::PathInfo;

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
            None => Ok(()),
        }
    }

    pub fn get_clipboard_entry(&self) -> Option<ClipboardEntry> {
        self.backend.as_ref().and_then(|backend| {
            backend
                .get_string()
                .ok()
                .and_then(|text| text.as_str().try_into().ok())
        })
    }

    pub fn get_text(&self) -> Option<String> {
        self.backend.as_ref().and_then(|b| b.get_string().ok())
    }

    pub fn set_text(&self, text: &str) {
        if let Some(backend) = &self.backend {
            let _ = backend.set_string(text);
        }
    }

    pub fn set_clipboard_entry(&self, entry: &ClipboardEntry) -> Result<(), Error> {
        match &self.backend {
            Some(backend) => {
                let text = entry.to_string();
                backend.set_string(&text)
            }
            None => Ok(()),
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

/// Serialized as `"cp /path\n/path2..."` or `"mv /path\n/path2..."` in the system clipboard.
/// Paths are stored unquoted because they are never passed to a shell -- they are parsed back
/// into `PathInfo` values and used directly with Rust filesystem APIs (`fs::copy`, `fs::rename`,
/// etc.), so there is no shell injection risk.
impl Display for ClipboardEntry {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Copy(_) => "cp",
            Self::Move(_) => "mv",
        };
        let paths = self.paths();
        write!(f, "{} {}", name, paths[0])?;
        for path in &paths[1..] {
            write!(f, "\n{}", path)?;
        }
        Ok(())
    }
}

impl TryFrom<&str> for ClipboardEntry {
    type Error = Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut lines = value.lines();

        let first_line = lines.next().ok_or_else(|| anyhow!("Empty clipboard"))?;
        let mut parts = first_line.splitn(2, ' ');

        let command_str = parts.next().ok_or_else(|| anyhow!("Missing command"))?;
        let path_str = parts.next().ok_or_else(|| anyhow!("Missing path"))?;

        let mut paths = vec![PathInfo::try_from(path_str)?];
        for line in lines {
            if !line.is_empty() {
                paths.push(PathInfo::try_from(line)?);
            }
        }

        match command_str {
            "cp" => Ok(Self::Copy(paths)),
            "mv" => Ok(Self::Move(paths)),
            _ => Err(anyhow!("Invalid ClipboardEntry: {command_str}")),
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

