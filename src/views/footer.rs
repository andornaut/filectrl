use super::View;
use crate::{app::focus::Focus, command::handler::CommandHandler};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::Paragraph,
    Frame,
};

#[derive(Default)]
pub(super) struct FooterView {}

impl CommandHandler for FooterView {}

impl<B: Backend> View<B> for FooterView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, focus: &Focus) {
        let block = match *focus {
            Focus::Content => content_help(),
            Focus::Header => header_help(),
            Focus::Prompt => prompt_help(),
        };
        frame.render_widget(block, rect);
    }
}

fn content_help() -> Paragraph<'static> {
    let spans = vec![
        Span::raw("Navigate up: "),
        Span::styled("Backspace", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Select down/up: "),
        Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Open selected: "),
        Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Refresh: "),
        Span::styled("CTRL+r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Rename: "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Delete: "),
        Span::styled("Delete", Style::default().add_modifier(Modifier::BOLD)),
    ];
    let text = Text::from(Line::from(spans));
    Paragraph::new(text).style(Style::default())
}

fn header_help() -> Paragraph<'static> {
    let spans = vec![
        Span::raw("Navigate up: "),
        Span::styled("Backspace", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Select left/right: "),
        Span::styled("h/l", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Open selected: "),
        Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
    ];
    let text = Text::from(Line::from(spans));
    Paragraph::new(text).style(Style::default())
}

fn prompt_help() -> Paragraph<'static> {
    let spans = vec![
        Span::raw("Submit: "),
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Cancel: "),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
    ];
    let text = Text::from(Line::from(spans));
    Paragraph::new(text).style(Style::default())
}
