mod errors;
mod header;
mod help;
mod prompt;
pub mod root;
mod status;
mod table;

use ratatui::{
    buffer::Buffer,
    layout::{Margin, Rect},
    style::Style,
    widgets::{Block, Borders, Widget},
};

use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode},
};

pub(super) trait View: CommandHandler {
    fn render(&mut self, buf: &mut Buffer, rect: Rect, mode: &InputMode, theme: &Theme);
}

pub(super) fn bordered(buf: &mut Buffer, rect: Rect, style: Style, title: Option<String>) -> Rect {
    let mut block = Block::default().borders(Borders::ALL).border_style(style);
    if let Some(title) = title {
        block = block.title(title);
    }
    block.render(rect, buf);
    rect.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}
