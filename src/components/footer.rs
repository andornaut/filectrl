use super::Component;
use crate::{app::command::CommandHandler, views::Renderable};
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};

#[derive(Default)]
pub struct Footer {}

impl<B: Backend> Component<B> for Footer {}

impl CommandHandler for Footer {}

impl<B: Backend> Renderable<B> for Footer {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let block = Block::default().title("Footer");
        frame.render_widget(block, rect);
    }
}
