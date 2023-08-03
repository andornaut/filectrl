mod content;
mod errors;
mod footer;
mod header;
mod prompt;
pub mod root;
mod table;

use crate::command::handler::CommandHandler;
use ratatui::{backend::Backend, layout::Rect, Frame};

pub trait View<B: Backend>: CommandHandler {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect);
}
