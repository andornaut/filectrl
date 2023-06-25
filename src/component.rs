mod content;
mod footer;
mod header;
pub mod root;

use crate::{app::command::CommandHandler, view::Renderable};
use ratatui::backend::Backend;

pub trait Component<B: Backend>: CommandHandler + Renderable<B> {}
