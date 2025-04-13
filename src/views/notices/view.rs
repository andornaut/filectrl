use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Widget,
    Frame,
};

use super::NoticesView;
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

impl View for NoticesView {
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        Constraint::Length(self.height())
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _: &InputMode, theme: &Theme) {
        self.area = area;

        let notices: Vec<_> = self.active_notices().collect();
        if notices.is_empty() {
            return;
        }

        let constraints = vec![Constraint::Length(1); notices.len()];
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        for (notice, area) in notices.iter().zip(layout.iter()) {
            let widget = notice.create_widget(theme, area.width, &self.tasks);
            widget.render(*area, frame.buffer_mut());
        }
    }
}
