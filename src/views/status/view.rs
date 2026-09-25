use ratatui::{
    Frame,
    buffer::CellWidth,
    layout::{Constraint, Layout, Rect},
    widgets::{Paragraph, Widget},
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
        let (total, shown) = self.item_count();
        let widget = default_widget(directory, total, shown, self.selected.as_ref(), theme);
        // The hint takes the right edge only where it leaves the fields most
        // of the line.
        let hint_width = self.help_hint.cell_width();
        if hint_width == 0 || area.width < hint_width.saturating_mul(4) {
            widget.render(area, frame.buffer_mut());
            return;
        }
        let [fields, hint] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_width)]).areas(area);
        widget.render(fields, frame.buffer_mut());
        Paragraph::new(self.help_hint.as_str())
            .style(theme.status.label())
            .render(hint, frame.buffer_mut());
    }
}
