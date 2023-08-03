use super::View;
use crate::command::handler::CommandHandler;
use ratatui::{backend::Backend, layout::Rect, widgets::Block, Frame};

#[derive(Default)]
pub(super) struct FooterView {}

impl CommandHandler for FooterView {}

impl<B: Backend> View<B> for FooterView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let block = Block::default().title("Footer");
        frame.render_widget(block, rect);
    }
}
