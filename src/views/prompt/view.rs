use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::{Paragraph, Widget},
};
use unicode_width::UnicodeWidthStr;

use super::{PromptView, View};
use crate::app::config::Config;
use crate::command::PromptAction;

impl View for PromptView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Length(1)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        let theme = Config::global().theme();
        let label = self.label();
        let label_width = label.width() as u16;

        if matches!(self.actions, PromptAction::Delete(_)) {
            let label_widget = Paragraph::new(label).style(theme.prompt.delete());
            label_widget.render(area, frame.buffer_mut());
            return;
        }

        let [label_area, input_area] =
            Layout::horizontal([Constraint::Length(label_width), Constraint::Min(1)]).areas(area);

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
