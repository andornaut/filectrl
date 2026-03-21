mod handler;
mod view;

use ratatui::layout::Rect;
use ratatui_textarea::{CursorMove, TextArea};
use unicode_width::UnicodeWidthChar;

use super::{View, unicode::pluralize_items};
use crate::command::{Command, PromptAction, result::CommandResult};

#[derive(Default)]
pub(super) struct PromptView {
    actions: PromptAction,
    text_area: TextArea<'static>,
    initial_text: String,
    render_area: Rect,
    /// Horizontal scroll offset (in display columns), mirroring tui-textarea's internal viewport.
    scroll_col: u16,
}

impl PromptView {
    fn label(&self) -> String {
        match &self.actions {
            PromptAction::Delete(count) => {
                format!(" Delete {}? (y/n) ", pluralize_items(*count))
            }
            PromptAction::Filter(_) => " Filter ".to_string(),
            PromptAction::Rename(_, _) => " Rename ".to_string(),
        }
    }

    fn open(&mut self, kind: &PromptAction) -> CommandResult {
        let text = match kind {
            PromptAction::Delete(_) => String::new(),
            PromptAction::Filter(text) | PromptAction::Rename(_, text) => text.clone(),
        };
        self.actions = kind.clone();
        self.initial_text = text.clone();
        self.reset_text(&text);
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
            PromptAction::Delete(_) => Command::ConfirmDelete.into(),
            PromptAction::Filter(_) => Command::SetFilter(value).into(),
            PromptAction::Rename(path, _) => Command::Rename(path.clone(), value).into(),
        }
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
        assert_eq!(result, Command::SetFilter("foo".to_string()).into());
    }

    #[test]
    fn enter_with_rename_returns_rename_path() {
        let path = test_path();
        let mut view = prompt_with_action(PromptAction::Rename(path.clone(), "bar.txt".into()));
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        assert_eq!(
            result,
            Command::Rename(path, "bar.txt".to_string()).into()
        );
    }

    // ── open (via handle_command) ─────────────────────────────────────────────

    #[test]
    fn open_loads_initial_text_and_positions_cursor_at_end() {
        let view = prompt_with_action(PromptAction::Filter("hello".into()));
        assert_eq!(view.text_area.lines()[0], "hello");
        assert_eq!(view.text_area.cursor(), (0, 5));
    }

    #[test]
    fn open_resets_scroll_col() {
        let mut view = prompt_with_action(PromptAction::Filter("hello".into()));
        view.scroll_col = 99;
        view.handle_command(&Command::OpenPrompt(PromptAction::Filter("new".into())));
        assert_eq!(view.scroll_col, 0);
    }

    // ── Ctrl+Z resets to initial text ──────────────────────────────────────────

    #[test]
    fn ctrl_z_resets_to_initial_text() {
        let mut view = prompt_with_action(PromptAction::Rename(test_path(), "original.txt".into()));
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
}
