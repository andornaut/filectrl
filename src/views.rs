mod alerts;
mod breadcrumbs;
mod help;
mod notices;
mod prompt;
pub mod root;
mod status;
mod table;
mod unicode;

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Margin, Rect},
    style::Style,
    symbols::{block, line},
    text::Line,
    widgets::{Block, Borders, Scrollbar, ScrollbarOrientation, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{config::theme::{ScrollbarConfig, Theme}, AppState},
    command::handler::CommandHandler,
};

pub(super) trait View: CommandHandler {
    fn constraint(&self, area: Rect, state: &AppState) -> Constraint;
    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState, theme: &Theme);
}

fn bordered(
    area: Rect,
    buf: &mut Buffer,
    style: Style,
    title_left: &str,
    title_right: &str,
) -> Rect {
    let fits = (area.width as usize) > title_left.width() + title_right.width() + 2; // 2 = left + right border
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Line::from(title_left));
    if fits {
        block = block.title(Line::from(title_right).alignment(Alignment::Right));
    }
    block.render(area, buf);
    area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}

fn scrollbar_widget(theme: &ScrollbarConfig) -> Scrollbar<'_> {
    let mut scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .thumb_style(theme.thumb())
        .thumb_symbol(block::FULL)
        .track_style(theme.track())
        .track_symbol(Some(line::VERTICAL));
    if theme.show_ends() {
        scrollbar = scrollbar
            .begin_symbol(Some("▲"))
            .begin_style(theme.ends())
            .end_symbol(Some("▼"))
            .end_style(theme.ends());
    } else {
        scrollbar = scrollbar.begin_symbol(None).end_symbol(None);
    }
    scrollbar
}
