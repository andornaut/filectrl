use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};
use tui_input::Input;

use crate::app::config::theme::Theme;

pub(super) fn prompt_widget<'a>(theme: &'a Theme, label: String) -> Paragraph<'a> {
    Paragraph::new(label).style(theme.prompt_label())
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

// Create a new version of input widget that supports text selection
pub(super) fn input_widget_with_selection<'a>(
    input: &'a Input,
    theme: &'a Theme,
    x_offset_scroll: usize,
    selection: Option<(usize, usize)>,
) -> Paragraph<'a> {
    let value = input.value();

    // If there's a selection, create styled spans
    if let Some((start, end)) = selection {
        let selection_style = Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);

        let before = &value[..start];
        let selected = &value[start..end];
        let after = &value[end..];

        let spans = vec![
            Span::styled(before, theme.prompt_input()),
            Span::styled(selected, selection_style),
            Span::styled(after, theme.prompt_input()),
        ];

        Paragraph::new(Line::from(spans))
            .scroll((0, x_offset_scroll as u16))
            .style(theme.prompt_input())
            .wrap(Wrap { trim: true })
    } else {
        // If no selection, use the simple version
        input_widget(input, theme, x_offset_scroll)
    }
}
