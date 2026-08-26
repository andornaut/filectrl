use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    widgets::Widget,
};

use super::{StatusView, widget::default_widget};
use crate::app::config::theme::Theme;
use crate::views::View;

impl View for StatusView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&mut self, theme: &Theme, area: Rect, frame: &mut Frame<'_>) {
        let Some(directory) = &self.directory else {
            return;
        };
        let widget = default_widget(directory, self.directory_len, self.selected.as_ref(), theme);
        widget.render(area, frame.buffer_mut());
    }
}
