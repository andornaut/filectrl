mod handler;
mod view;
mod word_navigation;

use rat_text::{TextPosition, TextRange};
use rat_widget::textarea::{self, TextAreaState};
use ratatui::crossterm::event::Event;

use super::View;
use crate::{
    app::clipboard::Clipboard,
    command::{Command, PromptKind, mode::InputMode, result::CommandResult},
};

pub(super) struct PromptView {
    clipboard: Clipboard,
    kind: PromptKind,
    text_area_state: TextAreaState,
}

impl PromptView {
    pub(super) fn new(clipboard: Clipboard) -> Self {
        Self {
            clipboard,
            kind: PromptKind::default(),
            text_area_state: TextAreaState::default(),
        }
    }

    fn label(&self) -> &'static str {
        match self.kind {
            PromptKind::Filter => " Filter ",
            PromptKind::Rename => " Rename ",
        }
    }

    fn move_by_word<F>(&mut self, find_boundary: F, select: bool) -> CommandResult
    where
        F: Fn(&str, usize) -> usize,
    {
        let text = self.text_area_state.text();
        let current_pos = self.text_area_state.cursor();
        let current_byte_offset = self
            .text_area_state
            .try_bytes_at_range(TextRange::new((0, 0), current_pos))
            .map(|r| r.end)
            .unwrap_or(0);

        let new_byte_offset = find_boundary(&text, current_byte_offset);
        let new_pos = self
            .text_area_state
            .try_byte_pos(new_byte_offset)
            .unwrap_or(current_pos);

        self.text_area_state.set_cursor(new_pos, select);
        CommandResult::Handled
    }

    fn navigate_by_word<F>(&mut self, find_boundary: F) -> CommandResult
    where
        F: Fn(&str, usize) -> usize,
    {
        self.move_by_word(find_boundary, false)
    }

    fn select_by_word<F>(&mut self, find_boundary: F) -> CommandResult
    where
        F: Fn(&str, usize) -> usize,
    {
        self.move_by_word(find_boundary, true)
    }

    fn open(&mut self, kind: &PromptKind, initial_text: &str) -> CommandResult {
        self.kind = *kind;

        let mut text_area_state = TextAreaState::new();
        text_area_state.set_clipboard(Some(self.clipboard.to_rat_clipboard()));
        text_area_state.focus.set(true);
        text_area_state.set_text(initial_text);
        text_area_state.move_to_line_end(false);
        self.text_area_state = text_area_state;
        CommandResult::Handled
    }

    fn should_show(&self, mode: &InputMode) -> bool {
        *mode == InputMode::Prompt
    }

    fn workaround_navigate_right_when_at_edge(&mut self, event: &Event) -> CommandResult {
        let text_area_state = &mut self.text_area_state;
        let cursor_x_before = text_area_state.cursor().x;
        textarea::handle_events(text_area_state, true, event);

        // Workaround https://github.com/thscharler/rat-salsa/issues/6
        // rat-widget wraps the cursor to x=0 when pressing Right at the right edge of the
        // viewport. Detect this by checking if the cursor moved backwards and restore it.
        let cursor_x_after = text_area_state.cursor().x;
        if cursor_x_after < cursor_x_before {
            // set_cursor clamps to end of text, so cursor_x_before + 1 is always safe
            text_area_state.set_cursor(TextPosition::new(cursor_x_before + 1, 0), false);
        }
        CommandResult::Handled
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.text_area_state.text().to_string();
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => Command::RenameSelected(value).into(),
        }
    }
}
