use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::{BreadcrumbsView, widget::spans};
use crate::{app::config::Config, views::View};

impl View for BreadcrumbsView {
    fn constraint(&self, area: Rect) -> Constraint {
        Constraint::Length(self.height(area.width))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        let theme = Config::global().theme();
        let display = self.display_breadcrumbs();

        let tag_style = if self.is_bookmarks {
            Some(theme.breadcrumbs.bookmarks())
        } else if self.is_searching {
            Some(theme.breadcrumbs.search())
        } else {
            None
        };
        let (mut container, mut positions) = spans(
            &display,
            self.area.width,
            tag_style,
            theme.breadcrumbs.basename(),
            theme.breadcrumbs.ancestor(),
            theme.breadcrumbs.separator(),
        );

        // Prioritize displaying the deepest directories.
        // positions.len() >= area.height always holds: constraint() requests exactly
        // self.height() rows, and the layout engine never allocates more than requested.
        // This invariant is relied upon by handle_mouse, which indexes into self.positions
        // using a y offset guaranteed to be < self.area.height by should_handle_mouse.
        debug_assert!(
            positions.len() >= self.area.height as usize,
            "layout allocated more height than the header requested"
        );
        let at = positions.len().saturating_sub(self.area.height as usize);
        let container = container.split_off(at);
        self.positions = positions.split_off(at);

        let text: Vec<_> = container.into_iter().map(Line::from).collect();

        let widget = Paragraph::new(text).style(theme.breadcrumbs.base());
        widget.render(self.area, frame.buffer_mut());
    }
}
