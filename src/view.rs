use crate::component::root::Root;
use anyhow::Result;
use ratatui::{backend::Backend, layout::Rect, Frame, Terminal};

pub trait Renderable<B: Backend> {
    fn render(&self, frame: &mut Frame<B>, rect: Rect);
}

pub fn render<B>(terminal: &mut Terminal<B>, root: &mut Root) -> Result<()>
where
    B: Backend,
{
    terminal.draw(|frame| {
        let window = frame.size();
        root.render(frame, window);
    })?;
    Ok(())
}
