use rat_widget::textarea::TextArea;
use ratatui::{
    layout::{Constraint, Direction, Layout, Position, Rect},
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
        let [label_area, input_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width), Constraint::Min(1)].as_ref())
            .areas(area);

        let label_widget = prompt_widget(theme, label);
        label_widget.render(label_area, frame.buffer_mut());

        self.input_area = input_area;
        let textarea_widget = TextArea::new().style(theme.prompt_input());
        frame.render_stateful_widget(textarea_widget, input_area, &mut self.input_state);
        let cursor = self.input_state.cursor();
        let cursor_position = Position::new(
            input_area.x + cursor.x as u16 - self.input_state.hscroll.offset() as u16,
            input_area.y + cursor.y as u16,
        );
        self.input_state.scroll_cursor_to_visible();
        frame.set_cursor_position(cursor_position);
        // debug!(
        //     "Cursor details - cursor: {:?}, cursor_position: {:?}, area: {:?}, text_len: {}",
        //     cursor,
        //     cursor_position,
        //     self.input_state.area,
        //     self.input_state.text().len()
        // );
        //self.cursor_position = cursor_position;
    }
}
