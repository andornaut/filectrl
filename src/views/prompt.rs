mod handler;
mod view;
mod widgets;

use rat_widget::{text::TextPosition, textarea::TextAreaState};
use ratatui::layout::Rect;

use super::View;
use crate::{
    clipboard::Clipboard,
    command::{mode::InputMode, result::CommandResult, Command, PromptKind},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct PromptView {
    directory: Option<PathInfo>,
    filter: String,
    input_area: Rect,
    input_state: TextAreaState,
    kind: PromptKind,
    selected: Option<PathInfo>,
}

impl PromptView {
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

        let mut text_area_state = TextAreaState::new();
        text_area_state.set_clipboard(Some(Clipboard::default().as_rat_clipboard()));
        text_area_state.set_text(&text);

        // Move to 1 position after the last character
        let text_width_plus_1 = text_area_state.line_width(0) + 1;
        let position_after_last_char = TextPosition::new(text_width_plus_1 - 1, 0);
        text_area_state.set_cursor(position_after_last_char, false);

        // Workaround https://github.com/thscharler/rat-salsa/issues/5
        // `text_area_state.area` is not set yet, because `TextArea` hasn't been rendered yet, so
        // we need to keep track of the `self.input_area` that would have applied, and then set the
        // hscroll offset manually.
        let hscroll_offset = text_width_plus_1.saturating_sub(self.input_area.width as u32);
        text_area_state.hscroll.set_offset(hscroll_offset as usize);

        self.input_state = text_area_state;
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
        let value = self.input_state.text().to_string();
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => match &self.selected {
                Some(selected_path) => Command::RenamePath(selected_path.clone(), value).into(),
                None => CommandResult::Handled,
            },
        }
    }
}
