use super::{bordered, split_with_ellipsis, View};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    backend::Backend,
    layout::Rect,
    text::{Line, Text},
    widgets::Paragraph,
    Frame,
};
use std::collections::VecDeque;

const MAX_NUMBER_ERRORS: usize = 5;

#[derive(Default)]
pub(super) struct ErrorsView {
    errors: VecDeque<String>,
    rect: Rect,
}

impl ErrorsView {
    pub(super) fn height(&self, width: u16) -> u16 {
        if self.should_show() {
            // TODO cache `self.list_items()` result for use in render()
            let width = width.saturating_sub(2); // -2 for horizontal borders
            let items = self.list_items(width);
            items.len() as u16 + 2 // +2 for vertical borders
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

    fn list_items(&self, width: u16) -> Vec<Line<'_>> {
        self.errors
            .iter()
            .rev() // Newest error messages near the top
            .flat_map(|message| {
                split_with_ellipsis(&format!("• {message}"), width)
                    .into_iter()
                    .map(|line| Line::from(line))
            })
            .collect()
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

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('e'), KeyModifiers::NONE) => self.clear_errors(),
            (_, _) => CommandResult::NotHandled,
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
        let bordered_rect = bordered(frame, rect, style, Some("Errors".into()));
        let items = self.list_items(bordered_rect.width);
        frame.render_widget(
            Paragraph::new(Text::from(items)).style(style),
            bordered_rect,
        );
    }
}
