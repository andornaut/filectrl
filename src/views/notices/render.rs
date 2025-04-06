use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Widget,
};

use super::{
    widgets::{clipboard_widget, filter_widget, progress_widget},
    NoticesView,
};
use crate::views::View;
use crate::{app::config::theme::Theme, command::mode::InputMode};

impl View for NoticesView {
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height())
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, _: &InputMode, theme: &Theme) {
        self.area = area;

        let mut widgets = Vec::new();
        let mut constraints = Vec::new();

        if !self.tasks.is_empty() {
            let widget = progress_widget(&self.tasks, theme, area.width);
            widgets.push(widget);
            constraints.push(Constraint::Length(1));
        }

        if let Some((is_cut, path)) = &self.clipboard {
            let widget = clipboard_widget(path, is_cut, area.width, theme);
            widgets.push(widget);
            constraints.push(Constraint::Length(1));
        }

        if !self.filter.is_empty() {
            let widget = filter_widget(&self.filter, theme);
            widgets.push(widget);
            constraints.push(Constraint::Length(1));
        }

        if !widgets.is_empty() {
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(constraints)
                .split(area);

            for (area, widget) in layout.into_iter().zip(widgets) {
                widget.render(*area, buf);
            }
        }
    }
}
