mod handler;
mod view;
mod widget;

use std::path::{Path, PathBuf};

use ratatui::buffer::CellWidth;
use ratatui::layout::Rect;
use ratatui_textarea::{CursorMove, TextArea};

use super::{View, as_dimension, scroll_to_show, unicode::pluralize_items};
use crate::{
    app::clipboard::ClipboardEntry,
    command::{Command, PromptAction, result::CommandResult},
    file_system::{
        chmod_mode,
        path_info::{PathInfo, compact, quoted},
    },
};

/// Paths listed by a paste-from-elsewhere confirmation before it counts the rest.
const MAX_LISTED_PASTE_PATHS: usize = 5;

/// Directory size above which Goto offers no suggestions (the read runs on the UI thread).
const MAX_SUGGESTION_ENTRIES: usize = 10_000;

#[derive(Default)]
pub(super) struct PromptView {
    actions: PromptAction,
    text_area: TextArea<'static>,
    initial_text: String,
    /// Filter: the text last sent to the table, so an unchanged edit sends nothing.
    live_filter: String,
    render_area: Rect,
    /// Horizontal scroll offset in display columns, mirroring tui-textarea's viewport.
    scroll_col: u16,
    basedir: PathBuf,
    suggestions: Vec<(String, bool)>,
    suggestion_index: usize,
    /// Goto: the directory `cached_entries` was read from.
    cached_dir: Option<PathBuf>,
    cached_entries: Vec<(String, bool)>,
    /// Delete: the display name of the single entry being deleted.
    delete_subject: Option<String>,
}

/// One line of a prompt's label. The path is kept separate so the widget can trim it to fit.
#[derive(Default)]
struct LabelLine {
    before: String,
    /// Escaped, without its quotes (those are in `before` and `after`).
    path: String,
    after: String,
}

impl LabelLine {
    fn plain(text: String) -> Self {
        Self {
            before: text,
            ..Self::default()
        }
    }

    fn quoting(before: &str, path: &str, after: &str) -> Self {
        Self {
            before: format!("{before}\""),
            path: path.to_string(),
            after: format!("\"{after}"),
        }
    }
}

impl std::fmt::Display for LabelLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{}{}", self.before, self.path, self.after)
    }
}

/// `path` escaped as `quoted` renders it, without the quotes.
fn quoted_inner(path: &Path) -> String {
    let quoted = quoted(path).to_string();
    quoted[1..quoted.len() - 1].to_string()
}

impl PromptView {
    pub(super) fn set_delete_subject(&mut self, name: Option<String>) {
        self.delete_subject = name;
    }

    /// Whether the open prompt is a y/n or conflict question rather than a text input.
    pub(super) fn is_confirmation(&self) -> bool {
        matches!(
            self.actions,
            PromptAction::Conflict { .. }
                | PromptAction::ConfirmPaste { .. }
                | PromptAction::ConfirmQuit(_)
                | PromptAction::Delete(_)
        )
    }

