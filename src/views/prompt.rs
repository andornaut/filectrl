mod handler;
mod view;
mod word_navigation;

use rat_widget::textarea::TextAreaState;

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
    text_area_state: TextAreaState,
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
        text_area_state.focus.set(true);
        text_area_state.set_text(&text);
        text_area_state.move_to_line_end(false);
        self.text_area_state = text_area_state;
        CommandResult::Handled
    }

    fn set_directory(&mut self, directory: &PathInfo) -> CommandResult {
        let is_different_dir = self
            .directory
            .as_ref()
            .map_or(false, |previous_dir| !previous_dir.is_same_inode(directory));

        self.directory = Some(directory.clone());

        if is_different_dir {
            Command::SetFilter("".into()).into()
        } else {
            CommandResult::Handled
        }
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
}
