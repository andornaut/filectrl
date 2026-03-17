use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Widget,
};

use super::NoticesView;
use crate::{
    app::{config::theme::Theme, state::AppState},
    views::View,
};

impl View for NoticesView {
    fn constraint(&self, _: Rect, state: &AppState) -> Constraint {
        Constraint::Length(self.build_notices(state).len() as u16)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState, theme: &Theme) {
        self.notices = self.build_notices(state);
        if self.notices.is_empty() {
            return;
        }

        self.area = area;

        let constraints = vec![Constraint::Length(1); self.notices.len()];
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        self.notices
            .iter()
            .zip(layout.iter())
            .for_each(|(notice, area)| {
                let widget = notice.create_widget(theme, area.width, &self.tasks);
                widget.render(*area, frame.buffer_mut());
            });
    }
}
