mod errors;
mod header;
mod help;
mod notices;
mod prompt;
pub mod root;
mod status;
mod table;

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Margin, Rect},
    style::Style,
    widgets::{Block, Borders, Widget},
};

use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode},
};

pub(super) trait View: CommandHandler {
    fn constraint(&self, area: Rect, mode: &InputMode) -> Constraint;
    fn render(&mut self, area: Rect, buf: &mut Buffer, mode: &InputMode, theme: &Theme);
}

pub(super) fn bordered(buf: &mut Buffer, area: Rect, style: Style, title: Option<String>) -> Rect {
    let mut block = Block::default().borders(Borders::ALL).border_style(style);
    if let Some(title) = title {
        block = block.title(title);
    }
    block.render(area, buf);
    area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}
