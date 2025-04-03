mod errors;
mod header;
mod help;
mod prompt;
pub mod root;
mod status;
mod table;

use ratatui::{
    backend::Backend,
    layout::{Margin, Rect},
    style::Style,
    widgets::{Block, Borders},
    Frame,
};
use textwrap::{wrap, Options};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::config::theme::Theme,
    command::{handler::CommandHandler, mode::InputMode},
};

const ELLIPSIS: &str = "…";
const NEWLINE_ELLIPSIS: &str = "\n…";

pub(super) trait View<B: Backend>: CommandHandler {
    fn render(&mut self, frame: &mut Frame, rect: Rect, mode: &InputMode, theme: &Theme);
}

pub(super) fn bordered<B: Backend>(
    frame: &mut Frame,
    rect: Rect,
    style: Style,
    title: Option<String>,
) -> Rect {
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
