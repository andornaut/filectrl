use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Paragraph, Widget},
};

use super::{HelpView, MIN_HEIGHT};
use crate::{
    app::config::Config,
    views::{View, bordered},
};

impl View for HelpView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        if area.height < MIN_HEIGHT {
            return;
        }

        let theme = Config::global().theme();
        let style = theme.help.base();
        let bordered_area = bordered(area, frame.buffer_mut(), style, "Help", &self.hint);

        self.inner_height = bordered_area.height;
        self.max_scroll = self.content_height.saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        if self.max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);

            Paragraph::new(self.lines.clone())
                .style(style)
                .scroll((scroll, 0))
                .render(content_area, frame.buffer_mut());

            self.scrollbar_view.render(
                scrollbar_area,
                frame.buffer_mut(),
                scroll as usize,
                self.max_scroll as usize,
                self.inner_height as usize,
            );
        } else {
            self.scrollbar_view
                .render(Rect::default(), frame.buffer_mut(), 0, 0, 0);
            Paragraph::new(self.lines.clone())
                .style(style)
                .render(bordered_area, frame.buffer_mut());
        }
    }
}
