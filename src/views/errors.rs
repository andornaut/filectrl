use std::collections::VecDeque;

use super::{bordered, View};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    backend::Backend,
    layout::Rect,
    widgets::{List, ListItem},
    Frame,
};

const MAX_NUMBER_ERRORS: usize = 5;

#[derive(Default)]
pub(super) struct ErrorsView {
    errors: VecDeque<String>,
    rect: Rect,
}

impl ErrorsView {
    pub(super) fn height(&self) -> u16 {
        if self.should_show() {
            self.errors.len() as u16 + 2 // +2 for borders
        } else {
            0
        }
    }

    fn add_error(&mut self, message: String) -> CommandResult {
        if self.errors.len() == MAX_NUMBER_ERRORS {
            self.errors.pop_front();
        }
        self.errors
            .push_back(format!("{message}{}", self.errors.len()));

        CommandResult::none()
    }

    fn clear_errors(&mut self) -> CommandResult {
        self.errors.clear();
        CommandResult::none()
    }

    fn should_show(&self) -> bool {
        !self.errors.is_empty()
    }
}

impl CommandHandler for ErrorsView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::AddError(message) => self.add_error(message.clone()),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, _: &KeyModifiers) -> CommandResult {
        match *code {
            KeyCode::Char('e') => self.clear_errors(),
            _ => CommandResult::NotHandled,
        }
    }
    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // `self.should_receive_mouse()` guards this method to ensure that the click intersects with this view.
                self.clear_errors();
                CommandResult::none()
            }
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.rect.intersects(Rect::new(x, y, 1, 1))
    }
}

impl<B: Backend> View<B> for ErrorsView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _: &InputMode, theme: &Theme) {
        self.rect = rect;

        if !self.should_show() {
            return;
        }
        let style = theme.error();
        let rect = bordered(frame, rect, style, Some("Errors".into()));
        let items: Vec<ListItem> = self
            .errors
            .iter()
            .rev() // Newest error messages near the top
            .map(|error| ListItem::new(format!(" • {error}")))
            .collect();
        let list = List::new(items).style(style);
        frame.render_widget(list, rect);
    }
}
