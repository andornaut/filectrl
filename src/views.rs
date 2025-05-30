mod alerts;
mod header;
mod help;
mod notices;
mod prompt;
pub mod root;
mod status;
mod table;

use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Constraint, Margin, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Widget},
    Frame,
};

use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode},
};

pub(super) trait View: CommandHandler {
    fn constraint(&self, area: Rect, mode: &InputMode) -> Constraint;
    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, mode: &InputMode, theme: &Theme);
}

fn bordered(
    area: Rect,
    buf: &mut Buffer,
    style: Style,
    title_left: Option<&str>,
    title_right: Option<&str>,
) -> Rect {
    let mut block = Block::default().borders(Borders::ALL).border_style(style);
    if let Some(title) = title_left {
        block = block.title(Line::from(title));
    }
    if let Some(title) = title_right {
        block = block.title(Line::from(title).alignment(Alignment::Right));
    }
    block.render(area, buf);
    area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}