    fn label(&self) -> Vec<LabelLine> {
        let plain = |text: String| vec![LabelLine::plain(text)];
        match &self.actions {
            PromptAction::Chmod { paths, .. } => {
                plain(format!(" Chmod {} (octal) ", pluralize_items(paths.len())))
            }
            PromptAction::AddBookmark { .. } => plain(" Add bookmark ".to_string()),
            PromptAction::CreateDirectory => plain(" New directory ".to_string()),
            PromptAction::Delete(1) if let Some(name) = &self.delete_subject => {
                vec![LabelLine::quoting(" Delete ", name, "? (y/n) ")]
            }
            PromptAction::Delete(count) => {
                plain(format!(" Delete {}? (y/n) ", pluralize_items(*count)))
            }
            PromptAction::ConfirmQuit(1) => {
                plain(" 1 task is running. Quit anyway? (y/n) ".to_string())
            }
            PromptAction::ConfirmQuit(tasks) => {
                plain(format!(" {tasks} tasks are running. Quit anyway? (y/n) "))
            }
            PromptAction::Filter(_) => plain(" Filter ".to_string()),
            PromptAction::Goto { .. } => plain(" Go to ".to_string()),
            PromptAction::Rename { .. } => plain(" Rename ".to_string()),
            PromptAction::Search(_) => plain(" Search ".to_string()),
            PromptAction::ConfirmPaste { entry, .. } => {
                let verb = match entry {
                    ClipboardEntry::Copy(_) => "copy",
                    ClipboardEntry::Move(_) => "move",
                };
                // Whole paths, trimmed from the left only as the width requires: another program
                // chose them.
                let paths = entry.paths();
                if let [path] = paths {
                    return vec![LabelLine::quoting(
                        &format!(" Clipboard from elsewhere: {verb} "),
                        &quoted_inner(&path.path),
                        " here? (y/n) ",
                    )];
                }
                let mut lines = plain(format!(
                    " Clipboard from elsewhere: {verb} {} here? (y/n) ",
                    pluralize_items(paths.len())
                ));
                lines.extend(
                    paths
                        .iter()
                        .take(MAX_LISTED_PASTE_PATHS)
                        .map(|path| LabelLine::quoting("   ", &quoted_inner(&path.path), "")),
                );
                if paths.len() > MAX_LISTED_PASTE_PATHS {
                    let more = paths.len() - MAX_LISTED_PASTE_PATHS;
                    lines.push(LabelLine::plain(format!("   and {more} more")));
                }
                lines
            }
            // `name` is already escaped.
            PromptAction::Conflict {
                name,
                can_overwrite: true,
            } => vec![LabelLine::quoting(
                " ",
                name,
                " exists: [s]kip, [S]kip all, [o]verwrite, [O]verwrite all ",
            )],
            PromptAction::Conflict {
                name,
                can_overwrite: false,
            } => vec![LabelLine::quoting(
                " ",
                name,
                " exists and cannot be replaced: [s]kip, [S]kip all ",
            )],
        }
    }

