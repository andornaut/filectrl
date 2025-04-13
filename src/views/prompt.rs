mod handler;
mod view;
mod widgets;

use log::debug;
use rat_widget::text::TextPosition;
use rat_widget::textarea::TextAreaState;
use ratatui::layout::{Position, Rect};

use super::View;
use crate::{
    command::{mode::InputMode, result::CommandResult, Command, PromptKind},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct PromptView {
    cursor_position: Position,
    directory: Option<PathInfo>,
    filter: String,
    input_area: Rect,
    input_state: TextAreaState,
    kind: PromptKind,
    selected: Option<PathInfo>,
}

impl PromptView {
    pub(super) fn cursor_position(&self, mode: &InputMode) -> Option<(u16, u16)> {
        if self.should_show(mode) {
            Some((self.cursor_position.x, self.cursor_position.y))
        } else {
            None
        }
    }

    fn height(&self, mode: &InputMode) -> u16 {
        if self.should_show(mode) {
            1
        } else {
            0
        }
    }

    fn label(&self) -> String {
        match self.kind {
            PromptKind::Filter => " Filter ".into(),
            PromptKind::Rename => " Rename ".into(),
        }
    }

    fn open(&mut self, kind: &PromptKind) -> CommandResult {
        self.kind = kind.clone();

        let text = match &self.kind {
            PromptKind::Filter => self.filter.clone(),
            PromptKind::Rename => self
                .selected
                .as_ref()
                .map_or(String::new(), |s| s.basename.clone()),
        };

        debug!(
            "Opening prompt with kind={:?}, text=\"{}\", text_len={}",
            kind,
            text,
            text.len()
        );
        self.input_state = TextAreaState::new();
        self.input_state.set_text(&text);
        let last_position_x = TextPosition::new(text.len() as u32 - 1, 0);
        self.input_state.set_cursor(last_position_x, false);
        debug!("Setting initial cursor to {:?}", last_position_x);

        CommandResult::none()
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        self.filter = filter;
        CommandResult::none()
    }

    fn set_selected(&mut self, selected: Option<PathInfo>) -> CommandResult {
        self.selected = selected;
        CommandResult::none()
    }

    fn should_show(&self, mode: &InputMode) -> bool {
        *mode == InputMode::Prompt
    }

    fn submit(&mut self) -> CommandResult {
        let value = self.input_state.text().to_string();
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => match &self.selected {
                Some(selected_path) => Command::RenamePath(selected_path.clone(), value).into(),
                None => CommandResult::none(),
            },
        }
    }
}
