use ratatui::{buffer::Buffer, layout::Rect, widgets::StatefulWidget};

use super::{widget::scrollbar, ScrollbarView};
use crate::app::config::theme::Theme;

impl ScrollbarView {
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        selected_line: usize,
        total_lines_count: usize,
    ) {
        self.area = area;
        if total_lines_count <= self.area.height as usize {
            return;
        }

        self.state = self
            .state
            .content_length(total_lines_count)
            .position(selected_line);

        let scrollbar_widget = scrollbar(theme);
        StatefulWidget::render(scrollbar_widget, self.area, buf, &mut self.state);
    }
}
