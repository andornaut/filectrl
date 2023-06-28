use ratatui::{backend::Backend, layout::Rect, Frame};

pub trait Renderable<B: Backend> {
    fn render(&self, frame: &mut Frame<B>, rect: Rect);
}
