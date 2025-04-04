use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
};

use super::{bordered, View};
use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode, result::CommandResult},
};

#[derive(Default)]
pub(super) struct HelpView {
    should_show: bool,
}

impl HelpView {
    pub(super) fn height(&self) -> u16 {
        if self.should_show {
            4 // 2 + 2 for borders
        } else {
            0
        }
    }

    fn toggle_show_help(&mut self) -> CommandResult {
        self.should_show = !self.should_show;
        CommandResult::none()
    }
}

impl CommandHandler for HelpView {
    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match (*code, *modifiers) {
            (KeyCode::Char('?'), KeyModifiers::NONE) => self.toggle_show_help(),
            (_, _) => CommandResult::NotHandled,
        }
    }
}

impl View for HelpView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, mode: &InputMode, theme: &Theme) {
        if !self.should_show {
            return;
        }
        let style = theme.help();
        let bordered_rect = bordered(buf, area, style, Some("Help".into()));
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
        Span::raw(" Navigate up: "),
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
