use std::{
    fmt::{Display, Formatter},
    path::Path,
};

use crate::{
    command::Command,
    file_system::path_info::{PathInfo, compact, quoted},
};
use anyhow::{Context, Error, Result, anyhow};
use arboard::Clipboard as ArboardClipboard;
use log::warn;

pub struct Clipboard {
    backend: Option<ClipboardBackend>,
    /// What this window last wrote; `None` after `clear` or before any write.
    last_written: Option<Written>,
    /// The text of another window's entry that a paste here started from, which
    /// `clear` also blanks.
    adopted: Option<String>,
}

/// Text this window wrote to the clipboard, and the entry it serializes when
/// it was one rather than text copied from a prompt.
///
/// - Without a system clipboard the text is the storage, so copy/paste still
///   works within this window.
/// - The entry keeps the exact paths, which the lossy text may not.
/// - `clear` blanks an entry only while the clipboard still holds its text.
struct Written {
    text: String,
    entry: Option<ClipboardEntry>,
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
            last_written: None,
            adopted: None,
        }
    }
}

impl Clipboard {
    /// Whether a system clipboard backend is available. Without one, entries from
    /// other windows cannot be reached.
    pub fn is_available(&self) -> bool {
        self.backend.is_some()
    }

    /// A clipboard with no backend, the state a failed `try_new` leaves.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            backend: None,
            last_written: None,
            adopted: None,
        }
    }

    /// Clears the entry this window copied or cut, and one adopted by a paste.
    /// Text copied from a prompt is not an entry and stays.
    pub fn clear(&mut self) -> Result<(), Error> {
        let adopted = self.adopted.take();
        let Some(backend) = &mut self.backend else {
            // Without a system clipboard the record is the storage.
            if self.last_written.as_ref().is_some_and(|written| {
                written.entry.is_some() || adopted.as_ref() == Some(&written.text)
            }) {
                self.last_written = None;
            }
            return Ok(());
        };
        let owned = self
            .last_written
            .take_if(|written| written.entry.is_some())
            .map(|written| written.text);
        for text in owned.iter().chain(adopted.iter()) {
            if should_clear(text, || backend.get_string().ok().flatten()) {
                return backend.set_string("");
            }
        }
        Ok(())
    }

    /// Records that a paste of `entry` started, so `clear` consumes it even when
    /// another window wrote it.
    pub fn adopt(&mut self, entry: &ClipboardEntry) {
        if self.last_entry().is_some_and(|(_, own)| own == entry) {
            return;
        }
        self.adopted = Some(entry.to_string());
    }

    /// Reads the system clipboard as a `ClipboardEntry`.
    /// - `Ok(Some(_))`: valid entry
    /// - `Ok(None)`: clipboard empty or holds unrelated text
    /// - `Err(_)`: the clipboard could not be read, or the text looks like an
    ///   entry but is invalid
    ///
    /// The flag is true when this window wrote the entry; an entry from elsewhere
    /// is confirmed before it is acted on.
    pub fn get_clipboard_entry(&mut self) -> Result<Option<(ClipboardEntry, bool)>> {
        match self.read_text()? {
            Some(text) => resolve_clipboard_text(self.last_entry(), &text),
            None => Ok(None),
        }
    }

    /// The entry this window last wrote, beside the text written for it.
    fn last_entry(&self) -> Option<(&str, &ClipboardEntry)> {
        let written = self.last_written.as_ref()?;
        Some((&written.text, written.entry.as_ref()?))
    }

    pub fn get_text(&mut self) -> Option<String> {
        self.read_text().unwrap_or_else(|error| {
            warn!("{error:#}");
            None
        })
    }

    /// The clipboard's text: `Ok(None)` when it is empty or holds no text.
    fn read_text(&mut self) -> Result<Option<String>> {
        match &mut self.backend {
            Some(backend) => backend.get_string(),
            None => Ok(self
                .last_written
                .as_ref()
                .map(|written| written.text.clone())),
        }
    }

    pub fn set_text(&mut self, text: &str) {
        self.adopted = None;
        self.last_written = Some(Written {
            text: text.to_string(),
            entry: None,
        });
        if let Some(backend) = &mut self.backend
            && let Err(e) = backend.set_string(text)
        {
            warn!("Failed to set clipboard text: {e}");
        }
    }

    pub fn set_clipboard_entry(&mut self, entry: &ClipboardEntry) -> Result<(), Error> {
        let text = entry.to_string();
        let result = match &mut self.backend {
            Some(backend) => backend.set_string(&text),
            None => Ok(()),
        };
        self.adopted = None;
        self.last_written = Some(Written {
            text,
            entry: Some(entry.clone()),
        });
        result
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

    /// The operation that pastes this entry into `dest`.
    pub fn into_paste(self, dest: PathInfo) -> Command {
        match self {
            Self::Copy(srcs) => Command::Copy { srcs, dest },
            Self::Move(srcs) => Command::Move { srcs, dest },
        }
    }
}

