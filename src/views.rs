mod alerts;
mod breadcrumbs;
mod help;
mod notices;
mod prompt;
pub mod root;
mod scrollbar;
mod status;
mod table;
mod unicode;

pub use scrollbar::ScrollbarView;

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Alignment, Constraint, Margin, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Widget},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{config::theme::Theme, AppState},
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
