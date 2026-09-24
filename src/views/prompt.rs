mod handler;
mod view;
mod widget;

use std::path::PathBuf;

use ratatui::buffer::CellWidth;
use ratatui::layout::Rect;
use ratatui_textarea::{CursorMove, TextArea};

use super::{View, as_dimension, unicode::pluralize_items};
use crate::{
    app::clipboard::ClipboardEntry,
    command::{Command, PromptAction, result::CommandResult},
    file_system::path_info::{PathInfo, compact, quoted},
};

/// Paths a confirmation of a paste from elsewhere lists, one per line, before
/// it counts the rest.
const MAX_LISTED_PASTE_PATHS: usize = 5;

/// Entries of a directory the Goto prompt reads for its suggestions. The read
/// runs on the UI thread, so a larger directory gets no suggestions rather
/// than a stall.
const MAX_SUGGESTION_ENTRIES: usize = 10_000;

#[derive(Default)]
pub(super) struct PromptView {
    actions: PromptAction,
    text_area: TextArea<'static>,
    initial_text: String,
    render_area: Rect,
    /// Horizontal scroll offset (in display columns), mirroring tui-textarea's internal viewport.
    scroll_col: u16,
    /// Goto: the directory that relative input is resolved against.
    basedir: PathBuf,
    /// Goto: prefix-matching entries `(name, is_dir)`, sorted ascending.
    suggestions: Vec<(String, bool)>,
    /// Goto: index of the currently shown suggestion.
    suggestion_index: usize,
    /// Goto: the directory `cached_entries` was read from. Avoids re-reading
    /// the filesystem on every keystroke while the directory prefix is unchanged.
    cached_dir: Option<PathBuf>,
    /// Goto: every entry of `cached_dir` as `(name, is_dir)`, sorted ascending.
    cached_entries: Vec<(String, bool)>,
}

impl PromptView {
    fn label(&self) -> String {
        match &self.actions {
            PromptAction::Chmod { paths, .. } => {
                format!(" Chmod {} (octal) ", pluralize_items(paths.len()))
            }
            PromptAction::AddBookmark { .. } => " Add bookmark ".to_string(),
            PromptAction::CreateDirectory => " New directory ".to_string(),
            PromptAction::Delete(count) => {
                format!(" Delete {}? (y/n) ", pluralize_items(*count))
            }
            PromptAction::Filter(_) => " Filter ".to_string(),
            PromptAction::Goto { .. } => " Go to ".to_string(),
            PromptAction::Rename { .. } => " Rename ".to_string(),
            PromptAction::Search(_) => " Search ".to_string(),
            PromptAction::ConfirmPaste { entry, .. } => {
                let verb = match entry {
                    ClipboardEntry::Copy(_) => "copy",
                    ClipboardEntry::Move(_) => "move",
                };
                // Each path in full: another program chose it, and an elided
                // middle would hide which directory it is in.
                let paths = entry.paths();
                if let [path] = paths {
                    return format!(
                        " Clipboard from elsewhere: {verb} {} here? (y/n) ",
                        quoted(&path.path)
                    );
                }
                // Every path is named, up to a few lines' worth: the text came
                // from another program, which chose what follows the first.
                let mut lines = vec![format!(
                    " Clipboard from elsewhere: {verb} {} here? (y/n) ",
                    pluralize_items(paths.len())
                )];
                lines.extend(
                    paths
                        .iter()
                        .take(MAX_LISTED_PASTE_PATHS)
                        .map(|path| format!("   {}", quoted(&path.path))),
                );
                if paths.len() > MAX_LISTED_PASTE_PATHS {
                    let more = paths.len() - MAX_LISTED_PASTE_PATHS;
                    lines.push(format!("   and {more} more"));
                }
                lines.join("\n")
            }
            // `name` is the table's display name, already escaped, so it is
            // quoted as it is rather than escaped a second time.
            PromptAction::Conflict {
                name,
                can_overwrite: true,
            } => format!(" \"{name}\" exists: [s]kip, [S]kip all, [o]verwrite, [O]verwrite all "),
            PromptAction::Conflict {
                name,
                can_overwrite: false,
            } => format!(" \"{name}\" exists as a directory: [s]kip, [S]kip all "),
        }
    }

    fn open(&mut self, kind: &PromptAction) -> CommandResult {
        let text = match kind {
            PromptAction::Chmod { mode, .. } => mode.clone(),
            PromptAction::Conflict { .. }
            | PromptAction::ConfirmPaste { .. }
            | PromptAction::CreateDirectory
            | PromptAction::Delete(_)
            | PromptAction::Goto { .. } => String::new(),
            PromptAction::AddBookmark { name: text, .. }
            | PromptAction::Filter(text)
            | PromptAction::Rename { name: text, .. }
            | PromptAction::Search(text) => text.clone(),
        };
        self.actions = kind.clone();
        self.initial_text.clone_from(&text);
        self.reset_text(&text);
        if let PromptAction::Goto { directory } = kind {
            self.basedir.clone_from(directory);
            self.suggestion_index = 0;
            // Drop any cache from a previous prompt so on-disk changes since
            // it was last open are picked up.
            self.cached_dir = None;
            self.cached_entries.clear();
            self.refresh_suggestions();
        }
        CommandResult::Handled
    }

    fn reset_text(&mut self, text: &str) {
        let mut text_area = TextArea::from([text]);
        text_area.move_cursor(CursorMove::End);
        text_area.set_cursor_line_style(ratatui::style::Style::default());
        self.text_area = text_area;
        self.scroll_col = 0;
    }

