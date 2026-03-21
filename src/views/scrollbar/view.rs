use ratatui::{
    buffer::Buffer,
    layout::Rect,
    symbols::{block, line},
    widgets::{Scrollbar, ScrollbarOrientation, StatefulWidget},
};

use super::ScrollbarView;
use crate::app::config::{Config, theme::ScrollbarConfig};

impl ScrollbarView {
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        position: usize,
        max_position: usize,
        viewport_size: usize,
    ) {
        self.area = area;
        if max_position == 0 {
            return;
        }

        self.state = self
            .state
            .content_length(max_position + 1)
            .viewport_content_length(viewport_size)
            .position(position);

        let widget = scrollbar_widget(&Config::global().theme().scrollbar);
        StatefulWidget::render(widget, self.area, buf, &mut self.state);
    }
}

fn scrollbar_widget(theme: &ScrollbarConfig) -> Scrollbar<'_> {
    let mut scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .thumb_style(theme.thumb())
        .thumb_symbol(block::FULL)
        .track_style(theme.track())
        .track_symbol(Some(line::VERTICAL));
    if theme.show_ends() {
        scrollbar = scrollbar
            .begin_symbol(Some("▲"))
            .begin_style(theme.ends())
            .end_symbol(Some("▼"))
            .end_style(theme.ends());
    } else {
        scrollbar = scrollbar.begin_symbol(None).end_symbol(None);
    }
    scrollbar
}
