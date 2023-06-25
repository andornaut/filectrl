use super::Component;
use crate::{
    app::command::{CommandHandler},
    view::Renderable,
};
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};

pub struct Footer {}

impl Footer {
    pub fn new() -> Self {
        Self {}
    }
}
impl<B: Backend> Component<B> for Footer {}

impl CommandHandler for Footer {}

impl<B: Backend> Renderable<B> for Footer {
    fn render(&self, frame: &mut Frame<B>, rect: Rect) {
        let block = Block::default().title("Footer");
        frame.render_widget(block, rect);
    }
}
