mod content;
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
    widgets::{Block, Borders},
    Frame,
};

pub(super) trait View<B: Backend>: CommandHandler {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect, _focus: &Focus);
}

pub(super) fn bordered<B: Backend>(
    frame: &mut Frame<B>,
    rect: Rect,
    title: Option<String>,
) -> Rect {
    let mut block = Block::default().borders(Borders::ALL);
    if let Some(title) = title {
        block = block.title(title);
    }
    frame.render_widget(block, rect);
    rect.inner(&Margin {
        horizontal: 1,
        vertical: 1,
    })
}