    /// Mirrors tui-textarea's internal `scroll_top_col` logic to track horizontal scroll offset
    /// without accessing the crate-private `viewport` field. Call after each `render_widget`.
    fn update_scroll_col(&mut self, width: u16) {
        if width == 0 {
            return;
        }
        let cursor_display_col = as_dimension(self.text_area.screen_cursor().col);
        self.scroll_col = next_scroll_top(self.scroll_col, cursor_display_col, width);
    }

    /// Converts a display-column offset (viewport-relative + scroll) to a character index
    /// suitable for `CursorMove::Jump`.
    fn display_col_to_char_idx(&self, display_col: u16) -> u16 {
        let line = &self.text_area.lines()[0];
        let mut remaining = display_col;
        let mut idx = 0u16;
        let mut chars = line.char_indices().peekable();
        while let Some((start, _)) = chars.next() {
            let end = chars.peek().map_or(line.len(), |(i, _)| *i);
            let w = line[start..end].cell_width();
            if remaining < w {
                break;
            }
            remaining -= w;
            idx += 1;
        }
        idx
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.text_area.lines().join("");
        match &self.actions {
            PromptAction::Chmod { paths, .. } => Command::Chmod {
                paths: paths.clone(),
                mode: value,
            },
            PromptAction::AddBookmark { directory, .. } => Command::AddBookmark {
                directory: directory.clone(),
                name: value,
            },
            PromptAction::CreateDirectory => Command::CreateDirectory(value),
            // The confirmation prompts resolve in `handle_key` on a single
            // keypress, so submit never reaches them; treat it as a cancel
            // rather than guessing an answer on the user's behalf.
            PromptAction::Conflict { .. } | PromptAction::ConfirmPaste { .. } => {
                Command::CancelPrompt
            }
            PromptAction::Delete(_) => Command::ConfirmDelete,
            PromptAction::Filter(_) => Command::FilterChanged(value),
            PromptAction::Goto { .. } => {
                let path = self.resolve_path(&value);
                if path.exists() {
                    match PathInfo::try_from(&path) {
                        Ok(info) => Command::Open(info),
                        Err(error) => Command::AlertWarn(format!(
                            "Failed to access {}: {error}",
                            compact(&path)
                        )),
                    }
                } else {
                    Command::AlertWarn(format!("Path does not exist: {}", compact(&path)))
                }
            }
            // Submitting the name as it was offered changes nothing, and for a
            // name that is not UTF-8 the offered text is a lossy spelling that
            // names a different file.
            PromptAction::Rename { .. } if value == self.initial_text => Command::CancelPrompt,
            PromptAction::Rename { path, .. } => Command::Rename {
                path: path.clone(),
                name: value,
            },
            // An empty query would match everything; treat it like Esc.
            PromptAction::Search(_) if value.is_empty() => Command::CancelPrompt,
            PromptAction::Search(_) => Command::StartSearch(value),
        }
        .into()
    }

    /// Resolve user input to a path: leading `~` expands to home, absolute
    /// paths are used as-is, and relative input is joined onto `basedir`.
    fn resolve_path(&self, input: &str) -> PathBuf {
        if let Some(rest) = input.strip_prefix('~')
            && let Some(base) = directories::BaseDirs::new()
        {
            let home = base.home_dir();
            let rest = rest.strip_prefix('/').unwrap_or(rest);
            return if rest.is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            };
        }
        // `join` replaces the base with an absolute input.
        self.basedir.join(input)
    }

    /// Splits the current input into `(dir_prefix, partial)` at the last `/`.
    /// `dir_prefix` includes the trailing `/`; `partial` is the basename being typed.
    fn split_input(input: &str) -> (&str, &str) {
        match input.rfind('/') {
            Some(i) => (&input[..=i], &input[i + 1..]),
            None => ("", input),
        }
    }

    /// Re-reads the resolved directory and rebuilds the prefix-matching
    /// (case-sensitive), alphabetically sorted suggestion list.
    fn refresh_suggestions(&mut self) {
        self.suggestions.clear();
        if !matches!(self.actions, PromptAction::Goto { .. }) {
            return;
        }
        let input = self.text_area.lines().join("");
        let (dir_prefix, partial) = Self::split_input(&input);
        // Only suggest once a basename character has been typed; an empty
        // partial would otherwise dump the entire directory listing.
        if partial.is_empty() {
            self.suggestion_index = 0;
            return;
        }
        let dir = self.resolve_path(dir_prefix);
        // Only hit the filesystem when the resolved directory changes; typing
        // within the same directory just re-filters the cached listing.
        if self.cached_dir.as_deref() != Some(dir.as_path()) {
            self.cached_entries.clear();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let entries: Vec<_> = entries.flatten().take(MAX_SUGGESTION_ENTRIES + 1).collect();
                // A partial listing would suggest some names and silently
                // omit others, so a directory past the limit gets none.
                if entries.len() <= MAX_SUGGESTION_ENTRIES {
                    // Completed into the input, so it has to be the name
                    // itself. A name that is not UTF-8 cannot be, so it is
                    // not suggested.
                    let mut all: Vec<(String, bool)> = entries
                        .into_iter()
                        .filter_map(|entry| {
                            let name = entry.file_name().into_string().ok()?;
                            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                            Some((name, is_dir))
                        })
                        .collect();
                    all.sort_by(|a, b| a.0.cmp(&b.0));
                    self.cached_entries = all;
                }
            }
            self.cached_dir = Some(dir);
        }
        self.suggestions = self
            .cached_entries
            .iter()
            .filter(|(name, _)| name.starts_with(partial))
            .cloned()
            .collect();
        if self.suggestion_index >= self.suggestions.len() {
            self.suggestion_index = 0;
        }
    }

    /// The current suggestion as `(suffix, index, total)`, where `suffix` is
    /// the not-yet-typed remainder (plus a trailing `/` for directories).
    fn current_suggestion(&self) -> Option<(String, usize, usize)> {
        if self.suggestions.is_empty() {
            return None;
        }
        let input = self.text_area.lines().join("");
        let (_, partial) = Self::split_input(&input);
        let (name, is_dir) = &self.suggestions[self.suggestion_index];
        // The suggestions can lag the input; only slice while the typed
        // partial is still a prefix of the suggestion.
        let mut suffix = name.strip_prefix(partial)?.to_string();
        if *is_dir {
            suffix.push('/');
        }
        Some((suffix, self.suggestion_index, self.suggestions.len()))
    }

    /// Replace the typed basename with the selected suggestion and move the
    /// cursor to the end, so typing can continue into an accepted directory.
    fn accept_suggestion(&mut self) {
        // The suggestion overlay only renders while the cursor is at the end
        // of the input (see `render`), so acceptance must mirror that guard:
        // a suggestion that is not displayed must never be applied.
        if !self.cursor_at_end() {
            return;
        }
        let Some((name, is_dir)) = self.suggestions.get(self.suggestion_index).cloned() else {
            return;
        };
        let input = self.text_area.lines().join("");
        let (dir_prefix, _) = Self::split_input(&input);
        let mut new_text = format!("{dir_prefix}{name}");
        if is_dir {
            new_text.push('/');
        }
        self.reset_text(&new_text);
        self.suggestion_index = 0;
        self.refresh_suggestions();
    }

    /// Cycle the active suggestion by `delta` (wrapping).
    fn cycle_suggestion(&mut self, delta: isize) {
        // The suggestion overlay only renders while the cursor is at the end
        // of the input (see `render`), so cycling must mirror that guard: the
        // index must not move while the overlay is hidden.
        if !self.cursor_at_end() {
            return;
        }
        let count = self.suggestions.len();
        if count == 0 {
            return;
        }
        // rem_euclid rather than `%`, so stepping back from the first
        // suggestion wraps to the last instead of going negative.
        let count = isize::try_from(count).unwrap_or(isize::MAX);
        let index = isize::try_from(self.suggestion_index).unwrap_or(0);
        self.suggestion_index = usize::try_from((index + delta).rem_euclid(count)).unwrap_or(0);
    }

    /// Whether the text cursor is at the end of the input line.
    fn cursor_at_end(&self) -> bool {
        let cursor = self.text_area.cursor();
        let (row, col) = (cursor.0, cursor.1);
        let len = self
            .text_area
            .lines()
            .get(row)
            .map_or(0, |line| line.chars().count());
        col >= len
    }
}

