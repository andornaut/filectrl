// use log::debug;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Widget,
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::{widgets::prompt_widget, PromptView, View};
use crate::{app::config::theme::Theme, command::mode::InputMode};

impl View for PromptView {
    fn constraint(&self, _: Rect, mode: &InputMode) -> Constraint {
        Constraint::Length(self.height(mode))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, mode: &InputMode, theme: &Theme) {
        if !self.should_show(mode) {
            return;
        }

        let label = self.label();
        let label_width = label.width_cjk() as u16;
        let [label_area, input_widget_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width), Constraint::Min(1)].as_ref())
            .areas(area);
        self.input_widget_area = input_widget_area;

        let label_widget = prompt_widget(theme, label);
        label_widget.render(label_area, frame.buffer_mut());

        self.input.set_style(theme.prompt_input());

        frame.render_widget(&self.input, input_widget_area);
    }
}
