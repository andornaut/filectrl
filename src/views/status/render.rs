use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};

use super::{
    widgets::{clipboard_widget, default_widget, filter_widget, progress_widget},
    StatusView,
};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

impl View for StatusView {
    fn render(&mut self, buf: &mut Buffer, area: Rect, _: &InputMode, theme: &Theme) {
        self.area = area;

        let widget = if !self.tasks.is_empty() {
            progress_widget(&self.tasks, theme, area.width)
        } else if self.clipboard.is_some() {
            clipboard_widget(&self.clipboard, self.area.width, theme)
        } else if !self.filter.is_empty() {
            filter_widget(&self.filter, theme)
        } else {
            default_widget(&self.directory, self.directory_len, &self.selected, theme)
        };
        widget.render(area, buf);
    }
}
