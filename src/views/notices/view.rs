use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Widget,
};

use super::NoticesView;
use crate::{
    app::config::Config,
    views::View,
};

impl View for NoticesView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Length(self.build_notices().len() as u16)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.notices = self.build_notices();
        if self.notices.is_empty() {
            return;
        }

        self.area = area;
        let theme = Config::global().theme();

        let constraints = vec![Constraint::Length(1); self.notices.len()];
        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        self.notices
            .iter()
            .zip(layout.iter())
            .for_each(|(notice, area)| {
                let widget = notice.create_widget(theme, area.width, &self.tasks, &self.hint);
                widget.render(*area, frame.buffer_mut());
            });
    }
}
