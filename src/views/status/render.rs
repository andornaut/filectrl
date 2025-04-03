use ratatui::{
    prelude::{Backend, Rect},
    Frame,
};

use super::{
    widgets::{clipboard_paragraph, default_paragraph, filter_paragraph, progress_paragraph},
    StatusView,
};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

impl<B: Backend> View<B> for StatusView {
    fn render(&mut self, frame: &mut Frame, rect: Rect, _: &InputMode, theme: &Theme) {
        self.rect = rect;

        let widget = if !self.tasks.is_empty() {
            progress_paragraph(&self.tasks, theme, rect.width)
        } else if self.clipboard.is_some() {
            clipboard_paragraph(&self.clipboard, self.rect.width, theme)
        } else if !self.filter.is_empty() {
            filter_paragraph(&self.filter, theme)
        } else {
            default_paragraph(&self.directory, self.directory_len, &self.selected, theme)
        };
        frame.render_widget(widget, rect);
    }
}
