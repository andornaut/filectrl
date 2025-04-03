mod errors;
mod header;
mod help;
mod prompt;
pub mod root;
mod status;
mod table;

use ratatui::{
    layout::{Margin, Rect},
    style::Style,
    widgets::{Block, Borders},
    Frame,
};

use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode},
};

pub(super) trait View: CommandHandler {
    fn render(&mut self, frame: &mut Frame, rect: Rect, mode: &InputMode, theme: &Theme);
}

pub(super) fn bordered(frame: &mut Frame, rect: Rect, style: Style, title: Option<String>) -> Rect {
    let mut block = Block::default().borders(Borders::ALL).border_style(style);
    if let Some(title) = title {
        block = block.title(title);
    }
    frame.render_widget(block, rect);
    rect.inner(Margin {
        horizontal: 1,
        vertical: 1,
    })
}
