use std::fmt::{Display, Formatter};

use crate::file_system::path_info::PathInfo;
use anyhow::{Context, Error, Result, anyhow};
use arboard::Clipboard as ArboardClipboard;
use log::warn;

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
    pub fn clear(&mut self) -> Result<(), Error> {
        match &mut self.backend {
            Some(backend) => backend.clear(),
            None => {
                warn!("No clipboard backend available");
                Ok(())
            }
        }
    }

    /// Reads the system clipboard as a `ClipboardEntry`.
    /// - `Ok(Some(_))`: valid entry
    /// - `Ok(None)`: clipboard empty, unreadable, or holds unrelated text
    /// - `Err(_)`: the text looks like an entry ("cp "/"mv " prefix) but is
    ///   invalid (e.g. a path that no longer exists); callers should surface
    ///   this to the user rather than silently doing nothing
    pub fn get_clipboard_entry(&mut self) -> Result<Option<ClipboardEntry>> {
        match self.get_text() {
            Some(text) => parse_clipboard_text(&text),
            None => Ok(None),
        }
    }

    pub fn get_text(&mut self) -> Option<String> {
        let backend = match &mut self.backend {
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

    pub fn set_text(&mut self, text: &str) {
        match &mut self.backend {
            Some(backend) => {
                if let Err(e) = backend.set_string(text) {
                    warn!("Failed to set clipboard text: {e}");
                }
            }
            None => warn!("No clipboard backend available"),
        }
    }

    pub fn set_clipboard_entry(&mut self, entry: &ClipboardEntry) -> Result<(), Error> {
        match &mut self.backend {
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

/// Parses clipboard text, distinguishing unrelated text (ignored) from a
/// malformed entry (shaped like "cp <path>"/"mv <path>" but failing to
/// convert), which is returned as an error so the caller can alert the user.
/// The text is tokenized exactly once, so classification and parsing cannot
/// disagree about token boundaries.
fn parse_clipboard_text(text: &str) -> Result<Option<ClipboardEntry>> {
    let Ok(parts) = shell_words::split(text) else {
        // Unparseable quoting after an operation token is most likely a
        // truncated entry (filectrl quotes paths), so surface the error;
        // anything else is unrelated text.
        let mut tokens = text.split_whitespace();
        if matches!(tokens.next(), Some("cp" | "mv")) && tokens.next().is_some() {
            return Err(anyhow!("Malformed clipboard entry: {text:?}"));
        }
        return Ok(None);
    };
    if parts.len() < 2 {
        return Ok(None);
    }
    match parse_clipboard_parts(&parts) {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if is_entry_shaped(&parts) => Err(error),
        Err(_) => Ok(None),
    }
}

/// True when the parsed tokens are shaped like an entry filectrl itself
/// writes: a "cp"/"mv" command token followed by absolute paths. The
/// absolute-path requirement keeps ordinary copied shell lines (e.g. an
/// indented "cp build dist" from a script) from raising alerts: filectrl
/// always writes absolute paths.
fn is_entry_shaped(parts: &[String]) -> bool {
    matches!(parts.first().map(String::as_str), Some("cp" | "mv"))
        && parts[1..].iter().all(|part| part.starts_with('/'))
}

fn parse_clipboard_parts(parts: &[String]) -> Result<ClipboardEntry> {
    let command_str = &parts[0];
    let paths: Vec<_> = parts[1..]
        .iter()
        .map(|p| PathInfo::try_from(p.as_str()).with_context(|| format!("Cannot access '{p}'")))
        .collect::<Result<Vec<_>, _>>()?;
    match command_str.as_str() {
        "cp" => Ok(ClipboardEntry::Copy(paths)),
        "mv" => Ok(ClipboardEntry::Move(paths)),
        _ => Err(anyhow!("Invalid ClipboardEntry: {command_str}")),
    }
}

struct ClipboardBackend {
    clipboard: ArboardClipboard,
    /// The last text this process wrote to the system clipboard, if any.
    /// Used by `clear` so a window only clears the clipboard when it was the
    /// last writer (its written content still matches the current content).
    /// Multiple filectrl windows are separate processes, each with its own
    /// tracker, so only the most recent writer will clear.
    last_written: Option<String>,
}

impl ClipboardBackend {
    fn try_new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            clipboard: ArboardClipboard::new()?,
            last_written: None,
        })
    }

    fn get_string(&mut self) -> Result<String, Error> {
        self.clipboard
            .get_text()
            .map_err(|e| anyhow!("Failed to get clipboard contents: {}", e))
    }

    fn set_string(&mut self, text: &str) -> Result<(), Error> {
        self.clipboard
            .set_text(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {}", e))?;
        self.last_written = Some(text.to_string());
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Error> {
        let Some(prev) = self.last_written.clone() else {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_clipboard_text_ignores_unrelated_text() {
        assert!(parse_clipboard_text("some copied text").unwrap().is_none());
        assert!(parse_clipboard_text("").unwrap().is_none());
        // "cp" without a path is not treated as an entry
        assert!(parse_clipboard_text("cp").unwrap().is_none());
    }

    #[test]
    fn parse_clipboard_text_parses_valid_entry() {
        let path = std::env::temp_dir();
        let text = format!("cp {}", shell_words::quote(&path.to_string_lossy()));
        let entry = parse_clipboard_text(&text).unwrap().unwrap();
        assert!(matches!(entry, ClipboardEntry::Copy(_)));
    }

    #[test]
    fn parse_clipboard_text_errors_on_tab_separated_missing_path() {
        // The entry parser splits on any whitespace, so classification must
        // not depend on a literal "cp "/"mv " space prefix.
        assert!(parse_clipboard_text("mv\t'/filectrl-does-not-exist-xyz'").is_err());
    }

    #[test]
    fn parse_clipboard_text_errors_on_truncated_quoted_entry() {
        // A filectrl-written entry mangled by a clipboard manager: the quote
        // never closes, so tokenizing fails, but the operation token makes
        // it clearly an entry, not prose.
        assert!(parse_clipboard_text("cp '/path wi").is_err());
        // Unclosed quotes without an operation token stay silent.
        assert!(parse_clipboard_text("don't").unwrap().is_none());
    }

    #[test]
    fn parse_clipboard_text_ignores_relative_path_shell_lines() {
        // An ordinary copied shell line: filectrl writes absolute paths only,
        // so a failing relative-path "entry" is unrelated text, not an error.
        assert!(
            parse_clipboard_text("cp filectrl-nonexistent-dir/ dist/")
                .unwrap()
                .is_none()
        );
        assert!(
            parse_clipboard_text("\tmv filectrl-nonexistent-dir/ dist/")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parse_clipboard_text_errors_on_missing_path() {
        let result = parse_clipboard_text("mv '/filectrl-does-not-exist-xyz'");
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("filectrl-does-not-exist-xyz"),
            "error should name the path: {error}"
        );
    }
}
