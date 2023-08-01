mod content;
mod errors;
mod footer;
mod header;
mod prompt;
pub mod root;

use crate::command::handler::CommandHandler;
use ratatui::{backend::Backend, layout::Rect, Frame};

pub trait Renderable<B: Backend> {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect);
}

pub trait View<B: Backend>: CommandHandler + Renderable<B> {}
