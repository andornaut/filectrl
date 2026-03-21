use ratatui::{
    layout::{Constraint, Rect},
    widgets::Widget,
    Frame,
};

use super::{widgets::default_widget, StatusView};
use crate::{app::{config::Config, AppState}, views::View};

impl View for StatusView {
    fn constraint(&self, _: Rect, _: &AppState) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _: &AppState) {
        let Some(directory) = &self.directory else {
            return;
        };
        let theme = Config::global().theme();
        let widget = default_widget(directory, self.directory_len, &self.selected, theme);
        widget.render(area, frame.buffer_mut());
    }
}
