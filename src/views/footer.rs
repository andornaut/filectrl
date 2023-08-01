use super::View;
use crate::{app::command::CommandHandler, views::Renderable};
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};

#[derive(Default)]
pub struct FooterView {}

impl<B: Backend> View<B> for FooterView {}

impl CommandHandler for FooterView {}

impl<B: Backend> Renderable<B> for FooterView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let block = Block::default().title("Footer");
        frame.render_widget(block, rect);
    }
}
