use ratatui::{prelude::Rect, Frame};

use super::{
    widgets::{clipboard_widget, default_widget, filter_widget, progress_widget},
    StatusView,
};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

impl View for StatusView {
    fn render(&mut self, frame: &mut Frame, rect: Rect, _: &InputMode, theme: &Theme) {
        self.rect = rect;

        let widget = if !self.tasks.is_empty() {
            progress_widget(&self.tasks, theme, rect.width)
        } else if self.clipboard.is_some() {
            clipboard_widget(&self.clipboard, self.rect.width, theme)
        } else if !self.filter.is_empty() {
            filter_widget(&self.filter, theme)
        } else {
            default_widget(&self.directory, self.directory_len, &self.selected, theme)
        };
        frame.render_widget(widget, rect);
    }
}
