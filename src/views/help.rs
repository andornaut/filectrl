use super::View;
use crate::{
    app::{focus::Focus, style::error_style},
    command::handler::CommandHandler,
    views::bordered,
};
use ratatui::{
    backend::Backend,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

#[derive(Default)]
pub(super) struct HelpView {}

impl CommandHandler for HelpView {}

const MIN_WIDTH: u16 = 44;

impl<B: Backend> View<B> for HelpView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, focus: &Focus) {
        let width = rect.width;
        let rect = bordered(frame, rect, Some("Help".into()));

        if width < MIN_WIDTH {
            let span = Span::raw(format!(
                "Resize your terminal to >={MIN_WIDTH} columns to display help"
            ));
            let text = Text::from(Line::from(span));
            let paragraph = Paragraph::new(text)
                .style(error_style())
                .wrap(Wrap { trim: true });
            frame.render_widget(paragraph, rect);
            return;
        }

        let spans = match *focus {
            Focus::Content => content_help(),
            Focus::Header => header_help(),
            Focus::Prompt => prompt_help(),
        };
        let text = Text::from(Line::from(spans));
        let text_width = text.width();
        let paragraph = Paragraph::new(text)
            .style(Style::default())
            .wrap(Wrap { trim: true });
        eprintln!(
            "FooterView.render() text.width:{} rect.width:{}",
            text_width, rect.width
        );
        frame.render_widget(paragraph, rect);
    }
}

fn content_help() -> Vec<Span<'static>> {
    vec![
        Span::raw("Navigate up: "),
        Span::styled("b", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Select down/up: "),
        Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Open: "),
        Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Refresh: "),
        Span::styled("CTRL+r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Rename: "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Delete: "),
        Span::styled("Delete", Style::default().add_modifier(Modifier::BOLD)),
    ]
}

fn header_help() -> Vec<Span<'static>> {
    vec![
        Span::raw("Navigate up: "),
        Span::styled("b", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Select left/right: "),
        Span::styled("h/l", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" Open selected: "),
        Span::styled("f", Style::default().add_modifier(Modifier::BOLD)),
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
