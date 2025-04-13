mod handler;
mod view;
mod widgets;

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use tui_input::{backend::crossterm::EventHandler, Input};

use super::View;
use crate::{
    command::{mode::InputMode, result::CommandResult, Command, PromptKind},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct CursorPosition {
    x: u16,
    y: u16,
}

#[derive(Default)]
pub(super) struct SelectionState {
    active: bool,
    start: usize,
    end: usize,
}

#[derive(Default)]
pub(super) struct PromptView {
    cursor_position: CursorPosition,
    directory: Option<PathInfo>,
    filter: String,
    input: Input,
    area: Rect,
    kind: PromptKind,
    selected: Option<PathInfo>,
    selection: SelectionState,
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

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let key_event = KeyEvent::new(code, modifiers);
        self.input.handle_event(&Event::Key(key_event));
        CommandResult::none()
    }

    fn label(&self) -> String {
        match self.kind {
            PromptKind::Filter => " Filter ".into(),
            PromptKind::Rename => " Rename ".into(),
        }
    }

    fn open(&mut self, kind: &PromptKind) -> CommandResult {
        self.kind = kind.clone();

        match &self.kind {
            PromptKind::Filter => self.input = Input::new(self.filter.clone()),
            PromptKind::Rename => {
                if let Some(selected) = &self.selected {
                    self.input = Input::new(selected.basename.clone())
                }
            }
        }
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
        let value = self.input.value().to_string();
        match self.kind {
            PromptKind::Filter => Command::SetFilter(value).into(),
            PromptKind::Rename => match &self.selected {
                Some(selected_path) => Command::RenamePath(selected_path.clone(), value).into(),
                None => CommandResult::none(),
            },
        }
    }
}
