use ratatui::{buffer::Buffer, layout::Rect, widgets::StatefulWidget};

use super::ScrollbarView;
use crate::app::config::theme::Theme;

impl ScrollbarView {
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        first_visible_line: usize,
        total_lines_count: usize,
    ) {
        self.area = area;
        let visible_lines_count = self.area.height as usize;
        if total_lines_count <= visible_lines_count {
            return;
        }

        // content_length = max_scroll + 1 (scroll positions, not total rows).
        // This gives thumb size = visible_lines_count / total_lines_count fraction of the track.
        let max_scroll = total_lines_count.saturating_sub(visible_lines_count);
        self.state = self
            .state
            .content_length(max_scroll + 1)
            .viewport_content_length(visible_lines_count)
            .position(first_visible_line);

        let scrollbar_widget = crate::views::scrollbar_widget(&theme.scrollbar);
        StatefulWidget::render(scrollbar_widget, self.area, buf, &mut self.state);
    }
}
