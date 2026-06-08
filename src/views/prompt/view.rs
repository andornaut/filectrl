use ratatui::buffer::CellWidth;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    widgets::Widget,
};

use super::widget::{delete_label_widget, label_widget, suggestion_overlay_text};
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
        let label_width = label.cell_width();

        if matches!(self.actions, PromptAction::Delete(_)) {
            delete_label_widget(label, theme).render(area, frame.buffer_mut());
            return;
        }

        let [label_area, input_area] =
            Layout::horizontal([Constraint::Length(label_width), Constraint::Min(1)]).areas(area);

        label_widget(label, theme).render(label_area, frame.buffer_mut());

        self.text_area.set_style(theme.prompt.input());
        self.text_area.set_selection_style(theme.prompt.selected());
        self.text_area.set_cursor_style(theme.prompt.cursor());
        self.render_area = input_area;
        frame.render_widget(&self.text_area, input_area);
        self.update_scroll_col(input_area.width);

        // Goto type-ahead: paint the muted completion suffix + match counter
        // as an overlay after the typed text, only while the cursor is at the
        // end of the input (otherwise it would misalign with an interior cursor).
        if matches!(self.actions, PromptAction::Goto { .. })
            && self.cursor_at_end()
            && let Some((suffix, idx, total)) = self.current_suggestion()
        {
            let typed_width = self.text_area.lines()[0].cell_width();
            let start = typed_width.saturating_sub(self.scroll_col);
            if start < input_area.width {
                let text = suggestion_overlay_text(suffix, idx, total);
                let max_width = input_area.width.saturating_sub(start) as usize;
                frame.buffer_mut().set_stringn(
                    input_area.x + start,
                    input_area.y,
                    text,
                    max_width,
                    theme.prompt.goto_suggestion(),
                );
            }
        }
    }
}
