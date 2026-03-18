mod handler;
mod view;

use tui_textarea::{CursorMove, TextArea};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthChar;

use super::View;
use crate::{
    app::clipboard::Clipboard,
    command::{Command, PromptKind, mode::InputMode, result::CommandResult},
};

pub(super) struct PromptView {
    clipboard: Clipboard,
    kind: PromptKind,
    text_area: TextArea<'static>,
    render_area: Rect,
    /// Horizontal scroll offset (in display columns), mirroring tui-textarea's internal viewport.
    scroll_col: u16,
}

impl PromptView {
    pub(super) fn new(clipboard: Clipboard) -> Self {
        Self {
            clipboard,
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
        let mut text_area = TextArea::from([initial_text]);
        text_area.move_cursor(CursorMove::End);
        text_area.set_cursor_line_style(ratatui::style::Style::default());
        self.text_area = text_area;
        self.scroll_col = 0;
        CommandResult::Handled
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
