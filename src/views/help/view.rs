use ratatui::{
    Frame,
    layout::{Constraint, Rect},
};

use super::{HelpView, MIN_HEIGHT};
use crate::app::config::theme::Theme;
use crate::views::{View, as_dimension, bordered, render_lines, split_scrollbar};

impl View for HelpView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, theme: &Theme, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        if area.height < MIN_HEIGHT {
            return;
        }

        let style = theme.help.base();
        let bordered_area = bordered(area, frame.buffer_mut(), style, "Help", &self.hint);

        let lines = self.lines(&theme.help);
        self.inner_height = bordered_area.height;
        self.max_scroll = as_dimension(lines.len()).saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        let (content_area, scrollbar_area) = split_scrollbar(bordered_area, self.max_scroll > 0);
        render_lines(&lines, content_area, frame.buffer_mut(), style, scroll);
        self.scrollbar_view.render(
            theme,
            scrollbar_area,
            frame.buffer_mut(),
            scroll as usize,
            self.max_scroll as usize,
            self.inner_height as usize,
        );
    }
}
