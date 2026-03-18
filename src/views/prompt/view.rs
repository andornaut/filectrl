use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::{PromptView, View};
use crate::app::{config::theme::Theme, state::AppState};

impl View for PromptView {
    fn constraint(&self, _: Rect, state: &AppState) -> Constraint {
        Constraint::Length(if self.should_show(&state.mode) { 1 } else { 0 })
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState, theme: &Theme) {
        if !self.should_show(&state.mode) {
            return;
        }

        let label = self.label();
        let label_width = label.width() as u16;
        let [label_area, input_area] = Layout::horizontal([Constraint::Length(label_width), Constraint::Min(1)])
            .areas(area);

        let label_widget = Paragraph::new(label).style(theme.prompt.label());
        label_widget.render(label_area, frame.buffer_mut());

        self.text_area.set_style(theme.prompt.input());
        self.text_area.set_selection_style(theme.prompt.selected());
        self.text_area.set_cursor_style(theme.prompt.cursor());
        self.render_area = input_area;
        frame.render_widget(&self.text_area, input_area);
        self.update_scroll_col(input_area.width);
    }
}
