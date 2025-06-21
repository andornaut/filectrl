use rat_text::HasScreenCursor;
use rat_widget::textarea::TextArea;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Paragraph, StatefulWidget, Widget},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use super::{PromptView, View};
use crate::{app::config::theme::Theme, command::mode::InputMode};

impl View for PromptView {
    fn constraint(&self, _: Rect, mode: &InputMode) -> Constraint {
        Constraint::Length(self.height(mode))
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, mode: &InputMode, theme: &Theme) {
        let label = self.label();
        let label_width = label.width() as u16;
        let [label_area, input_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width), Constraint::Min(1)].as_ref())
            .areas(area);

        if !self.should_show(mode) {
            return;
        }

        let label_widget = Paragraph::new(label).style(theme.prompt_label());
        label_widget.render(label_area, frame.buffer_mut());
        let textarea_widget = TextArea::new().style(theme.prompt_input());
        textarea_widget.render(input_area, frame.buffer_mut(), &mut self.text_area_state);

        // .screen_cursor() returns None when there's an active selection.
        if let Some((x, y)) = self.text_area_state.screen_cursor() {
            frame.set_cursor_position((x, y));
        }
    }
}
