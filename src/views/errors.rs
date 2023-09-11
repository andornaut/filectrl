use super::{bordered, View};
use crate::{
    app::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult, Command},
};
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    backend::Backend,
    layout::Rect,
    prelude::Constraint,
    widgets::{Paragraph, Wrap},
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
    pub(super) fn constraint(&self) -> Constraint {
        if self.should_show() {
            // The actual height may be greater if there's text wrapping.
            Constraint::Min(self.errors.len() as u16 + 2) // +2 for borders
        } else {
            Constraint::Length(0)
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

    fn paragraphs(&self) -> Vec<Paragraph<'_>> {
        self.errors
            .iter()
            .rev() // Newest error messages near the top
            .map(|message| Paragraph::new(format!("• {message}")).wrap(Wrap { trim: true }))
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
        self.paragraphs()
            .into_iter()
            .for_each(|paragraph| frame.render_widget(paragraph.style(style), bordered_rect));
    }
}
