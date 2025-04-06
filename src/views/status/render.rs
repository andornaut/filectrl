use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Rect},
    widgets::Widget,
};

use super::{widgets::default_widget, StatusView};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

impl View for StatusView {
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, _: &InputMode, theme: &Theme) {
        self.area = area;
        let widget = default_widget(&self.directory, self.directory_len, &self.selected, theme);
        widget.render(area, buf);
    }
}