/// Serialized as `"cp '/path/one' '/path/two'"`, quoted with
/// `shell_words::quote`.
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

/// Parses clipboard text, preferring `last_entry`'s exact paths while the text
/// still matches. The paths are looked up either way.
fn resolve_clipboard_text(
    last_entry: Option<(&str, &ClipboardEntry)>,
    text: &str,
) -> Result<Option<(ClipboardEntry, bool)>> {
    let Some((_, entry)) = last_entry.filter(|(written, _)| *written == text) else {
        return Ok(parse_clipboard_text(text)?.map(|entry| (entry, false)));
    };
    let paths: Vec<PathInfo> = entry
        .paths()
        .iter()
        .map(|path| path_info(&path.path))
        .collect::<Result<_>>()?;
    let entry = match entry {
        ClipboardEntry::Copy(_) => ClipboardEntry::Copy(paths),
        ClipboardEntry::Move(_) => ClipboardEntry::Move(paths),
    };
    Ok(Some((entry, true)))
}

fn path_info(path: &Path) -> Result<PathInfo> {
    PathInfo::try_from(path).with_context(|| format!("Failed to access {}", compact(path)))
}

/// Parses clipboard text: unrelated text is `Ok(None)`, a malformed entry
/// (shaped like "cp <path>"/"mv <path>") is an error. Tokenized once, so
/// classification and parsing agree.
fn parse_clipboard_text(text: &str) -> Result<Option<ClipboardEntry>> {
    let Ok(parts) = shell_words::split(text) else {
        // Unparseable quoting after an operation token is most likely a truncated
        // entry; anything else is unrelated text.
        let mut tokens = text.split_whitespace();
        if matches!(tokens.next(), Some("cp" | "mv")) && tokens.next().is_some() {
            // From another program, so not echoed.
            return Err(anyhow!("Cannot paste the clipboard entry: it is malformed"));
        }
        return Ok(None);
    };
    if parts.len() < 2 || !is_entry_shaped(&parts) {
        return Ok(None);
    }
    if let Some(part) = parts[1..].iter().find(|part| !is_plain_path(part)) {
        return Err(anyhow!(
            "Cannot paste {}: a clipboard path must not contain \".\" or \"..\"",
            quoted(Path::new(part))
        ));
    }
    parse_clipboard_parts(&parts).map(Some)
}

/// Whether no component of `path` is `.` or `..`, so the confirmation names
/// where the paste acts. Split on the separator because `Path::components`
/// drops an inner `.`.
fn is_plain_path(path: &str) -> bool {
    path.split('/').all(|part| part != "." && part != "..")
}

/// Whether the tokens are shaped like an entry filectrl writes: "cp"/"mv"
/// followed by absolute paths. A relative path would resolve against the
/// wrong directory.
fn is_entry_shaped(parts: &[String]) -> bool {
    matches!(parts.first().map(String::as_str), Some("cp" | "mv"))
        && parts[1..].iter().all(|part| part.starts_with('/'))
}

fn parse_clipboard_parts(parts: &[String]) -> Result<ClipboardEntry> {
    let command_str = &parts[0];
    let paths: Vec<_> = parts[1..]
        .iter()
        .map(|p| path_info(Path::new(p)))
        .collect::<Result<Vec<_>, _>>()?;
    // `is_entry_shaped` admitted only "cp" and "mv".
    if command_str == "cp" {
        Ok(ClipboardEntry::Copy(paths))
    } else {
        Ok(ClipboardEntry::Move(paths))
    }
}

struct ClipboardBackend {
    clipboard: ArboardClipboard,
}

