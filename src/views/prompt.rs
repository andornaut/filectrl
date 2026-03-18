mod handler;
mod view;
mod word_navigation;

use rat_text::TextRange;
use rat_widget::textarea::{self, TextAreaState};
use ratatui::crossterm::event::Event;

use super::View;
use crate::{
    app::clipboard::Clipboard,
    command::{Command, PromptKind, mode::InputMode, result::CommandResult},
    file_system::path_info::PathInfo,
};

pub(super) struct PromptView {
    clipboard: Clipboard,
    directory: Option<PathInfo>,
    filter: String,
    kind: PromptKind,
    selected: Option<PathInfo>,
    text_area_state: TextAreaState,
}

impl PromptView {
    pub(super) fn new(clipboard: Clipboard) -> Self {
        Self {
            clipboard,
            directory: None,
            filter: String::new(),
            kind: PromptKind::default(),
            selected: None,
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

    fn open(&mut self, kind: &PromptKind) -> CommandResult {
        self.kind = *kind;

        let text = match kind {
            PromptKind::Filter => self.filter.clone(),
            PromptKind::Rename => self
                .selected
                .as_ref()
                .map_or(String::new(), |s| s.basename.clone()),
        };

        let mut text_area_state = TextAreaState::new();
        text_area_state.set_clipboard(Some(self.clipboard.to_rat_clipboard()));
        text_area_state.focus.set(true);
        text_area_state.set_text(&text);
        text_area_state.move_to_line_end(false);
        self.text_area_state = text_area_state;
        CommandResult::Handled
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
        CommandResult::Handled
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::Handled
    }

    fn should_show(&self, mode: &InputMode) -> bool {
        *mode == InputMode::Prompt
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.text_area_state.text().to_string();
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => match &self.selected {
                Some(selected_path) => Command::RenamePath(selected_path.clone(), value).into(),
                None => CommandResult::Handled,
            },
        }
    }

    fn workaround_navigate_right_when_at_edge(&mut self, event: &Event) -> CommandResult {
        let text_area_state = &mut self.text_area_state;
        textarea::handle_events(text_area_state, true, event);
        CommandResult::Handled
    }
}