    fn open(&mut self, kind: &PromptAction) -> CommandResult {
        let text = match kind {
            PromptAction::Chmod { mode, .. } => mode.clone(),
            PromptAction::Conflict { .. }
            | PromptAction::ConfirmPaste { .. }
            | PromptAction::ConfirmQuit(_)
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
        self.live_filter.clone_from(&text);
        self.reset_text(&text);
        if let PromptAction::Goto { directory } = kind {
            self.basedir.clone_from(directory);
            self.suggestion_index = 0;
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

    /// Mirrors tui-textarea's `scroll_top_col` logic, since `viewport` is private. Call after each
    /// `render_widget`.
    fn update_scroll_col(&mut self, width: u16) {
        if width == 0 {
            return;
        }
        let cursor_display_col = self.text_area.screen_cursor().col;
        self.scroll_col = as_dimension(scroll_to_show(
            width.into(),
            self.scroll_col.into(),
            cursor_display_col,
        ));
    }

    /// Converts a display column (viewport-relative plus scroll) to a character index for
    /// `CursorMove::Jump`.
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
            // An invalid mode keeps the prompt open so it can be corrected.
            PromptAction::Chmod { paths, .. } => match chmod_mode(paths, &value) {
                Ok(_) => Command::Chmod {
                    paths: paths.clone(),
                    mode: value,
                },
                Err(error) => error.into(),
            },
            PromptAction::AddBookmark { directory, .. } => Command::AddBookmark {
                directory: directory.clone(),
                name: value,
            },
            PromptAction::CreateDirectory => Command::CreateDirectory(value),
            // Confirmations resolve in `handle_key`, so submit never reaches them.
            PromptAction::Conflict { .. }
            | PromptAction::ConfirmPaste { .. }
            | PromptAction::ConfirmQuit(_) => Command::CancelPrompt,
            PromptAction::Delete(_) => Command::ConfirmDelete,
            PromptAction::Filter(_) => Command::FilterChanged(value),
            PromptAction::Goto { .. } => {
                let path = self.resolve_path(&value);
                match PathInfo::try_from(&path) {
                    Ok(info) if info.is_symlink_broken() => Command::AlertWarn(format!(
                        "Cannot go to {}: its target does not exist",
                        compact(&path)
                    )),
                    Ok(info) => Command::Open(info),
                    Err(error) => {
                        Command::AlertWarn(format!("Failed to access {}: {error}", compact(&path)))
                    }
                }
            }
            // For a non-UTF-8 name the offered text is a lossy spelling that names a different
            // file.
            PromptAction::Rename { .. } if value == self.initial_text => Command::CancelPrompt,
            PromptAction::Rename { path, .. } => Command::Rename {
                path: path.clone(),
                name: value,
            },
            PromptAction::Search(_) if value.is_empty() => Command::CancelPrompt,
            PromptAction::Search(_) => Command::StartSearch(value),
        }
        .into()
    }

    /// `~` alone or a leading `~/` expands to home; other relative input (`~backup` included) joins
    /// onto `basedir`.
    fn resolve_path(&self, input: &str) -> PathBuf {
        let home_relative = if input == "~" {
            Some("")
        } else {
            input.strip_prefix("~/")
        };
        if let Some(rest) = home_relative
            && let Some(base) = directories::BaseDirs::new()
        {
            let home = base.home_dir();
            return if rest.is_empty() {
                home.to_path_buf()
            } else {
                home.join(rest)
            };
        }
        self.basedir.join(input)
    }

    /// Splits input at the last `/` into `(dir_prefix, partial)`; `dir_prefix` keeps the `/`.
    fn split_input(input: &str) -> (&str, &str) {
        match input.rfind('/') {
            Some(i) => (&input[..=i], &input[i + 1..]),
            None => ("", input),
        }
    }

    /// Rebuilds the case-sensitive prefix-matching suggestions, sorted.
    fn refresh_suggestions(&mut self) {
        self.suggestions.clear();
        if !matches!(self.actions, PromptAction::Goto { .. }) {
            return;
        }
        let input = self.text_area.lines().join("");
        let (dir_prefix, partial) = Self::split_input(&input);
        if partial.is_empty() {
            self.suggestion_index = 0;
            return;
        }
        let dir = self.resolve_path(dir_prefix);
        if self.cached_dir.as_deref() != Some(dir.as_path()) {
            self.cached_entries.clear();
            if let Ok(entries) = std::fs::read_dir(&dir) {
                let entries: Vec<_> = entries.flatten().take(MAX_SUGGESTION_ENTRIES + 1).collect();
                // A directory past the limit gets no suggestions rather than a partial list.
                if entries.len() <= MAX_SUGGESTION_ENTRIES {
                    // A non-UTF-8 name cannot be typed into the input, so it is not suggested.
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

    /// The current suggestion as `(suffix, index, total)`; `suffix` is the untyped remainder,
    /// `/`-terminated for directories.
    fn current_suggestion(&self) -> Option<(String, usize, usize)> {
        if self.suggestions.is_empty() {
            return None;
        }
        let input = self.text_area.lines().join("");
        let (_, partial) = Self::split_input(&input);
        let (name, is_dir) = &self.suggestions[self.suggestion_index];
        // The suggestions can lag the input.
        let mut suffix = name.strip_prefix(partial)?.to_string();
        if *is_dir {
            suffix.push('/');
        }
        Some((suffix, self.suggestion_index, self.suggestions.len()))
    }

    fn accept_suggestion(&mut self) {
        // The overlay renders only while the cursor is at the end, so only then does a suggestion
        // apply.
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

    fn cycle_suggestion(&mut self, delta: isize) {
        // The overlay renders only while the cursor is at the end.
        if !self.cursor_at_end() {
            return;
        }
        let count = self.suggestions.len();
        if count == 0 {
            return;
        }
        let count = isize::try_from(count).unwrap_or(isize::MAX);
        let index = isize::try_from(self.suggestion_index).unwrap_or(0);
        self.suggestion_index = usize::try_from((index + delta).rem_euclid(count)).unwrap_or(0);
    }

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

#[cfg(test)]
#[allow(clippy::disallowed_methods)]
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    use super::*;
    use crate::{
        app::config::{Config, keybindings::Action},
        command::{Command, ConflictChoice, handler::CommandHandler},
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

    /// The label as drawn at unlimited width, one line per row.
    fn label_text(view: &PromptView) -> String {
        view.label()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test_case(1, Some("a.txt") => " Delete \"a.txt\"? (y/n) " ; "one named entry")]
    #[test_case(1, None => " Delete 1 item? (y/n) " ; "one entry with no name given")]
    #[test_case(2, None => " Delete 2 items? (y/n) " ; "several entries")]
    fn the_delete_prompt_asks(count: usize, name: Option<&str>) -> String {
        let mut view = prompt_with_action(PromptAction::Delete(count));
        view.set_delete_subject(name.map(str::to_string));
        label_text(&view)
    }

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
        // Ignored rather than cancelling, so answering `o` through a batch keeps the rest of the
        // paste.
        assert!(matches!(
            conflict_result(false, key),
            CommandResult::Handled
        ));
    }

    /// Every modifier crossterm reports except Shift, which types the uppercase choices.
    #[test_case(KeyModifiers::CONTROL ; "ctrl")]
    #[test_case(KeyModifiers::ALT     ; "alt")]
    #[test_case(KeyModifiers::SUPER   ; "super key")]
    #[test_case(KeyModifiers::HYPER   ; "hyper")]
    #[test_case(KeyModifiers::META    ; "meta")]
    #[test_case(KeyModifiers::CONTROL.union(KeyModifiers::SHIFT) ; "ctrl and shift")]
    fn a_chord_is_not_one_of_the_offered_choices(modifiers: KeyModifiers) {
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
        conflict_chord(true, key, KeyModifiers::SHIFT)
    }

    #[test_case(true => " \"a\\u{202e}b\" exists: [s]kip, [S]kip all, [o]verwrite, [O]verwrite all " ; "a file")]
    #[test_case(false => " \"a\\u{202e}b\" exists and cannot be replaced: [s]kip, [S]kip all " ; "a directory")]
    fn a_conflict_names_the_entry_as_the_table_shows_it(can_overwrite: bool) -> String {
        let view = prompt_with_action(PromptAction::Conflict {
            name: "a\\u{202e}b".to_string(),
            can_overwrite,
        });
        label_text(&view)
    }

    #[test]
    fn a_conflict_prompt_renders_as_a_confirmation() {
        assert!(
            PromptAction::Conflict {
                name: "a.txt".to_string(),
                can_overwrite: true,
            }
            .is_confirmation()
        );
        assert!(!PromptAction::CreateDirectory.is_confirmation());
    }

    #[test_case(KeyCode::Char('y'), KeyModifiers::NONE => Command::Quit ; "y quits")]
    #[test_case(KeyCode::Char('Y'), KeyModifiers::SHIFT => Command::Quit ; "uppercase Y quits")]
    #[test_case(KeyCode::Char('n'), KeyModifiers::NONE => Command::CancelPrompt ; "n cancels")]
    #[test_case(KeyCode::Esc, KeyModifiers::NONE => Command::CancelPrompt ; "Esc cancels")]
    #[test_case(KeyCode::Char('q'), KeyModifiers::NONE => Command::CancelPrompt ; "the quit key again cancels")]
    #[test_case(KeyCode::Char('y'), KeyModifiers::CONTROL => Command::CancelPrompt ; "a chord cancels")]
    fn the_quit_prompt_answers(code: KeyCode, modifiers: KeyModifiers) -> Command {
        let mut view = prompt_with_action(PromptAction::ConfirmQuit(2));
        Command::try_from(view.handle_key(code, modifiers)).unwrap()
    }

    #[test_case(1 => " 1 task is running. Quit anyway? (y/n) " ; "one")]
    #[test_case(3 => " 3 tasks are running. Quit anyway? (y/n) " ; "several")]
    fn the_quit_prompt_counts_the_tasks(tasks: usize) -> String {
        label_text(&prompt_with_action(PromptAction::ConfirmQuit(tasks)))
    }

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
        assert_eq!(expected, label_text(&view));

        drop(dir);
    }

    #[test]
    fn a_paste_from_elsewhere_lists_its_paths_and_counts_the_rest() {
        let names = ["a", "b", "c", "d", "e", "f", "g"];
        let (dir, entry, dest) = foreign_paste(&names);
        let view = prompt_with_action(PromptAction::ConfirmPaste { entry, dest });

        let label = label_text(&view);

        let mut expected = vec![" Clipboard from elsewhere: move 7 items here? (y/n) ".to_string()];
        for name in &names[..MAX_LISTED_PASTE_PATHS] {
            expected.push(format!("   {}", quoted(&dir.join(name))));
        }
        expected.push("   and 2 more".to_string());
        assert_eq!(expected, label.lines().collect::<Vec<_>>());
    }

    /// Deep enough that `compact` would elide the middle directory.
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

        let label = label_text(&view);

        let shown = quoted(&parent.join("a")).to_string();
        assert!(shown.ends_with("/one/two/three/four/a\""), "{shown}");
        assert_eq!(count, label.matches(&shown).count(), "{label}");
    }

    /// The rows `view` draws at `width` columns, as text.
    fn rendered(view: &mut PromptView, width: u16) -> Vec<String> {
        use ratatui::{Terminal, backend::TestBackend, layout::Constraint};

        let Constraint::Length(rows) = view.constraint(Rect::new(0, 0, width, 10)) else {
            panic!("expected a fixed height");
        };
        let mut terminal = Terminal::new(TestBackend::new(width, rows)).unwrap();
        terminal
            .draw(|frame| view.render(Config::global().theme(), frame.area(), frame))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..rows)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// A file named `name` whose path is wider than 150 columns.
    fn deep_file(dir: &TempDir, name: &str) -> PathInfo {
        let parent = dir.join("d".repeat(70)).join("e".repeat(70));
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join(name);
        std::fs::write(&path, b"x").unwrap();
        assert!(path.as_os_str().len() > 150);
        PathInfo::try_from(path.as_path()).unwrap()
    }

    #[test]
    fn a_long_path_loses_its_start_rather_than_its_name_and_question() {
        let (dir, _, dest) = foreign_paste(&[]);
        let src = deep_file(&dir, "target.txt");
        let mut view = prompt_with_action(PromptAction::ConfirmPaste {
            entry: ClipboardEntry::Move(vec![src]),
            dest,
        });

        let rows = rendered(&mut view, 80);

        assert_eq!(1, rows.len());
        assert!(
            rows[0].starts_with(" Clipboard from elsewhere: move \"…"),
            "{rows:?}"
        );
        assert!(
            rows[0].ends_with("eeee/target.txt\" here? (y/n) "),
            "{rows:?}"
        );
    }

    #[test]
    fn a_list_of_long_paths_keeps_one_row_per_path() {
        let (dir, _, dest) = foreign_paste(&[]);
        let paths = vec![deep_file(&dir, "first.txt"), deep_file(&dir, "second.txt")];
        let mut view = prompt_with_action(PromptAction::ConfirmPaste {
            entry: ClipboardEntry::Copy(paths),
            dest,
        });

        let rows = rendered(&mut view, 80);

        assert_eq!(3, rows.len(), "{rows:?}");
        assert!(rows[0].contains("copy 2 items here? (y/n)"), "{rows:?}");
        assert!(rows[1].starts_with("   \"…"), "{rows:?}");
        assert!(rows[1].trim_end().ends_with("/first.txt\""), "{rows:?}");
        assert!(rows[2].trim_end().ends_with("/second.txt\""), "{rows:?}");
    }

    #[test]
    fn a_conflict_over_a_long_name_keeps_its_choices_visible() {
        let mut view = prompt_with_action(PromptAction::Conflict {
            name: format!("{}.txt", "n".repeat(150)),
            can_overwrite: true,
        });

        let rows = rendered(&mut view, 80);

        assert_eq!(1, rows.len());
        assert!(
            rows[0]
                .ends_with("nnnn.txt\" exists: [s]kip, [S]kip all, [o]verwrite, [O]verwrite all "),
            "{rows:?}"
        );
    }

    #[test]
    fn a_path_that_fits_is_shown_whole() {
        let (dir, entry, dest) = foreign_paste(&["a"]);
        let mut view = prompt_with_action(PromptAction::ConfirmPaste { entry, dest });
        let width = label_text(&view).cell_width();

        let rows = rendered(&mut view, width);

        assert_eq!(label_text(&view).trim_end(), rows[0].trim_end());
        drop(dir);
    }

    #[test]
    fn a_paste_is_inserted_as_one_line_of_text() {
        let mut view = prompt_with_action(PromptAction::Filter(String::new()));

        let result = view.handle_paste("important\r\n\u{1b}dy");

        assert_eq!(
            CommandResult::from(Command::FilterEdited("importantdy".to_string())),
            result
        );
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

    // Not a textarea copy or cut key, so only the binding can make it do either.
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

    // Selecting but empty: Shift+Left then Shift+Right at the end of the input.
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

    // `input()` would insert these, but only the first line is drawn.
    #[test_case(KeyCode::Tab, KeyModifiers::NONE ; "tab")]
    #[test_case(KeyCode::BackTab, KeyModifiers::SHIFT ; "backtab")]
    #[test_case(KeyCode::Enter, KeyModifiers::SHIFT ; "shift enter")]
    #[test_case(KeyCode::Enter, KeyModifiers::ALT ; "alt enter")]
    #[test_case(KeyCode::Enter, KeyModifiers::CONTROL ; "ctrl enter")]
    #[test_case(KeyCode::Char('m'), KeyModifiers::CONTROL ; "ctrl m")]
    #[test_case(KeyCode::Char('\n'), KeyModifiers::NONE ; "a literal line feed")]
    #[test_case(KeyCode::Char('\r'), KeyModifiers::NONE ; "a literal carriage return")]
    fn a_key_that_would_insert_whitespace_leaves_the_input_alone(
        code: KeyCode,
        modifiers: KeyModifiers,
    ) {
        for kind in [
            PromptAction::Filter("ab".into()),
            PromptAction::CreateDirectory,
            PromptAction::Rename {
                path: test_path(),
                name: "ab".into(),
            },
        ] {
            let mut view = prompt_with_action(kind);
            view.reset_text("ab");
            view.text_area.move_cursor(CursorMove::Back);
            let result = view.handle_key(code, modifiers);
            assert_eq!(
                CommandResult::Handled,
                result,
                "{code:?} with {modifiers:?}"
            );
            assert_eq!(
                vec!["ab"],
                view.text_area.lines(),
                "{code:?} with {modifiers:?}"
            );
        }
    }

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
    fn each_edit_of_the_filter_is_sent_as_typed() {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));

        assert_eq!(
            CommandResult::from(Command::FilterEdited("foo".to_string())),
            view.handle_key(KeyCode::Char('o'), KeyModifiers::NONE)
        );
        assert_eq!(
            CommandResult::from(Command::FilterEdited("fo".to_string())),
            view.handle_key(KeyCode::Backspace, KeyModifiers::NONE)
        );
        assert_eq!(
            CommandResult::Handled,
            view.handle_key(KeyCode::Left, KeyModifiers::NONE)
        );
        assert_eq!(
            CommandResult::from(Command::FilterEdited("fxo".to_string())),
            view.handle_paste("x")
        );
        view.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert_eq!(
            CommandResult::from(vec![
                Command::SetClipboardText("fxo".to_string()),
                Command::FilterEdited(String::new()),
            ]),
            view.handle_key(KeyCode::Char('x'), KeyModifiers::CONTROL)
        );
    }

    #[test]
    fn clipboard_text_pasted_into_the_filter_is_sent_as_typed() {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));

        assert_eq!(
            CommandResult::from(Command::FilterEdited("fox".to_string())),
            view.handle_command(&Command::ClipboardText("x".to_string()))
        );
    }

    /// A close from beneath (e.g. opening a file) restores the filter like Esc; one after a submit,
    /// new listing, or reset does not.
    #[test_case(None => vec![Command::FilterEdited("fo".to_string())] ; "a close alone")]
    #[test_case(Some(Command::FilterChanged("foo".to_string())) => Vec::<Command>::new() ; "after a submit")]
    #[test_case(Some(Command::ResetView) => Vec::<Command>::new() ; "after a reset")]
    fn a_filter_prompt_closed_without_esc_puts_back_the_opening_filter(
        before: Option<Command>,
    ) -> Vec<Command> {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));
        view.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);
        if let Some(command) = before {
            view.handle_command(&command);
        }

        view.handle_command(&Command::CancelPrompt).into_commands()
    }

    #[test]
    fn a_filter_prompt_closed_by_a_new_listing_puts_nothing_back() {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));
        view.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);

        view.handle_command(&Command::NavigatedDirectory {
            directory: PathInfo::try_from("/tmp").unwrap(),
            generation: 1,
        });

        assert_eq!(
            CommandResult::NotHandled,
            view.handle_command(&Command::CancelPrompt)
        );
    }

    #[test]
    fn esc_puts_the_filter_back_once() {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));
        view.handle_key(KeyCode::Char('o'), KeyModifiers::NONE);

        view.handle_key(KeyCode::Esc, KeyModifiers::NONE);

        assert_eq!(
            CommandResult::NotHandled,
            view.handle_command(&Command::CancelPrompt)
        );
    }

    #[test]
    fn a_text_prompt_other_than_the_filter_sends_nothing_as_typed() {
        let mut view = prompt_with_action(PromptAction::Search("fo".into()));

        assert_eq!(
            CommandResult::Handled,
            view.handle_key(KeyCode::Char('o'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn cancelling_the_filter_puts_back_the_one_it_opened_with() {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));
        type_str(&mut view, "o");

        assert_eq!(
            CommandResult::HandledWithMany(vec![
                Command::FilterEdited("fo".to_string()),
                Command::CancelPrompt,
            ]),
            view.handle_key(KeyCode::Esc, KeyModifiers::NONE)
        );
    }

    #[test]
    fn cancelling_an_unedited_filter_only_closes_the_prompt() {
        let mut view = prompt_with_action(PromptAction::Filter("fo".into()));
        type_str(&mut view, "o");
        view.handle_key(KeyCode::Backspace, KeyModifiers::NONE);

        assert_eq!(
            CommandResult::from(Command::CancelPrompt),
            view.handle_key(KeyCode::Esc, KeyModifiers::NONE)
        );
    }

    #[test]
    fn submitting_a_search_starts_it_unless_the_query_is_empty() {
        let mut view = prompt_with_action(PromptAction::Search("txt".into()));
        assert_eq!(
            CommandResult::from(Command::StartSearch("txt".to_string())),
            view.handle_key(KeyCode::Enter, KeyModifiers::NONE)
        );

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

    #[test]
    fn submitting_a_rename_unchanged_cancels_it() {
        let mut view = prompt_with_action(PromptAction::Rename {
            path: test_path(),
            name: "caf\u{fffd}.txt".to_string(),
        });
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
        // Cursor at column 11, width 5: scroll = 11 + 1 - 5 = 7.
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

    /// Entries for prefix matching and sort order, distinct beyond case so it builds on
    /// case-insensitive filesystems.
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

    // Linux only: needs "Apple" and "apple" side by side.
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

    /// Both leave the input empty, so a stale "Ap" list shows the refresh was skipped.
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

    #[test]
    fn accepting_a_disguising_name_inserts_the_name_itself() {
        let dir = TempDir::new("goto_disguising");
        std::fs::write(dir.join("a\u{202e}txt"), b"").unwrap();
        let mut view = goto_prompt(dir.path());
        type_str(&mut view, "a");

        view.handle_key(KeyCode::Tab, KeyModifiers::NONE);

        assert_eq!("a\u{202e}txt", view.text_area.lines()[0]);
    }

    // Linux only, here and below: APFS cannot store a non-UTF-8 name.
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

    /// The lossy spelling of the base directory names an existing sibling.
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
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            Command::try_from(result).unwrap(),
            Command::AlertWarn(_)
        ));
    }

    #[test]
    fn a_mode_that_is_not_octal_keeps_the_chmod_prompt_and_its_text() {
        let mut view = prompt_with_action(PromptAction::Chmod {
            paths: vec![test_path()],
            mode: String::new(),
        });
        type_str(&mut view, "79");

        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);

        let Ok(Command::AlertError(message)) = Command::try_from(result) else {
            panic!("expected Command::AlertError");
        };
        assert_eq!(
            "Cannot chmod \"/tmp\": \"79\" is not an octal mode",
            message
        );
        assert_eq!("79", view.text_area.lines().join(""));
    }

    #[test]
    fn an_octal_mode_submits_the_chmod() {
        let mut view = prompt_with_action(PromptAction::Chmod {
            paths: vec![test_path()],
            mode: String::new(),
        });
        type_str(&mut view, "750");

        assert_eq!(
            Command::Chmod {
                paths: vec![test_path()],
                mode: "750".into(),
            },
            Command::try_from(view.handle_key(KeyCode::Enter, KeyModifiers::NONE)).unwrap()
        );
    }

    #[test]
    fn goto_submit_missing_path_names_the_cause() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Nope");
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let Ok(Command::AlertWarn(message)) = Command::try_from(result) else {
            panic!("expected Command::AlertWarn");
        };
        assert!(message.starts_with("Failed to access "), "{message}");
        assert!(
            message.ends_with("No such file or directory (os error 2)"),
            "{message}"
        );
    }

    /// Permission denied on a parent is reported as such, not as a missing path.
    #[test]
    fn goto_submit_under_an_unsearchable_directory_names_the_permission_error() {
        use std::os::unix::fs::PermissionsExt;

        if nix::unistd::geteuid().is_root() {
            eprintln!("skipped: root searches a mode-000 directory");
            return;
        }
        let fixture = GotoFixture::new();
        let locked = fixture.dir.join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("inner"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "locked/inner");
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        let Ok(Command::AlertWarn(message)) = Command::try_from(result) else {
            panic!("expected Command::AlertWarn");
        };
        assert!(
            message.ends_with("Permission denied (os error 13)"),
            "{message}"
        );
    }

    #[test]
    fn goto_submit_broken_symlink_is_refused() {
        let fixture = GotoFixture::new();
        std::os::unix::fs::symlink("missing", fixture.dir.join("broken")).unwrap();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "broken");
        let result = view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let Ok(Command::AlertWarn(message)) = Command::try_from(result) else {
            panic!("expected Command::AlertWarn");
        };
        assert!(message.starts_with("Cannot go to "), "{message}");
        assert!(
            message.ends_with(": its target does not exist"),
            "{message}"
        );
    }

    #[test]
    fn current_suggestion_is_none_when_suggestions_are_stale() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(fixture.dir.path());
        type_str(&mut view, "Ap");
        assert_eq!(Some(("ple/".to_string(), 0, 2)), view.current_suggestion());
        // The typed partial is no longer a prefix of any cached suggestion.
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

    #[test]
    fn resolve_path_treats_a_name_starting_with_a_tilde_as_relative() {
        let view = goto_prompt(Path::new("/tmp/base"));
        assert_eq!(
            view.resolve_path("~backup"),
            PathBuf::from("/tmp/base/~backup")
        );
    }
}
