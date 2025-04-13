use ratatui::{text::Line, text::Span, widgets::Paragraph};
use tui_input::Input;

use crate::app::config::theme::Theme;

pub(super) fn prompt_widget<'a>(theme: &'a Theme, label: String) -> Paragraph<'a> {
    let line = Line::from(vec![
        Span::styled(label, theme.prompt_label()),
        Span::styled(" ", theme.prompt_input()),
    ]);
    Paragraph::new(line)
}

pub(super) fn input_widget<'a>(
    input: &'a Input,
    theme: &'a Theme,
    x_offset_scroll: usize,
) -> Paragraph<'a> {
    Paragraph::new(input.value())
        .scroll((0, x_offset_scroll as u16))
        .style(theme.prompt_input())
}
