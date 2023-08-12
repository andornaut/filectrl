mod errors;
mod header;
mod help;
mod prompt;
pub mod root;
mod status;
mod table;

use crate::{app::focus::Focus, command::handler::CommandHandler};
use ratatui::{
    backend::Backend,
    layout::Rect,
    prelude::Margin,
    style::Style,
    widgets::{Block, Borders},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;

pub(super) trait View<B: Backend>: CommandHandler {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _focus: &Focus);
}

pub(super) fn bordered<B: Backend>(
    frame: &mut Frame<B>,
    rect: Rect,
    style: Style,
    title: Option<String>,
) -> Rect {
    let mut block = Block::default().borders(Borders::ALL).border_style(style);
    if let Some(title) = title {
        block = block.title(title);
    }
    frame.render_widget(block, rect);
    rect.inner(&Margin {
        horizontal: 1,
        vertical: 1,
    })
}

pub(super) fn split_utf8_with_reservation(
    line: &str,
    width: u16,
    reservation: &str,
) -> Vec<String> {
    if len_utf8(line) <= width {
        return vec![line.to_string()];
    }

    let reserved = len_utf8(reservation);
    split_utf8(line, width.saturating_sub(reserved))
}

fn len_utf8(line: &str) -> u16 {
    UnicodeSegmentation::graphemes(line, true).count() as u16
}

fn split_utf8(line: &str, width: u16) -> Vec<String> {
    let mut graphemes = UnicodeSegmentation::graphemes(line, true);
    (0..)
        .map(|_| graphemes.by_ref().take(width as usize).collect::<String>())
        .take_while(|s| !s.is_empty())
        .collect::<Vec<_>>()
}
