use rat_text::HasScreenCursor;
use rat_widget::textarea::TextArea;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Paragraph, StatefulWidget, Widget},
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
        let [label_area, input_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width), Constraint::Min(1)].as_ref())
            .areas(area);

        let label_widget = Paragraph::new(label).style(theme.prompt_label());
        label_widget.render(label_area, frame.buffer_mut());
        let textarea_widget = TextArea::new()
            .style(theme.prompt_input())
            .select_style(theme.prompt_selection())
            .cursor_style(theme.prompt_cursor());
        textarea_widget.render(input_area, frame.buffer_mut(), &mut self.text_area_state);

        // .screen_cursor() returns None when there's an active selection.
        if let Some((x, y)) = self.text_area_state.screen_cursor() {
            frame.set_cursor_position((x, y));
        }
    }
}
