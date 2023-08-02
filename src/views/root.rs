use super::{content::ContentView, footer::FooterView, header::HeaderView};
use crate::{command::handler::CommandHandler, views::Renderable};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout, Margin, Rect},
    widgets::{Block, Borders},
    Frame,
};

#[derive(Default)]
pub struct RootView {
    content: ContentView,
    footer: FooterView,
    header: HeaderView,
}

impl CommandHandler for RootView {
    fn children(&mut self) -> Vec<&mut dyn CommandHandler> {
        let header: &mut dyn CommandHandler = &mut self.header;
        let content: &mut dyn CommandHandler = &mut self.content;
        let footer: &mut dyn CommandHandler = &mut self.footer;
        vec![header, content, footer]
    }
}

impl<B: Backend> Renderable<B> for RootView {
    fn render(&mut self, frame: &mut Frame<B>, rect: Rect) {
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Length(4),
                    Constraint::Min(5),
                    Constraint::Length(4),
                ]
                .as_ref(),
            );
        let chunks = layout.split(rect);
        let header_rect = bordered(frame, chunks[0]);
        let content_rect = bordered(frame, chunks[1]);
        let footer_rect = bordered(frame, chunks[2]);
        self.header.render(frame, header_rect);
        self.content.render(frame, content_rect);
        self.footer.render(frame, footer_rect);
    }
}

fn bordered<B: Backend>(frame: &mut Frame<B>, rect: Rect) -> Rect {
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(block, rect);
    rect.inner(&Margin {
        horizontal: 1,
        vertical: 1,
    })
}
