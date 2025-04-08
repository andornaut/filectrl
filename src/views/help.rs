use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use super::{bordered, View};
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult},
};

const MIN_HEIGHT: u16 = 2;

#[derive(Default)]
pub(super) struct HelpView {
    area: Rect,
    is_visible: bool,
}

impl HelpView {
    fn height(&self) -> u16 {
        if self.is_visible {
            4 // 2 lines of text + 2 borders
        } else {
            0
        }
    }

    fn toggle_visibility(&mut self) -> CommandResult {
        self.is_visible = !self.is_visible;
        CommandResult::none()
    }
}

impl CommandHandler for HelpView {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('?'), KeyModifiers::NONE) => self.toggle_visibility(),
            (_, _) => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.is_visible = false;
                CommandResult::none()
            }
            _ => CommandResult::none(),
        }
    }

    fn should_receive_mouse(&self, x: u16, y: u16) -> bool {
        self.is_visible && self.area.intersects(Rect::new(x, y, 1, 1))
    }
}

impl View for HelpView {
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height())
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, mode: &InputMode, theme: &Theme) {
        if !self.is_visible || area.height < MIN_HEIGHT {
            return;
        }
        self.area = area;

        let style = theme.help();
        let bordered_rect = bordered(buf, area, style, Some("Help (Press \"?\" to close)".into()));
        let spans = match *mode {
            InputMode::Prompt => prompt_help(),
            _ => content_help(),
        };
        let widget = Paragraph::new(Line::from(spans))
            .style(style)
            .wrap(Wrap { trim: true });
        widget.render(bordered_rect, buf);
    }
}

fn content_help() -> Vec<Span<'static>> {
    vec![
        Span::raw("Left/Down/Up/Right: "),
        Span::styled("h/j/k/l", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Open: "),
        Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Navigate back: "),
        Span::styled("b", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Refresh: "),
        Span::styled("CTRL+r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Rename: "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Delete: "),
        Span::styled("Delete", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Quit: "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
    ]
}

fn prompt_help() -> Vec<Span<'static>> {
    vec![
        Span::raw("Submit: "),
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Cancel: "),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
    ]
}
