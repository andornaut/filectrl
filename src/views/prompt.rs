mod handler;
mod view;

use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use ratatui_textarea::{CursorMove, TextArea};
use unicode_width::UnicodeWidthChar;

use super::{View, unicode::pluralize_items};
use crate::{
    command::{Command, PromptAction, result::CommandResult},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct PromptView {
    actions: PromptAction,
    text_area: TextArea<'static>,
    initial_text: String,
    render_area: Rect,
    /// Horizontal scroll offset (in display columns), mirroring tui-textarea's internal viewport.
    scroll_col: u16,
    /// Goto: the directory that relative input is resolved against.
    basedir: String,
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
        }
    }

    fn open(&mut self, kind: &PromptAction) -> CommandResult {
        let text = match kind {
            PromptAction::Chmod { mode, .. } => mode.clone(),
            PromptAction::CreateDirectory | PromptAction::Delete(_) | PromptAction::Goto { .. } => {
                String::new()
            }
            PromptAction::AddBookmark { name: text, .. }
            | PromptAction::Filter(text)
            | PromptAction::Rename { name: text, .. }
            | PromptAction::Search(text) => text.clone(),
        };
        self.actions = kind.clone();
        self.initial_text = text.clone();
        self.reset_text(&text);
        if let PromptAction::Goto { directory } = kind {
            self.basedir = directory.clone();
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
        let (row, col) = self.text_area.cursor();
        let cursor_display_col: u16 = self.text_area.lines()[row]
            .chars()
            .take(col)
            .map(|c| c.width().unwrap_or(0) as u16)
            .sum();
        self.scroll_col = next_scroll_top(self.scroll_col, cursor_display_col, width);
    }

    /// Converts a display-column offset (viewport-relative + scroll) to a character index
    /// suitable for `CursorMove::Jump`.
    fn display_col_to_char_idx(&self, display_col: u16) -> u16 {
        let line = &self.text_area.lines()[0];
        let mut remaining = display_col;
        let mut idx = 0u16;
        for c in line.chars() {
            let w = c.width().unwrap_or(1) as u16;
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
            PromptAction::Delete(_) => Command::ConfirmDelete,
            PromptAction::Filter(_) => Command::FilterChanged(value),
            PromptAction::Goto { .. } => {
                let path = self.resolve_path(&value);
                if !path.exists() {
                    Command::AlertWarn(format!("Path does not exist: {}", path.display()))
                } else {
                    match PathInfo::try_from(&path) {
                        Ok(info) => Command::Open(info),
                        Err(error) => {
                            Command::AlertWarn(format!("Cannot access {}: {error}", path.display()))
                        }
                    }
                }
            }
            PromptAction::Rename { path, .. } => Command::Rename {
                path: path.clone(),
                name: value,
            },
            PromptAction::Search(_) => Command::StartSearch(value),
        }
        .into()
    }

    /// Resolve user input to a path: leading `~` expands to home, absolute
    /// paths are used as-is, and relative input is joined onto `basedir`.
    fn resolve_path(&self, input: &str) -> PathBuf {
        if let Some(rest) = input.strip_prefix('~') {
            if let Some(base) = directories::BaseDirs::new() {
                let home = base.home_dir();
                let rest = rest.strip_prefix('/').unwrap_or(rest);
                return if rest.is_empty() {
                    home.to_path_buf()
                } else {
                    home.join(rest)
                };
            }
        }
        let path = Path::new(input);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(&self.basedir).join(input)
        }
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
                let mut all: Vec<(String, bool)> = entries
                    .flatten()
                    .map(|entry| {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        (name, is_dir)
                    })
                    .collect();
                all.sort_by(|a, b| a.0.cmp(&b.0));
                self.cached_entries = all;
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
        let mut suffix = name[partial.len()..].to_string();
        if *is_dir {
            suffix.push('/');
        }
        Some((suffix, self.suggestion_index, self.suggestions.len()))
    }

    /// Replace the typed basename with the selected suggestion and move the
    /// cursor to the end, so typing can continue into an accepted directory.
    fn accept_suggestion(&mut self) {
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
        let n = self.suggestions.len() as isize;
        if n == 0 {
            return;
        }
        let i = self.suggestion_index as isize;
        self.suggestion_index = (((i + delta) % n + n) % n) as usize;
    }

    /// Whether the text cursor is at the end of the input line.
    fn cursor_at_end(&self) -> bool {
        let (row, col) = self.text_area.cursor();
        let len = self
            .text_area
            .lines()
            .get(row)
            .map(|line| line.chars().count())
            .unwrap_or(0);
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
mod tests {
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    use super::*;
    use crate::{
        app::config::Config,
        command::{Command, PromptAction, handler::CommandHandler},
        file_system::path_info::PathInfo,
    };

    fn test_path() -> PathInfo {
        PathInfo::try_from("/tmp").unwrap()
    }

    fn ensure_config_initialized() {
        let config = Config::load(None, vec![]).unwrap();
        Config::init(config);
    }

    fn prompt_with_action(kind: PromptAction) -> PromptView {
        ensure_config_initialized();
        let mut view = PromptView::default();
        view.handle_command(&Command::OpenPrompt(kind));
        view
    }

    // ── next_scroll_top ──────────────────────────────────────────────────────

    #[test_case(0, 5, 10 => 0; "cursor within viewport stays")]
    #[test_case(0, 0, 10 => 0; "cursor at start stays")]
    #[test_case(5, 3, 10 => 3; "cursor before viewport scrolls back")]
    #[test_case(0, 10, 5 => 6; "cursor past viewport scrolls forward")]
    #[test_case(0, 5,  5 => 1; "cursor at exact right boundary scrolls forward")]
    #[test_case(3, 3,  5 => 3; "cursor at left edge of viewport stays")]
    fn next_scroll_top_cases(prev_top: u16, cursor: u16, len: u16) -> u16 {
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
    fn display_col_to_char_idx_cases(text: &str, col: u16) -> u16 {
        let view = prompt_with_action(PromptAction::Filter(text.to_string()));
        view.display_col_to_char_idx(col)
    }

    // ── handle_key: Esc / Enter dispatch ─────────────────────────────────────

    #[test]
    fn esc_returns_close_prompt() {
        ensure_config_initialized();
        let mut view = PromptView::default();
        let result = view.handle_key(&KeyCode::Esc, &KeyModifiers::NONE);
        assert_eq!(result, Command::CancelPrompt.into());
    }

    #[test]
    fn enter_with_filter_returns_set_filter() {
        let mut view = prompt_with_action(PromptAction::Filter("foo".into()));
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        assert_eq!(result, Command::FilterChanged("foo".to_string()).into());
    }

    #[test]
    fn enter_with_rename_returns_rename_path() {
        let path = test_path();
        let mut view = prompt_with_action(PromptAction::Rename {
            path: path.clone(),
            name: "bar.txt".into(),
        });
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        assert_eq!(
            result,
            Command::Rename {
                path,
                name: "bar.txt".to_string()
            }
            .into()
        );
    }

    // ── open (via handle_command) ─────────────────────────────────────────────

    #[test]
    fn open_loads_initial_text_and_positions_cursor_at_end() {
        let view = prompt_with_action(PromptAction::Filter("hello".into()));
        assert_eq!(view.text_area.lines()[0], "hello");
        assert_eq!(view.text_area.cursor(), (0, 5));
    }

    // ── Ctrl+Z resets to initial text ──────────────────────────────────────────

    #[test]
    fn ctrl_z_resets_to_initial_text() {
        let mut view = prompt_with_action(PromptAction::Rename {
            path: test_path(),
            name: "original.txt".into(),
        });
        // Type a character to modify the text
        view.handle_key(&KeyCode::Char('x'), &KeyModifiers::NONE);
        assert_ne!(view.text_area.lines()[0], "original.txt");

        // Ctrl+Z resets
        view.handle_key(&KeyCode::Char('z'), &KeyModifiers::CONTROL);
        assert_eq!(view.text_area.lines()[0], "original.txt");
        assert_eq!(view.text_area.cursor(), (0, 12));
    }

    // ── update_scroll_col ────────────────────────────────────────────────────

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

    /// Self-cleaning unique temp directory populated with known entries.
    struct GotoFixture {
        dir: PathBuf,
    }

    impl GotoFixture {
        fn new() -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                std::env::temp_dir().join(format!("filectrl_goto_{}_{nanos}", std::process::id()));
            std::fs::create_dir_all(dir.join("Apple")).unwrap();
            std::fs::create_dir_all(dir.join("Apricot")).unwrap();
            std::fs::write(dir.join("apple"), b"").unwrap();
            std::fs::write(dir.join("Banana"), b"").unwrap();
            Self { dir }
        }
    }

    impl Drop for GotoFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn goto_prompt(directory: &Path) -> PromptView {
        prompt_with_action(PromptAction::Goto {
            directory: directory.to_string_lossy().into_owned(),
        })
    }

    fn type_str(view: &mut PromptView, text: &str) {
        for c in text.chars() {
            view.handle_key(&KeyCode::Char(c), &KeyModifiers::NONE);
        }
    }

    #[test]
    fn goto_suggestions_are_prefix_matched_sorted_and_case_sensitive() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(&fixture.dir);
        type_str(&mut view, "Ap");
        let names: Vec<&str> = view.suggestions.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["Apple", "Apricot"]); // "apple" excluded (case-sensitive)
    }

    #[test]
    fn tab_accepts_directory_and_appends_slash() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(&fixture.dir);
        type_str(&mut view, "Ap");
        view.handle_key(&KeyCode::Tab, &KeyModifiers::NONE);
        assert_eq!(view.text_area.lines()[0], "Apple/");
        assert!(view.cursor_at_end());
    }

    #[test]
    fn down_and_up_cycle_suggestion_index_with_wrap() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(&fixture.dir);
        type_str(&mut view, "Ap"); // ["Apple", "Apricot"]
        assert_eq!(view.suggestion_index, 0);
        view.handle_key(&KeyCode::Down, &KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 1);
        view.handle_key(&KeyCode::Down, &KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 0); // wrapped
        view.handle_key(&KeyCode::Up, &KeyModifiers::NONE);
        assert_eq!(view.suggestion_index, 1); // wrapped backwards
    }

    #[test]
    fn goto_submit_existing_directory_returns_open() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(&fixture.dir);
        type_str(&mut view, "Apple");
        // Enter accepts the directory suggestion (appending `/`, like Tab)
        // and then submits, opening that directory.
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        let Command::Open(info) = Command::try_from(result).unwrap() else {
            panic!("expected Command::Open");
        };
        assert_eq!(
            info.path.to_string_lossy().trim_end_matches('/'),
            fixture.dir.join("Apple").to_string_lossy()
        );
    }

    #[test]
    fn goto_submit_missing_path_returns_alert_warn() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(&fixture.dir);
        type_str(&mut view, "Nope");
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        assert!(matches!(
            Command::try_from(result).unwrap(),
            Command::AlertWarn(_)
        ));
    }

    #[test]
    fn suggestion_is_hidden_when_cursor_not_at_end() {
        let fixture = GotoFixture::new();
        let mut view = goto_prompt(&fixture.dir);
        type_str(&mut view, "Ap");
        assert!(view.cursor_at_end());
        assert!(view.current_suggestion().is_some());
        view.handle_key(&KeyCode::Left, &KeyModifiers::NONE);
        assert!(!view.cursor_at_end()); // overlay is skipped in render() when false
    }

    // ── split_input ──────────────────────────────────────────────────────────

    #[test_case("",          "",      ""    ; "empty input")]
    #[test_case("file",      "",      "file"; "no separator: all partial")]
    #[test_case("dir/",      "dir/",  ""    ; "trailing slash: empty partial")]
    #[test_case("dir/file",  "dir/",  "file"; "relative dir prefix")]
    #[test_case("/abs/path", "/abs/", "path"; "absolute dir prefix")]
    #[test_case("a/b/c",     "a/b/",  "c"   ; "splits at the last separator")]
    #[test_case("/",         "/",     ""    ; "root only")]
    fn split_input_cases(input: &str, expected_prefix: &str, expected_partial: &str) {
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
