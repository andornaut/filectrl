use std::fmt::{Display, Formatter};

use crate::file_system::path_info::PathInfo;
use anyhow::{Context, Error, Result, anyhow};
use arboard::Clipboard as ArboardClipboard;
use log::warn;

pub struct Clipboard {
    backend: Option<ClipboardBackend>,
    /// In-process fallback so copy/paste within this window keeps working
    /// when no system clipboard is available (e.g. no X11/Wayland session).
    /// The system clipboard, when present, remains the storage, which is what
    /// makes copy/paste work across filectrl windows.
    fallback: Option<String>,
}

impl Default for Clipboard {
    fn default() -> Self {
        let backend = match ClipboardBackend::try_new() {
            Ok(backend) => Some(backend),
            Err(err) => {
                warn!("Failed to initialize clipboard: {err}");
                None
            }
        };

        Self {
            backend,
            fallback: None,
        }
    }
}

impl Clipboard {
    /// Whether a system clipboard backend is available. When it is not (e.g.
    /// no X11/Wayland session), copy/paste cannot work: the system clipboard
    /// is the only storage for clipboard entries.
    pub fn is_available(&self) -> bool {
        self.backend.is_some()
    }

    /// A clipboard with no backend, so nothing reaches the system clipboard.
    /// Every method already handles the backend-less state (it is what a
    /// failed `try_new` leaves behind), so handlers still run their real
    /// paths and return their real `CommandResult`s.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            backend: None,
            fallback: None,
        }
    }

    pub fn clear(&mut self) -> Result<(), Error> {
        self.fallback = None;
        match &mut self.backend {
            Some(backend) => backend.clear(),
            None => Ok(()),
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
        let Some(backend) = &mut self.backend else {
            return self.fallback.clone();
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
        self.fallback = Some(text.to_string());
        if let Some(backend) = &mut self.backend
            && let Err(e) = backend.set_string(text)
        {
            warn!("Failed to set clipboard text: {e}");
        }
    }

    pub fn set_clipboard_entry(&mut self, entry: &ClipboardEntry) -> Result<(), Error> {
        let text = entry.to_string();
        self.fallback = Some(text.clone());
        match &mut self.backend {
            Some(backend) => backend.set_string(&text),
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

/// True when the tokens are shaped like an entry filectrl writes: a "cp"/"mv"
/// token followed by absolute paths. Requiring absolute paths keeps an ordinary
/// copied shell line ("cp build dist") from raising an alert.
fn is_entry_shaped(parts: &[String]) -> bool {
    matches!(parts.first().map(String::as_str), Some("cp" | "mv"))
        && parts[1..].iter().all(|part| part.starts_with('/'))
}

fn parse_clipboard_parts(parts: &[String]) -> Result<ClipboardEntry> {
    let command_str = &parts[0];
    let paths: Vec<_> = parts[1..]
        .iter()
        .map(|p| PathInfo::try_from(p.as_str()).with_context(|| format!("Failed to access {p}")))
        .collect::<Result<Vec<_>, _>>()?;
    match command_str.as_str() {
        "cp" => Ok(ClipboardEntry::Copy(paths)),
        "mv" => Ok(ClipboardEntry::Move(paths)),
        _ => Err(anyhow!("Invalid ClipboardEntry: {command_str}")),
    }
}

struct ClipboardBackend {
    clipboard: ArboardClipboard,
    /// The last text this process wrote to the system clipboard. `clear` uses it
    /// so a window clears only what it wrote itself. Each filectrl window is its
    /// own process with its own tracker, so only the most recent writer clears.
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
            .map_err(|e| anyhow!("Failed to get clipboard contents: {e}"))
    }

    fn set_string(&mut self, text: &str) -> Result<(), Error> {
        self.clipboard
            .set_text(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {e}"))?;
        self.last_written = Some(text.to_string());
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Error> {
        // Cloned so the closure below can borrow `self` mutably for the read.
        let last_written = self.last_written.clone();
        if should_clear(last_written.as_deref(), || self.get_string().ok()) {
            return self.set_string("");
        }
        Ok(())
    }
}

/// Whether `clear` should blank the system clipboard, given what this window
/// last wrote to it and (only where that can decide it) what it holds now.
///
/// `read_current` is a closure rather than a value because reading the
/// clipboard blocks on whichever application owns the selection, and the first
/// two cases answer without it.
fn should_clear(last_written: Option<&str>, read_current: impl FnOnce() -> Option<String>) -> bool {
    let Some(previous) = last_written else {
        // This window never wrote to the clipboard, so blanking it would
        // discard whatever another application or filectrl window put there.
        return false;
    };
    if previous.is_empty() {
        // Already cleared by this window. A second empty write would make
        // arboard reacquire the X11 selection for no change.
        return false;
    }
    // Clear only what this window still owns. Anything else there now (or a
    // read that failed) means another window or application took it over.
    read_current().as_deref() == Some(previous)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports whether the clipboard was read, so the cases that must decide
    /// without reading can say so.
    fn should_clear_reading(last_written: Option<&str>, current: Option<&str>) -> (bool, bool) {
        let mut was_read = false;
        let clear = should_clear(last_written, || {
            was_read = true;
            current.map(ToString::to_string)
        });
        (clear, was_read)
    }

    #[test]
    fn the_clipboard_is_cleared_only_while_this_window_still_owns_it() {
        // Still holding what this window wrote, so clearing it discards only
        // this window's own entry.
        assert_eq!(
            (true, true),
            should_clear_reading(Some("cp /a"), Some("cp /a"))
        );

        // Another window or application has written since. Blanking now would
        // throw away someone else's clipboard.
        assert_eq!(
            (false, true),
            should_clear_reading(Some("cp /a"), Some("other"))
        );
        // A read that failed says nothing about ownership, so it is not a
        // licence to overwrite either.
        assert_eq!((false, true), should_clear_reading(Some("cp /a"), None));
    }

    #[test]
    fn a_window_that_wrote_nothing_clears_nothing_and_does_not_read() {
        // Reading blocks on whichever application owns the selection, so these
        // two cases must answer without touching it.
        assert_eq!((false, false), should_clear_reading(None, Some("someone")));
        // Already cleared by this window: a second empty write would reacquire
        // the X11 selection for no change.
        assert_eq!((false, false), should_clear_reading(Some(""), Some("")));
    }

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

    /// The clipboard is the cross-window contract, so an entry this process
    /// wrote has to parse back to the same paths in another window. Quoting is
    /// what carries a name a shell would otherwise split or expand.
    #[test]
    fn an_entry_round_trips_through_its_serialized_form() {
        use crate::test_support::TempDir;

        let dir = TempDir::new("clipboard_round_trip");
        let awkward = ["my report.txt", "it's here", "a;b$c", "two  spaces"];
        let paths: Vec<PathInfo> = awkward
            .iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, b"x").unwrap();
                PathInfo::try_from(path.as_path()).unwrap()
            })
            .collect();

        let entry = ClipboardEntry::Move(paths.clone());
        let parsed = parse_clipboard_text(&entry.to_string())
            .expect("a filectrl-written entry must parse")
            .expect("a filectrl-written entry is not unrelated text");

        assert_eq!(ClipboardEntry::Move(paths), parsed);
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
