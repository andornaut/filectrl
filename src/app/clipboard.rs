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
    /// What this window last wrote. `None` once `clear` has run, or before
    /// anything was written.
    last_written: Option<Written>,
    /// The text of an entry another window wrote that a paste here started
    /// from. A paste consumes the clipboard whoever wrote it, so `clear`
    /// blanks this too while the clipboard still holds it.
    adopted: Option<String>,
}

/// Text this window wrote to the clipboard, and the entry it serializes when
/// it was one rather than text copied from a prompt.
///
/// - Without a system clipboard (no X11/Wayland session) the text is the
///   storage, so copy/paste within this window keeps working. With one, the
///   system clipboard is the storage, which is what makes copy/paste work
///   across filectrl windows.
/// - The entry keeps the exact paths. The text renders a name that is not
///   valid UTF-8 lossily, so parsing it back would name a different file;
///   while the clipboard still holds the text, the paths are taken from here.
/// - `clear` blanks only an entry, and only while the clipboard still holds
///   its text, so text copied from a prompt, or written since by another
///   application or filectrl window, is never discarded. Each window is its
///   own process with its own record, so only the most recent writer clears.
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
    /// Whether a system clipboard backend is available. When it is not (e.g.
    /// no X11/Wayland session), copy/paste still works within this window
    /// through the text it last wrote, but an entry copied in another window
    /// cannot be reached.
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
            last_written: None,
            adopted: None,
        }
    }

    /// Clears the entry this window copied or cut, and one written elsewhere
    /// that a paste here started from (`adopt`). Text copied from a prompt is
    /// not an entry, so it stays for other applications to paste.
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
            if should_clear(text, || backend.get_string().ok()) {
                return backend.set_string("");
            }
        }
        Ok(())
    }

    /// Records that a paste of `entry` started, so `clear` consumes it even
    /// when another window wrote it. An entry this window wrote needs nothing.
    pub fn adopt(&mut self, entry: &ClipboardEntry) {
        if self.last_entry().is_some_and(|(_, own)| own == entry) {
            return;
        }
        self.adopted = Some(entry.to_string());
    }

    /// Reads the system clipboard as a `ClipboardEntry`.
    /// - `Ok(Some(_))`: valid entry
    /// - `Ok(None)`: clipboard empty, unreadable, or holds unrelated text
    /// - `Err(_)`: the text looks like an entry ("cp "/"mv " prefix) but is
    ///   invalid (e.g. a path that no longer exists); callers should surface
    ///   this to the user rather than silently doing nothing
    ///
    /// The flag is true when this window wrote the entry. Any program can put
    /// text shaped like one on the clipboard, so an entry from elsewhere is
    /// confirmed before it is acted on.
    pub fn get_clipboard_entry(&mut self) -> Result<Option<(ClipboardEntry, bool)>> {
        match self.get_text() {
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
        let Some(backend) = &mut self.backend else {
            return self
                .last_written
                .as_ref()
                .map(|written| written.text.clone());
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

/// Parses clipboard text, preferring `last_entry`'s exact paths when the text is
/// still what was written for it. The paths are looked up again either way, so
/// a path removed since the copy is reported the same as one parsed from text.
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
            // The text came from another program, so it is not echoed.
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

/// Whether every component of `path` names an entry: no `.` and no `..`. A
/// `..` makes a path lead somewhere other than the directories it spells out,
/// so the confirmation would name one location and the paste act on another.
/// Split on the separator rather than walking `Path::components`, which drops
/// a `.` it finds past the start.
fn is_plain_path(path: &str) -> bool {
    path.split('/').all(|part| part != "." && part != "..")
}

/// True when the tokens are shaped like an entry filectrl writes: a "cp"/"mv"
/// token followed by absolute paths. An ordinary copied shell line
/// ("cp build dist") is not one, even when its paths exist: a relative path
/// would resolve against the directory filectrl was started in, which is not
/// the one it names.
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
    match command_str.as_str() {
        "cp" => Ok(ClipboardEntry::Copy(paths)),
        "mv" => Ok(ClipboardEntry::Move(paths)),
        _ => Err(anyhow!("Invalid ClipboardEntry: {command_str}")),
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

    fn get_string(&mut self) -> Result<String, Error> {
        self.clipboard
            .get_text()
            .map_err(|e| anyhow!("Failed to get clipboard contents: {e}"))
    }

    fn set_string(&mut self, text: &str) -> Result<(), Error> {
        self.clipboard
            .set_text(text.to_string())
            .map_err(|e| anyhow!("Failed to set clipboard contents: {e}"))
    }
}

/// Whether `clear` should blank the system clipboard holding the entry this
/// window wrote as `entry_text`: only while it still holds that text. Anything
/// else there now (or a read that failed) means another window or application
/// took it over.
///
/// `read_current` is a closure so that tests can see the read happen; reading
/// blocks on whichever application owns the selection, so `clear` reads only
/// when it has an entry to clear.
fn should_clear(entry_text: &str, read_current: impl FnOnce() -> Option<String>) -> bool {
    read_current().as_deref() == Some(entry_text)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test]
    fn the_clipboard_is_cleared_only_while_this_window_still_owns_it() {
        // Still holding what this window wrote, so clearing it discards only
        // this window's own entry.
        assert!(should_clear("cp /a", || Some("cp /a".to_string())));

        // Another window or application has written since. Blanking now would
        // throw away someone else's clipboard.
        assert!(!should_clear("cp /a", || Some("other".to_string())));
        // A read that failed says nothing about ownership, so it is not a
        // licence to overwrite either.
        assert!(!should_clear("cp /a", || None));
    }

    #[test_case("some copied text" ; "prose")]
    #[test_case("" ; "nothing")]
    #[test_case("cp" ; "an operation without a path")]
    // Unclosed quotes without an operation token stay silent, however many
    // tokens follow: an apostrophe in copied prose is not a truncation.
    #[test_case("don't" ; "an apostrophe")]
    #[test_case("don't copy that" ; "an apostrophe then more tokens")]
    // An ordinary copied shell line: filectrl writes absolute paths only, so a
    // failing relative-path "entry" is unrelated text, not an error.
    #[test_case("cp filectrl-nonexistent-dir/ dist/" ; "a relative shell line")]
    #[test_case("\tmv filectrl-nonexistent-dir/ dist/" ; "an indented relative shell line")]
    // An absolute path alone does not make an entry: only "cp"/"mv" does.
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
    // The entry parser splits on any whitespace, so classification must not
    // depend on a literal "cp "/"mv " space prefix.
    #[test_case("mv\t'/filectrl-does-not-exist-xyz'" => "Failed to access \"/filectrl-does-not-exist-xyz\"" ; "a tab-separated missing path")]
    // A filectrl-written entry mangled by a clipboard manager: the quote never
    // closes, so tokenizing fails, but the operation token makes it clearly an
    // entry, not prose.
    #[test_case("cp '/path wi" => "Cannot paste the clipboard entry: it is malformed" ; "a truncated quoted entry")]
    // Paths that exist, so the refusal is not the lookup failing. Each leads
    // somewhere other than the directories it names.
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

    /// A bookmark is listed under the config directory, so a `..` in the
    /// config path would reach every entry copied from the bookmarks, and
    /// another window would refuse to paste it.
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

        // Another window has no `last_entry`, so it parses the text.
        let parsed = parse_clipboard_text(&entry.to_string())
            .expect("a filectrl-written entry must parse")
            .expect("a filectrl-written entry is not unrelated text");

        assert_eq!(entry, parsed);
    }

    // Linux only: macOS file systems refuse a name that is not valid UTF-8.
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

        // The text holds U+FFFD in place of 0xe9, which names no file.
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

        // Another window's entry: the text no longer matches, so it is what
        // counts, and it is not this window's own.
        let other = ClipboardEntry::Move(vec![PathInfo::try_from(second.as_path()).unwrap()]);
        assert_eq!(
            Some((other.clone(), false)),
            resolve_clipboard_text(Some((&written.0, &written.1)), &other.to_string()).unwrap()
        );
    }

    /// Text copied from a prompt replaces the entry as what this window
    /// wrote, so clearing leaves it for other applications to paste.
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
