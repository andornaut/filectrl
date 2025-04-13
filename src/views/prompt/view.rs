use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::Widget,
};
use tui_input::Input;
use unicode_width::UnicodeWidthStr;

use super::{
    widgets::{input_widget, prompt_widget},
    PromptView, View,
};
use crate::{app::config::theme::Theme, command::mode::InputMode};

impl View for PromptView {
    fn constraint(&self, _: Rect, mode: &InputMode) -> Constraint {
        Constraint::Length(self.height(mode))
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, mode: &InputMode, theme: &Theme) {
        if !self.should_show(mode) {
            return;
        }

        let label = self.label();
        let label_width = label.width_cjk() as u16 + 1; // +1 for the space between label and input
        let [label_area, input_area] = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(label_width), Constraint::Min(1)].as_ref())
            .areas(area);

        let (cursor_x_pos, cursor_x_scroll) = cursor_position(&self.input, input_area);

        let label_widget = prompt_widget(theme, label);
        label_widget.render(label_area, buf);

        let input_widget = input_widget(&self.input, theme, cursor_x_scroll);
        input_widget.render(input_area, buf);

        self.cursor_position.x = input_area.x + (cursor_x_pos - cursor_x_scroll) as u16;
        self.cursor_position.y = input_area.y;
    }
}

fn cursor_position(input: &Input, input_area: Rect) -> (usize, usize) {
    let input_width = input_area.width as usize;
    let cursor_x_pos = input.visual_cursor();
    let cursor_x_scroll = input.visual_scroll(input_width);
    let cursor_x_scroll = if cursor_x_pos >= input_width {
        // When there's horizontal scrolling in the input field, the cursor
        // would otherwise be positioned on the last char instead of after
        // the last char as is the case when there is no scrolling.
        cursor_x_scroll + 1
    } else {
        cursor_x_scroll
    };
    (cursor_x_pos, cursor_x_scroll)
}
