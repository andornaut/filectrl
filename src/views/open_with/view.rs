use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
};

use super::{MIN_HEIGHT, OpenWithView, widget::build_rows};
use crate::{
    app::config::Config,
    views::{View, bordered, render_lines},
};

impl View for OpenWithView {
    /// The same constraint as `TableView`, so the picker lands in exactly the
    /// table's slot and nothing above or below it moves.
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        if area.height < MIN_HEIGHT {
            // Zero-size areas clear both hit test regions, so a click on the
            // sliver that is left cannot be tested against a stale layout.
            self.area = Rect::default();
            self.content_area = Rect::default();
            return;
        }
        self.area = area;

        let theme = &Config::global().theme().open_with;
        let style = theme.base();
        let bordered_area = bordered(area, frame.buffer_mut(), style, &self.title, &self.hint);

        self.inner_height = bordered_area.height as usize;
        let max_scroll = self.max_scroll();
        // The viewport height is only known here, so a selection made before
        // the first render may still be off screen.
        self.scroll_offset =
            super::clamp_scroll(self.inner_height, self.selected, self.scroll_offset)
                .min(max_scroll);

        let (content_area, scrollbar_area) = if max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);
            (content_area, scrollbar_area)
        } else {
            // A zero-size area clears the scrollbar's hit test region, so
            // clicks in that column are not treated as scrollbar drags.
            (bordered_area, Rect::default())
        };

        self.content_area = content_area;
        let rows = build_rows(theme, self.selected, content_area.width, &self.candidates);
        render_lines(
            &rows,
            content_area,
            frame.buffer_mut(),
            style,
            self.scroll_offset as u16,
        );
        self.scrollbar_view.render(
            scrollbar_area,
            frame.buffer_mut(),
            self.scroll_offset,
            max_scroll,
            self.inner_height,
        );
    }
}
