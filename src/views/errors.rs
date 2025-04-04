use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    text::{Line, Text},
    widgets::{Paragraph, Widget},
};
use std::collections::VecDeque;

use super::{bordered, View};
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
    utf8::split_with_ellipsis,
};

const MAX_NUMBER_ERRORS: usize = 5;

#[derive(Default)]
pub(super) struct ErrorsView {
    errors: VecDeque<String>,
    area: Rect,
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
        self.errors.push_back(message);
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
                split_with_ellipsis(message, width.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let prefix = if i == 0 { "•" } else { " " };
                        Line::from(format!("{prefix} {line}"))
                    })
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
            (KeyCode::Char('e'), KeyModifiers::NONE) | (KeyCode::Char('c'), _) => {
                self.clear_errors()
            }
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
        self.area.intersects(Rect::new(x, y, 1, 1))
    }
}

impl View for ErrorsView {
    fn constraint(&self, area: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height(area.width))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, _: &InputMode, theme: &Theme) {
        self.area = area;
        if !self.should_show() {
            return;
        }
        let style = theme.error();
        let bordered_area = bordered(buf, area, style, Some("Errors".into()));
        let items = self.list_items(bordered_area.width);
        let widget = Paragraph::new(Text::from(items)).style(style);
        widget.render(bordered_area, buf);
    }
}