impl ClipboardBackend {
    fn try_new() -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            clipboard: ArboardClipboard::new()?,
        })
    }

    fn get_string(&mut self) -> Result<Option<String>, Error> {
        text_or_none(self.clipboard.get_text())
    }

    fn set_string(&mut self, text: &str) -> Result<(), Error> {
        self.clipboard
            .set_text(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {e}"))
    }
}

/// An empty clipboard, or one holding no text, is `None`; any other failure is
/// an error.
fn text_or_none(read: Result<String, arboard::Error>) -> Result<Option<String>, Error> {
    match read {
        Ok(text) => Ok(Some(text)),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(anyhow!("Failed to read the clipboard: {e}")),
    }
}

/// Whether `clear` should blank the system clipboard: only while it still
/// holds `entry_text`. A failed read counts as someone else's.
///
/// `read_current` is a closure because the read blocks on the selection owner,
/// so `clear` reads only when it has an entry to clear.
fn should_clear(entry_text: &str, read_current: impl FnOnce() -> Option<String>) -> bool {
    read_current().as_deref() == Some(entry_text)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test]
    fn an_empty_clipboard_is_nothing_and_a_failed_read_is_an_error() {
        assert_eq!(
            Some("text".to_string()),
            text_or_none(Ok("text".to_string())).unwrap()
        );
        assert_eq!(
            None,
            text_or_none(Err(arboard::Error::ContentNotAvailable)).unwrap()
        );
        assert_eq!(
            "Failed to read the clipboard: The selected clipboard is not supported with the current system configuration.",
            text_or_none(Err(arboard::Error::ClipboardNotSupported))
                .unwrap_err()
                .to_string()
        );
    }

    #[test]
    fn the_clipboard_is_cleared_only_while_this_window_still_owns_it() {
        assert!(should_clear("cp /a", || Some("cp /a".to_string())));

        assert!(!should_clear("cp /a", || Some("other".to_string())));
        assert!(!should_clear("cp /a", || None));
    }

    #[test_case("some copied text" ; "prose")]
    #[test_case("" ; "nothing")]
    #[test_case("cp" ; "an operation without a path")]
    // An apostrophe in copied prose is not a truncation.
    #[test_case("don't" ; "an apostrophe")]
    #[test_case("don't copy that" ; "an apostrophe then more tokens")]
    // A relative-path "entry" is unrelated text.
    #[test_case("cp filectrl-nonexistent-dir/ dist/" ; "a relative shell line")]
    #[test_case("\tmv filectrl-nonexistent-dir/ dist/" ; "an indented relative shell line")]
    #[test_case("see /filectrl-does-not-exist-xyz" ; "prose naming an absolute path")]
    fn unrelated_text_is_not_an_entry(text: &str) {
        assert!(parse_clipboard_text(text).unwrap().is_none());
    }

    #[test]
    fn a_shell_line_naming_existing_relative_paths_is_not_an_entry() {
        let dir = std::env::current_dir().unwrap();
        let name = dir.read_dir().unwrap().next().unwrap().unwrap().file_name();
        let text = format!("cp {}", shell_words::quote(&name.to_string_lossy()));
        assert!(parse_clipboard_text(&text).unwrap().is_none(), "{text}");
    }

    #[test_case("mv '/filectrl-does-not-exist-xyz'" => "Failed to access \"/filectrl-does-not-exist-xyz\"" ; "a missing path")]
    // Classification must not depend on a literal "cp "/"mv " prefix.
    #[test_case("mv\t'/filectrl-does-not-exist-xyz'" => "Failed to access \"/filectrl-does-not-exist-xyz\"" ; "a tab-separated missing path")]
    // A mangled entry: the quote never closes, but "cp" marks it as one.
    #[test_case("cp '/path wi" => "Cannot paste the clipboard entry: it is malformed" ; "a truncated quoted entry")]
    // Existing paths, so the refusal is not the lookup failing.
    #[test_case("cp /usr/../tmp" => "Cannot paste \"/usr/../tmp\": a clipboard path must not contain \".\" or \"..\"" ; "a parent component")]
    #[test_case("mv /tmp /tmp/." => "Cannot paste \"/tmp/.\": a clipboard path must not contain \".\" or \"..\"" ; "a current directory component after a plain path")]
    fn a_malformed_entry_is_an_error(text: &str) -> String {
        parse_clipboard_text(text)
            .expect_err("an entry that cannot be pasted must be reported")
            .to_string()
    }

    #[test]
    fn parse_clipboard_text_parses_valid_entry() {
        let path = std::env::temp_dir();
        let text = format!("cp {}", shell_words::quote(&path.to_string_lossy()));
        let entry = parse_clipboard_text(&text).unwrap().unwrap();
        assert!(matches!(entry, ClipboardEntry::Copy(_)));
    }

    /// An entry must parse back to the same paths in another window.
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

    /// A `..` in the config path would reach every bookmark entry, which another
    /// window would refuse.
    #[test]
    fn a_bookmark_under_a_config_path_with_a_parent_component_can_be_pasted() {
        use crate::{
            app::config::{Config, RuntimeEnv},
            test_support::TempDir,
        };

        let dir = TempDir::new("clipboard_bookmark_dotdot");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("config.toml"), b"").unwrap();
        let config = Config::load(
            RuntimeEnv::default(),
            Some(dir.join("sub/../config.toml")),
            &[],
        )
        .unwrap();
        let bookmarks = config.bookmarks_dir();
        std::fs::create_dir(&bookmarks).unwrap();
        let bookmark = bookmarks.join("mark");
        std::os::unix::fs::symlink(dir.path(), &bookmark).unwrap();
        let entry = ClipboardEntry::Copy(vec![PathInfo::try_from(bookmark.as_path()).unwrap()]);

        let parsed = parse_clipboard_text(&entry.to_string())
            .expect("a filectrl-written entry must parse")
            .expect("a filectrl-written entry is not unrelated text");

        assert_eq!(entry, parsed);
    }

    // macOS refuses a name that is not valid UTF-8.
    #[cfg(target_os = "linux")]
    #[test_case(ClipboardEntry::Copy ; "a copy")]
    #[test_case(ClipboardEntry::Move ; "a cut")]
    fn an_entry_this_process_wrote_keeps_a_name_that_is_not_utf8(
        operation: fn(Vec<PathInfo>) -> ClipboardEntry,
    ) {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        use crate::test_support::TempDir;

        let dir = TempDir::new("clipboard_not_utf8");
        let path = dir.join(OsStr::from_bytes(b"caf\xe9.txt"));
        std::fs::write(&path, b"x").unwrap();
        let entry = operation(vec![PathInfo::try_from(path.as_path()).unwrap()]);

        let mut clipboard = Clipboard::disabled();
        clipboard.set_clipboard_entry(&entry).unwrap();

        // U+FFFD in place of 0xe9 names no file.
        assert_eq!(
            Some((entry, true)),
            clipboard.get_clipboard_entry().unwrap()
        );
    }

    #[test]
    fn text_written_since_the_entry_is_parsed_rather_than_taken_as_the_entry() {
        use crate::test_support::TempDir;

        let dir = TempDir::new("clipboard_other_text");
        let (first, second) = (dir.join("first"), dir.join("second"));
        std::fs::write(&first, b"x").unwrap();
        std::fs::write(&second, b"x").unwrap();
        let entry = ClipboardEntry::Copy(vec![PathInfo::try_from(first.as_path()).unwrap()]);
        let written = (entry.to_string(), entry);

        let other = ClipboardEntry::Move(vec![PathInfo::try_from(second.as_path()).unwrap()]);
        assert_eq!(
            Some((other.clone(), false)),
            resolve_clipboard_text(Some((&written.0, &written.1)), &other.to_string()).unwrap()
        );
    }

    /// Text copied from a prompt is not cleared.
    #[test]
    fn clearing_keeps_text_copied_from_a_prompt() {
        let mut clipboard = Clipboard::disabled();
        let entry = ClipboardEntry::Copy(vec![PathInfo::try_from("/").unwrap()]);
        clipboard.set_clipboard_entry(&entry).unwrap();
        clipboard.set_text("a name");
        clipboard.clear().unwrap();
        assert_eq!(Some("a name".to_string()), clipboard.get_text());
    }

    #[test]
    fn clearing_removes_an_entry() {
        let mut clipboard = Clipboard::disabled();
        let entry = ClipboardEntry::Copy(vec![PathInfo::try_from("/").unwrap()]);
        clipboard.set_clipboard_entry(&entry).unwrap();
        clipboard.clear().unwrap();
        assert_eq!(None, clipboard.get_text());
    }
}
