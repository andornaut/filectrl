use ratatui::{backend::Backend, layout::Rect, Frame};

pub trait Renderable<B: Backend> {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect);
}