/// Replicates tui-textarea's `next_scroll_top` to keep our scroll offset in sync.
fn next_scroll_top(prev_top: u16, cursor: u16, len: u16) -> u16 {
    if cursor < prev_top {
        cursor
    } else if prev_top + len <= cursor {
        cursor + 1 - len
    } else {
        prev_top
    }
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use std::path::Path;

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    use super::*;
    use crate::{
        app::config::{Config, keybindings::Action},
        command::{Command, ConflictChoice, PromptAction, handler::CommandHandler},
        file_system::path_info::PathInfo,
        test_support::TempDir,
    };

    fn test_path() -> PathInfo {
        PathInfo::try_from("/tmp").unwrap()
    }

    fn prompt_with_action(kind: PromptAction) -> PromptView {
        Config::init_test();
        let mut view = PromptView::default();
        view.handle_command(&Command::OpenPrompt(kind));
        view
    }

    // ── conflict prompt ──────────────────────────────────────────────────────

    fn conflict_chord(can_overwrite: bool, key: char, modifiers: KeyModifiers) -> Option<Command> {
        let mut view = prompt_with_action(PromptAction::Conflict {
            name: "a.txt".to_string(),
            can_overwrite,
        });
        Command::try_from(view.handle_key(KeyCode::Char(key), modifiers)).ok()
    }

    fn conflict_key(can_overwrite: bool, key: char) -> Option<Command> {
        conflict_chord(can_overwrite, key, KeyModifiers::NONE)
    }

    fn conflict_result(can_overwrite: bool, key: char) -> CommandResult {
        let mut view = prompt_with_action(PromptAction::Conflict {
            name: "a.txt".to_string(),
            can_overwrite,
        });
        view.handle_key(KeyCode::Char(key), KeyModifiers::NONE)
    }

    #[test_case('s' => Some(Command::ResolveConflict(ConflictChoice::Skip))         ; "s skips")]
    #[test_case('S' => Some(Command::ResolveConflict(ConflictChoice::SkipAll))      ; "S skips all")]
    #[test_case('o' => Some(Command::ResolveConflict(ConflictChoice::Overwrite))    ; "o overwrites")]
    #[test_case('O' => Some(Command::ResolveConflict(ConflictChoice::OverwriteAll)) ; "O overwrites all")]
    #[test_case('q' => Some(Command::CancelPrompt) ; "an unoffered key abandons the paste")]
    fn a_conflict_resolves_on_one_keypress(key: char) -> Option<Command> {
        conflict_key(true, key)
    }

    #[test_case('o' ; "overwrite")]
    #[test_case('O' ; "overwrite all")]
    fn a_directory_collision_ignores_the_overwrite_keys(key: char) {
        // The prompt does not offer replacing a directory, so its keys must not
        // quietly do it. Ignoring them rather than treating them as the abandon
        // key keeps the prompt up: someone answering `o` through a batch would
        // otherwise lose the rest of the paste at the first directory.
        assert!(matches!(
            conflict_result(false, key),
            CommandResult::Handled
        ));
    }

    /// Every modifier crossterm can report except Shift, which is how the
    /// uppercase choices are typed. Enumerated so that a modifier missing from
    /// the rule is a failure here rather than a way to trigger a destructive
    /// choice by accident.
    #[test_case(KeyModifiers::CONTROL ; "ctrl")]
    #[test_case(KeyModifiers::ALT     ; "alt")]
    #[test_case(KeyModifiers::SUPER   ; "super key")]
    #[test_case(KeyModifiers::HYPER   ; "hyper")]
    #[test_case(KeyModifiers::META    ; "meta")]
    #[test_case(KeyModifiers::CONTROL.union(KeyModifiers::SHIFT) ; "ctrl and shift")]
    fn a_chord_is_not_one_of_the_offered_choices(modifiers: KeyModifiers) {
        // Ctrl+O is a different key from o, and o is destructive, so sharing a
        // letter must not be enough to trigger it. Falling through to cancel
        // loses nothing: the clipboard is restored.
        for key in ['s', 'S', 'o', 'O'] {
            assert_eq!(
                Some(Command::CancelPrompt),
                conflict_chord(true, key, modifiers),
                "{key} with {modifiers:?} should not resolve a conflict"
            );
        }
    }

    #[test_case('S' => Some(Command::ResolveConflict(ConflictChoice::SkipAll))      ; "shift skips all")]
    #[test_case('O' => Some(Command::ResolveConflict(ConflictChoice::OverwriteAll)) ; "shift overwrites all")]
    fn shift_still_reaches_the_uppercase_choices(key: char) -> Option<Command> {
        // Shift is how the uppercase choices are typed at all, so the chord
        // guard above must not reject it.
        conflict_chord(true, key, KeyModifiers::SHIFT)
    }

    /// The name arrives as the table shows it, escapes included, so the
    /// prompt must not escape its backslashes a second time.
    #[test_case(true => " \"a\\u{202e}b\" exists: [s]kip, [S]kip all, [o]verwrite, [O]verwrite all " ; "a file")]
    #[test_case(false => " \"a\\u{202e}b\" exists as a directory: [s]kip, [S]kip all " ; "a directory")]
    fn a_conflict_names_the_entry_as_the_table_shows_it(can_overwrite: bool) -> String {
        let view = prompt_with_action(PromptAction::Conflict {
            name: "a\\u{202e}b".to_string(),
            can_overwrite,
        });
        view.label()
    }

    #[test]
    fn a_conflict_prompt_renders_as_a_confirmation() {
        // No text is collected, so it takes the full-width label path rather
        // than reserving an input area next to the label.
        assert!(
            PromptAction::Conflict {
                name: "a.txt".to_string(),
                can_overwrite: true,
            }
            .is_confirmation()
        );
        assert!(!PromptAction::CreateDirectory.is_confirmation());
    }

    // ── delete prompt ────────────────────────────────────────────────────────

    #[test_case(KeyCode::Char('y'), KeyModifiers::NONE => Command::ConfirmDelete ; "y confirms")]
    #[test_case(KeyCode::Char('Y'), KeyModifiers::SHIFT => Command::ConfirmDelete ; "uppercase Y confirms")]
    #[test_case(KeyCode::Char('y'), KeyModifiers::CONTROL => Command::CancelPrompt ; "ctrl y cancels")]
    #[test_case(KeyCode::Char('y'), KeyModifiers::ALT => Command::CancelPrompt ; "alt y cancels")]
    #[test_case(KeyCode::Char('Y'), KeyModifiers::CONTROL | KeyModifiers::SHIFT => Command::CancelPrompt ; "ctrl shift y cancels")]
    #[test_case(KeyCode::Char('n'), KeyModifiers::NONE => Command::CancelPrompt ; "n cancels")]
    #[test_case(KeyCode::Enter, KeyModifiers::NONE => Command::CancelPrompt ; "enter cancels")]
    #[test_case(KeyCode::Esc, KeyModifiers::NONE => Command::CancelPrompt ; "esc cancels")]
    #[test_case(KeyCode::Char('q'), KeyModifiers::NONE => Command::CancelPrompt ; "any other key cancels")]
    fn a_delete_prompt_answers_on_one_keypress(code: KeyCode, modifiers: KeyModifiers) -> Command {
        let mut view = prompt_with_action(PromptAction::Delete(1));
        Command::try_from(view.handle_key(code, modifiers)).unwrap()
    }

    // ── paste of an entry from elsewhere ─────────────────────────────────────

    fn foreign_paste(names: &[&str]) -> (crate::test_support::TempDir, ClipboardEntry, PathInfo) {
        let dir = crate::test_support::TempDir::new("prompt_confirm_paste");
        let paths = names
            .iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, b"x").unwrap();
                PathInfo::try_from(path.as_path()).unwrap()
            })
            .collect();
        let dest = PathInfo::try_from(dir.path()).unwrap();
        (dir, ClipboardEntry::Move(paths), dest)
    }

    #[test_case(KeyCode::Char('y'), KeyModifiers::NONE, true ; "y pastes")]
    #[test_case(KeyCode::Char('Y'), KeyModifiers::SHIFT, true ; "uppercase Y pastes")]
    #[test_case(KeyCode::Char('y'), KeyModifiers::CONTROL, false ; "ctrl y cancels")]
    #[test_case(KeyCode::Char('n'), KeyModifiers::NONE, false ; "n cancels")]
    #[test_case(KeyCode::Enter, KeyModifiers::NONE, false ; "enter cancels")]
    fn a_paste_from_elsewhere_runs_only_when_confirmed(
        code: KeyCode,
        modifiers: KeyModifiers,
        pastes: bool,
    ) {
        let (_dir, entry, dest) = foreign_paste(&["a"]);
        let mut view = prompt_with_action(PromptAction::ConfirmPaste {
            entry: entry.clone(),
            dest: dest.clone(),
        });
        let expected = if pastes {
            entry.into_paste(dest)
        } else {
            Command::CancelPrompt
        };
        assert_eq!(
            expected,
            Command::try_from(view.handle_key(code, modifiers)).unwrap()
        );
    }

    #[test]
    fn a_paste_from_elsewhere_names_what_it_would_move() {
        let (dir, entry, dest) = foreign_paste(&["a"]);
        let view = prompt_with_action(PromptAction::ConfirmPaste { entry, dest });
        let expected = format!(
            " Clipboard from elsewhere: move {} here? (y/n) ",
            quoted(&dir.join("a"))
        );
        assert_eq!(expected, view.label());

        drop(dir);
    }

    /// Another program chose every path after the first, so each is named.
    #[test]
    fn a_paste_from_elsewhere_lists_its_paths_and_counts_the_rest() {
        let names = ["a", "b", "c", "d", "e", "f", "g"];
        let (dir, entry, dest) = foreign_paste(&names);
        let view = prompt_with_action(PromptAction::ConfirmPaste { entry, dest });

        let label = view.label();

        let mut expected = vec![" Clipboard from elsewhere: move 7 items here? (y/n) ".to_string()];
        for name in &names[..MAX_LISTED_PASTE_PATHS] {
            expected.push(format!("   {}", quoted(&dir.join(name))));
        }
        expected.push("   and 2 more".to_string());
        assert_eq!(expected, label.lines().collect::<Vec<_>>());
    }

    /// Deep enough that `compact` would elide the middle, which is where the
    /// directory that tells two locations apart would be.
    #[test_case(1 ; "one path")]
    #[test_case(2 ; "a list of paths")]
    fn a_paste_from_elsewhere_names_every_directory_in_the_path(count: usize) {
        let (dir, _, dest) = foreign_paste(&[]);
        let parent = dir.join("one/two/three/four");
        std::fs::create_dir_all(&parent).unwrap();
        std::fs::write(parent.join("a"), b"x").unwrap();
        let src = PathInfo::try_from(parent.join("a").as_path()).unwrap();
        let view = prompt_with_action(PromptAction::ConfirmPaste {
            entry: ClipboardEntry::Copy(vec![src; count]),
            dest,
        });

        let label = view.label();

        let shown = quoted(&parent.join("a")).to_string();
        assert!(shown.ends_with("/one/two/three/four/a\""), "{shown}");
        assert_eq!(count, label.matches(&shown).count(), "{label}");
    }

    // ── copy and cut ─────────────────────────────────────────────────────────

    /// A paste is text, so its line break does not submit the prompt and the
    /// letters after it are not read as keys.
    #[test]
    fn a_paste_is_inserted_as_one_line_of_text() {
        let mut view = prompt_with_action(PromptAction::Filter(String::new()));

        let result = view.handle_paste("important\r\n\u{1b}dy");

        assert_eq!(CommandResult::Handled, result);
        assert_eq!("importantdy", view.text_area.lines()[0]);
    }

    #[test]
    fn a_paste_into_a_confirmation_is_not_an_answer() {
        let mut view = prompt_with_action(PromptAction::Delete(1));

        assert_eq!(CommandResult::Handled, view.handle_paste("y"));
        assert_eq!("", view.text_area.lines()[0]);
    }

    /// "hello world" with "world" selected.
    fn prompt_with_selection() -> PromptView {
        let mut view = prompt_with_action(PromptAction::Filter("hello world".into()));
        view.text_area.move_cursor(CursorMove::Jump(0, 6));
        view.text_area.start_selection();
        view.text_area.move_cursor(CursorMove::End);
        view
    }

    // A key that ratatui-textarea does not treat as copy or cut itself, so only
    // the binding can make it do either. Ctrl+k deletes to the end of the line
    // when it reaches the textarea.
    #[test_case(KeyCode::Char('c'), KeyModifiers::ALT ; "alt c")]
    #[test_case(KeyCode::Char('k'), KeyModifiers::CONTROL ; "ctrl k")]
    fn a_rebound_copy_sets_the_clipboard_to_the_selection(code: KeyCode, modifiers: KeyModifiers) {
        let mut view = prompt_with_selection();
        // A stale yank, which a copy that did not happen would send instead.
        view.text_area.set_yank_text("stale");
        let result = view.handle_text_key(Some(Action::PromptCopy), code, modifiers);
        assert_eq!(
            CommandResult::from(Command::SetClipboardText("world".to_string())),
            result
        );
        assert_eq!("hello world", view.text_area.lines()[0]);
    }

    #[test_case(KeyCode::Char('x'), KeyModifiers::ALT ; "alt x")]
    #[test_case(KeyCode::Char('k'), KeyModifiers::CONTROL ; "ctrl k")]
    fn a_rebound_cut_removes_the_selection_into_the_clipboard(
        code: KeyCode,
        modifiers: KeyModifiers,
    ) {
        let mut view = prompt_with_selection();
        view.text_area.set_yank_text("stale");
        let result = view.handle_text_key(Some(Action::PromptCut), code, modifiers);
        assert_eq!(
            CommandResult::from(Command::SetClipboardText("world".to_string())),
            result
        );
        assert_eq!("hello ", view.text_area.lines()[0]);
    }

    #[test_case(Action::PromptCopy ; "copy")]
    #[test_case(Action::PromptCut ; "cut")]
    fn copy_or_cut_without_a_selection_leaves_the_clipboard_alone(action: Action) {
        let mut view = prompt_with_action(PromptAction::Filter("hello".into()));
        view.text_area.set_yank_text("stale");
        let result = view.handle_text_key(Some(action), KeyCode::Char('c'), KeyModifiers::ALT);
        assert_eq!(CommandResult::Handled, result);
        assert_eq!("hello", view.text_area.lines()[0]);
    }

    // A selection that was started and then moved back to its anchor, as with
    // Shift+Left then Shift+Right at the end of the input, is still selecting
    // but empty.
    #[test_case(Action::PromptCopy ; "copy")]
    #[test_case(Action::PromptCut ; "cut")]
    fn copy_or_cut_of_an_empty_selection_leaves_the_clipboard_alone(action: Action) {
        let mut view = prompt_with_action(PromptAction::Filter("hello".into()));
        view.text_area.set_yank_text("stale");
        view.text_area.start_selection();
        view.text_area.move_cursor(CursorMove::Back);
        view.text_area.move_cursor(CursorMove::Forward);
        assert!(view.text_area.is_selecting());
        let result = view.handle_text_key(Some(action), KeyCode::Char('c'), KeyModifiers::ALT);
        assert_eq!(CommandResult::Handled, result);
        assert_eq!("hello", view.text_area.lines()[0]);
    }

    // ── next_scroll_top ──────────────────────────────────────────────────────

    #[test_case(0, 5, 10 => 0; "cursor within viewport stays")]
    #[test_case(0, 0, 10 => 0; "cursor at start stays")]
    #[test_case(5, 3, 10 => 3; "cursor before viewport scrolls back")]
    #[test_case(0, 10, 5 => 6; "cursor past viewport scrolls forward")]
    #[test_case(0, 5,  5 => 1; "cursor at exact right boundary scrolls forward")]
    #[test_case(3, 3,  5 => 3; "cursor at left edge of viewport stays")]
    fn next_scroll_top_keeps_the_cursor_in_view(prev_top: u16, cursor: u16, len: u16) -> u16 {
        next_scroll_top(prev_top, cursor, len)
    }

    // ── display_col_to_char_idx ──────────────────────────────────────────────

    #[test_case("hello", 0 => 0; "ascii: col 0 maps to char 0")]
    #[test_case("hello", 3 => 3; "ascii: col 3 maps to char 3")]
    #[test_case("hello", 5 => 5; "ascii: col past end clamps to len")]
    #[test_case("hello", 9 => 5; "ascii: col far past end clamps to len")]
    #[test_case("日本語",  0 => 0; "wide: col 0 maps to char 0")]
    #[test_case("日本語",  1 => 0; "wide: col within first char clamps to 0")]
    #[test_case("日本語",  2 => 1; "wide: col at second char boundary")]
    #[test_case("日本語",  4 => 2; "wide: col at third char boundary")]
    #[test_case("ab日",   2 => 2; "mixed: col at wide char boundary")]
    #[test_case("ab日",   3 => 2; "mixed: col within wide char clamps")]
    #[test_case("ab日",   4 => 3; "mixed: col past wide char maps to char 3")]
    fn display_col_to_char_idx_counts_display_width(text: &str, col: u16) -> u16 {
        let view = prompt_with_action(PromptAction::Filter(text.to_string()));
        view.display_col_to_char_idx(col)
    }

    // ── handle_key: Esc / Enter dispatch ─────────────────────────────────────

    #[test]
    fn esc_cancels_the_prompt() {
        Config::init_test();
        let mut view = PromptView::default();
        let result = view.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(result, Command::CancelPrompt.into());
    }

    #[test]
    fn enter_with_filter_returns_set_filter() {
        let mut view = prompt_with_action(PromptAction::Filter("foo".into()));
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(result, Command::FilterChanged("foo".to_string()).into());
    }

    #[test]
    fn submitting_a_search_starts_it_unless_the_query_is_empty() {
        let mut view = prompt_with_action(PromptAction::Search("txt".into()));
        assert_eq!(
            CommandResult::from(Command::StartSearch("txt".to_string())),
            view.handle_key(KeyCode::Enter, KeyModifiers::NONE)
        );

        // An empty query matches every entry, so submitting one is a way of
        // changing your mind: it closes the prompt instead of walking the tree.
        let mut view = prompt_with_action(PromptAction::Search(String::new()));
        assert_eq!(
            CommandResult::from(Command::CancelPrompt),
            view.handle_key(KeyCode::Enter, KeyModifiers::NONE)
        );
    }

    #[test]
    fn a_shorter_suggestion_list_resets_the_index_rather_than_leaving_it_past_the_end() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap"); // ["Apple", "Apricot"]
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(1, view.suggestion_index);

        // Typing on narrows the list to one entry, so the index the user had
        // moved to no longer addresses a row.
        type_str(&mut view, "r"); // ["Apricot"]
        assert_eq!(1, view.suggestions.len());
        assert_eq!(0, view.suggestion_index);
        assert!(view.current_suggestion().is_some());
    }

    #[test]
    fn enter_with_rename_returns_rename_path() {
        let path = test_path();
        let mut view = prompt_with_action(PromptAction::Rename {
            path: path.clone(),
            name: "bar.txt".into(),
        });
        type_str(&mut view, "x");
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            result,
            Command::Rename {
                path,
                name: "bar.txtx".to_string()
            }
            .into()
        );
    }

    /// The prefill of a name that is not UTF-8 is its lossy spelling, so
    /// renaming to it would name a different file.
    #[test]
    fn submitting_a_rename_unchanged_cancels_it() {
        let mut view = prompt_with_action(PromptAction::Rename {
            path: test_path(),
            name: "caf\u{fffd}.txt".to_string(),
        });
        // An edit undone before submitting still leaves the name unchanged.
        type_str(&mut view, "x");
        view.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(
            CommandResult::from(Command::CancelPrompt),
            view.handle_key(KeyCode::Enter, KeyModifiers::NONE)
        );
    }

    #[test]
    fn open_loads_initial_text_and_positions_cursor_at_end() {
        let view = prompt_with_action(PromptAction::Filter("hello".into()));
        assert_eq!(view.text_area.lines()[0], "hello");
        assert_eq!(view.text_area.cursor(), (0, 5));
    }

    #[test]
    fn ctrl_z_resets_to_initial_text() {
        let mut view = prompt_with_action(PromptAction::Rename {
            path: test_path(),
            name: "original.txt".into(),
        });
        view.handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_ne!(view.text_area.lines()[0], "original.txt");

        view.handle_key(KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(view.text_area.lines()[0], "original.txt");
        // The cursor returns to the end of the restored text, not to where the
        // edit left it.
        assert_eq!(view.text_area.cursor(), (0, 12));
    }

    #[test]
    fn a_click_moves_the_cursor_to_the_clicked_column() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

        let mut view = prompt_with_action(PromptAction::Filter("hello".into()));
        view.render_area = Rect::new(10, 0, 20, 1);

        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(view.text_area.cursor(), (0, 2));
    }

    #[test]
    fn update_scroll_col_tracks_cursor_past_viewport() {
        // 11 ASCII chars, cursor at end (col 11); viewport width = 5
        // next_scroll_top(0, 11, 5) = 11 + 1 - 5 = 7
        let mut view = prompt_with_action(PromptAction::Filter("hello world".into()));
        view.update_scroll_col(5);
        assert_eq!(view.scroll_col, 7);
    }

    #[test]
    fn update_scroll_col_stays_zero_when_text_fits() {
        let mut view = prompt_with_action(PromptAction::Filter("hi".into()));
        view.update_scroll_col(20);
        assert_eq!(view.scroll_col, 0);
    }

    // ── Goto type-ahead ──────────────────────────────────────────────────────

    /// A temp directory populated with entries that exercise prefix matching
    /// and sort order. Names differ by more than case, so the fixture builds on
    /// a case-insensitive filesystem as well; the test that needs a case-only
    /// pair adds it itself.
    struct GotoFixture {
        dir: TempDir,
    }

    impl GotoFixture {
        fn new() -> Self {
            let dir = TempDir::new("goto");
            std::fs::create_dir_all(dir.join("Apple")).unwrap();
            std::fs::create_dir_all(dir.join("Apricot")).unwrap();
            std::fs::write(dir.join("Banana"), b"").unwrap();
            Self { dir }
        }
    }

    fn goto_prompt(directory: &Path) -> PromptView {
        prompt_with_action(PromptAction::Goto {
            directory: directory.to_path_buf(),
        })
    }

    fn type_str(view: &mut PromptView, text: &str) {
        for c in text.chars() {
            view.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
    }

    // Linux only: proving that matching is case-sensitive needs "Apple" and
    // "apple" to exist side by side, which a case-insensitive filesystem cannot
    // represent. The matching itself carries no platform-specific code.
    #[cfg(target_os = "linux")]
    #[test]
    fn goto_suggestions_are_prefix_matched_sorted_and_case_sensitive() {
        let fixture = GotoFixture::new();
        std::fs::write(fixture.dir.join("apple"), b"").unwrap();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        let names: Vec<&str> = view.suggestions.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Apple", "Apricot"]); // "apple" excluded (case-sensitive)
    }

    #[test]
    fn nothing_is_suggested_before_a_name_is_typed() {
        let fixture = GotoFixture::new();
        let view = goto_prompt(fixture.dir.path());
        assert!(view.suggestions.is_empty());
        assert_eq!(None, view.current_suggestion());
    }

    /// Both leave the input empty, where nothing is suggested, so a list left
    /// over from "Ap" shows they skipped the refresh.
    #[test_case(Action::PromptCut ; "cutting all of it")]
    #[test_case(Action::PromptReset ; "resetting to the empty initial text")]
    fn an_edit_outside_the_textarea_refreshes_the_suggestions(action: Action) {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        view.text_area.select_all();

        view.handle_text_key(Some(action), KeyCode::Char('x'), KeyModifiers::ALT);

        assert_eq!("", view.text_area.lines()[0]);
        assert!(view.suggestions.is_empty());
    }

    #[test]
    fn reopening_the_prompt_rereads_the_directory() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ch");
        assert!(view.suggestions.is_empty());

        std::fs::write(fixture.dir.join("Cherry"), b"").unwrap();
        view.handle_command(&Command::OpenPrompt(PromptAction::Goto {
            directory: fixture.dir.path().to_path_buf(),
        }));
        type_str(&mut view, "Ch");

        assert_eq!(Some(("erry".to_string(), 0, 1)), view.current_suggestion());
    }

    /// Filled to the limit and then one past it, reopening in between so the
    /// directory is read again.
    #[test]
    fn a_directory_past_the_entry_limit_gets_no_suggestions() {
        let dir = TempDir::new("goto_limit");
        for index in 0..MAX_SUGGESTION_ENTRIES {
            std::fs::write(dir.join(format!("f{index}")), b"").unwrap();
        }
        let mut view = goto_prompt(dir.path());
        type_str(&mut view, "f0");
        assert_eq!(Some((String::new(), 0, 1)), view.current_suggestion());

        std::fs::write(dir.join("g"), b"").unwrap();
        let mut view = goto_prompt(dir.path());
        type_str(&mut view, "f0");
        assert_eq!(None, view.current_suggestion());
    }

    /// The overlay spells a disguising name out, but the input has to hold
    /// the name itself for the path to resolve.
    #[test]
    fn accepting_a_disguising_name_inserts_the_name_itself() {
        let dir = TempDir::new("goto_disguising");
        std::fs::write(dir.join("a\u{202e}txt"), b"").unwrap();
        let mut view = goto_prompt(dir.path());
        type_str(&mut view, "a");

        view.handle_key(KeyCode::Tab, KeyModifiers::NONE);

        assert_eq!("a\u{202e}txt", view.text_area.lines()[0]);
    }

    // Linux only, here and below: a name that is not UTF-8 needs a filesystem
    // that stores arbitrary bytes, which APFS does not.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_not_suggested() {
        use std::os::unix::ffi::OsStrExt;
        let dir = TempDir::new("goto_not_utf8");
        std::fs::write(dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9")), b"").unwrap();
        std::fs::write(dir.join("cafe"), b"").unwrap();
        let mut view = goto_prompt(dir.path());
        type_str(&mut view, "caf");

        let names: Vec<&str> = view.suggestions.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(vec!["cafe"], names);
    }

    /// The lossy spelling of the base directory names a sibling that exists,
    /// so resolving against it would open the wrong `sub`.
    #[cfg(target_os = "linux")]
    #[test]
    fn goto_from_a_directory_that_is_not_utf8_resolves_against_it() {
        use std::os::unix::ffi::OsStrExt;
        let root = TempDir::new("goto_base_not_utf8");
        let base = root.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("caf\u{fffd}").join("sub")).unwrap();
        let mut view = goto_prompt(&base);
        view.handle_command(&Command::ClipboardText("sub".into()));

        let Ok(Command::Open(info)) = Command::try_from(view.submit()) else {
            panic!("expected an Open");
        };
        assert_eq!(base.join("sub"), info.path);
    }

    #[test]
    fn tab_accepts_directory_and_appends_slash() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        view.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(view.text_area.lines()[0], "Apple/");
        assert!(view.cursor_at_end());
    }

    #[test]
    fn down_and_up_cycle_suggestion_index_with_wrap() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap"); // ["Apple", "Apricot"]
        assert_eq!(view.suggestion_index, 0);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 1);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 0); // wrapped
        view.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 1); // wrapped backwards
    }

    #[test]
    fn cycling_with_cursor_mid_line_does_not_move_the_hidden_suggestion_index() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap"); // ["Apple", "Apricot"]
        view.text_area.move_cursor(CursorMove::Back);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 0);
        view.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 0);
    }

    #[test]
    fn enter_accepts_the_suggestion_then_opens_it() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        // "Apr" names nothing on disk, so only accepting the "Apricot"
        // suggestion before submitting can open a directory.
        type_str(&mut view, "Apr");
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let Command::Open(info) = Command::try_from(result).unwrap() else {
            panic!("expected Command::Open");
        };
        assert_eq!(
            info.path.to_string_lossy().trim_end_matches('/'),
            fixture.dir.join("Apricot").to_string_lossy()
        );
    }

    #[test]
    fn tab_with_cursor_mid_line_does_not_apply_hidden_suggestion() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        view.text_area.move_cursor(CursorMove::Back);
        view.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(view.text_area.lines()[0], "Ap");
    }

    #[test]
    fn enter_with_cursor_mid_line_submits_typed_text_not_hidden_suggestion() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        view.text_area.move_cursor(CursorMove::Back);
        // "Ap" does not exist, so submitting the typed text (rather than the
        // hidden "Apple/" suggestion) must warn instead of opening a path.
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            Command::try_from(result).unwrap(),
            Command::AlertWarn(_)
        ));
    }

    #[test]
    fn goto_submit_missing_path_returns_alert_warn() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Nope");
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let Ok(Command::AlertWarn(message)) = Command::try_from(result) else {
            panic!("expected Command::AlertWarn");
        };
        assert!(message.starts_with("Path does not exist:"), "{message}");
    }

    #[test]
    fn current_suggestion_is_none_when_suggestions_are_stale() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        // The untyped rest of the first match, marked as a directory.
        assert_eq!(Some(("ple/".to_string(), 0, 2)), view.current_suggestion());
        // Mutate the text without refreshing, so the typed partial is no
        // longer a prefix of any cached suggestion.
        view.text_area.insert_str("XYZXYZXYZ");
        assert_eq!(view.current_suggestion(), None);
    }

    #[test]
    fn clipboard_paste_refreshes_goto_suggestions() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        assert_eq!(view.suggestions.len(), 2);

        view.handle_command(&Command::ClipboardText("XYZ".into()));

        assert_eq!(view.text_area.lines()[0], "ApXYZ");
        assert!(view.suggestions.is_empty());
        assert_eq!(view.current_suggestion(), None);
    }

    // ── split_input ──────────────────────────────────────────────────────────

    #[test_case("",          "",      ""    ; "empty input")]
    #[test_case("file",      "",      "file"; "no separator: all partial")]
    #[test_case("dir/",      "dir/",  ""    ; "trailing slash: empty partial")]
    #[test_case("dir/file",  "dir/",  "file"; "relative dir prefix")]
    #[test_case("/abs/path", "/abs/", "path"; "absolute dir prefix")]
    #[test_case("a/b/c",     "a/b/",  "c"   ; "splits at the last separator")]
    #[test_case("/",         "/",     ""    ; "root only")]
    fn split_input_splits_at_the_last_separator(
        input: &str,
        expected_prefix: &str,
        expected_partial: &str,
    ) {
        assert_eq!(
            PromptView::split_input(input),
            (expected_prefix, expected_partial)
        );
    }

    // ── resolve_path ─────────────────────────────────────────────────────────

    #[test]
    fn resolve_path_joins_relative_input_onto_basedir() {
        let view = goto_prompt(Path::new("/tmp/base"));
        assert_eq!(
            view.resolve_path("sub/file"),
            PathBuf::from("/tmp/base/sub/file")
        );
    }

    #[test]
    fn resolve_path_uses_absolute_input_as_is() {
        let view = goto_prompt(Path::new("/tmp/base"));
        assert_eq!(view.resolve_path("/etc/hosts"), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn resolve_path_expands_tilde_to_home() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        let view = goto_prompt(Path::new("/tmp/base"));
        assert_eq!(view.resolve_path("~"), home);
        assert_eq!(view.resolve_path("~/Documents"), home.join("Documents"));
    }
}
