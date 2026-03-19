mod handler;
mod view;

use std::rc::Rc;

use ratatui_textarea::{CursorMove, TextArea};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthChar;

use super::View;
use crate::{
    app::clipboard::Clipboard,
    command::{Command, PromptKind, mode::InputMode, result::CommandResult},
    keybindings::KeyBindings,
};

pub(super) struct PromptView {
    clipboard: Clipboard,
    initial_text: String,
    keybindings: Rc<KeyBindings>,
    kind: PromptKind,
    text_area: TextArea<'static>,
    render_area: Rect,
    /// Horizontal scroll offset (in display columns), mirroring tui-textarea's internal viewport.
    scroll_col: u16,
}

impl PromptView {
    pub(super) fn new(clipboard: Clipboard, keybindings: Rc<KeyBindings>) -> Self {
        Self {
            clipboard,
            initial_text: String::new(),
            keybindings,
            kind: PromptKind::default(),
            text_area: TextArea::default(),
            render_area: Rect::default(),
            scroll_col: 0,
        }
    }

    fn label(&self) -> &'static str {
        match self.kind {
            PromptKind::Filter => " Filter ",
            PromptKind::Rename => " Rename ",
        }
    }

    fn open(&mut self, kind: &PromptKind, initial_text: &str) -> CommandResult {
        self.kind = *kind;
        self.initial_text = initial_text.to_string();
        self.reset_text(&self.initial_text.clone());
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

    fn should_show(&self, mode: &InputMode) -> bool {
        *mode == InputMode::Prompt
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.text_area.lines().join("");
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => Command::RenameSelected(value).into(),
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
        app::clipboard::Clipboard,
        command::{Command, PromptKind, handler::CommandHandler},
        keybindings::KeyBindings,
    };

    fn prompt_with_text(kind: PromptKind, text: &str) -> PromptView {
        let kb = Rc::new(KeyBindings::new(None).unwrap());
        let mut view = PromptView::new(Clipboard::default(), kb);
        view.handle_command(&Command::OpenPrompt(kind, text.to_string()));
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
        let view = prompt_with_text(PromptKind::Filter, text);
        view.display_col_to_char_idx(col)
    }

    // ── handle_key: Esc / Enter dispatch ─────────────────────────────────────

    #[test]
    fn esc_returns_close_prompt() {
        let kb = Rc::new(KeyBindings::new(None).unwrap());
        let mut view = PromptView::new(Clipboard::default(), kb);
        let result = view.handle_key(&KeyCode::Esc, &KeyModifiers::NONE);
        assert_eq!(result, Command::ClosePrompt.into());
    }

    #[test]
    fn enter_with_filter_returns_set_filter() {
        let mut view = prompt_with_text(PromptKind::Filter, "foo");
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        assert_eq!(result, Command::SetFilter("foo".to_string()).into());
    }

    #[test]
    fn enter_with_rename_returns_rename_selected() {
        let mut view = prompt_with_text(PromptKind::Rename, "bar.txt");
        let result = view.handle_key(&KeyCode::Enter, &KeyModifiers::NONE);
        assert_eq!(result, Command::RenameSelected("bar.txt".to_string()).into());
    }

    // ── open (via handle_command) ─────────────────────────────────────────────

    #[test]
    fn open_loads_initial_text_and_positions_cursor_at_end() {
        let view = prompt_with_text(PromptKind::Filter, "hello");
        assert_eq!(view.text_area.lines()[0], "hello");
        assert_eq!(view.text_area.cursor(), (0, 5));
    }

    #[test]
    fn open_resets_scroll_col() {
        let mut view = prompt_with_text(PromptKind::Filter, "hello");
        view.scroll_col = 99;
        view.handle_command(&Command::OpenPrompt(PromptKind::Filter, "new".to_string()));
        assert_eq!(view.scroll_col, 0);
    }

    // ── Ctrl+Z resets to initial text ──────────────────────────────────────────

    #[test]
    fn ctrl_z_resets_to_initial_text() {
        let mut view = prompt_with_text(PromptKind::Rename, "original.txt");
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
        let mut view = prompt_with_text(PromptKind::Filter, "hello world");
        view.update_scroll_col(5);
        assert_eq!(view.scroll_col, 7);
    }

    #[test]
    fn update_scroll_col_stays_zero_when_text_fits() {
        let mut view = prompt_with_text(PromptKind::Filter, "hi");
        view.update_scroll_col(20);
        assert_eq!(view.scroll_col, 0);
    }
}
