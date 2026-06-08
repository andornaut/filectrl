use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    text::Line,
    widgets::{Paragraph, Widget},
};

use super::{
    HelpView, MIN_HEIGHT,
    widget::{add_keybinding_lines, add_section_header, max_label_width},
};
use crate::{
    app::config::Config,
    views::{View, bordered},
};

impl View for HelpView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        self.area = area;
        if area.height < MIN_HEIGHT {
            return;
        }

        let theme = Config::global().theme();
        let style = theme.help.base();
        let bordered_area = bordered(area, frame.buffer_mut(), style, "Help", &self.hint);

        let max_width = max_label_width(&self.normal_keybindings, &self.prompt_keybindings);
        let mut lines: Vec<Line> = Vec::new();
        add_section_header(&mut lines, "Normal Mode", max_width, &theme.help);
        add_keybinding_lines(&mut lines, &self.normal_keybindings, max_width, &theme.help);
        lines.push(Line::raw(""));
        add_section_header(&mut lines, "Prompt Mode", max_width, &theme.help);
        add_keybinding_lines(&mut lines, &self.prompt_keybindings, max_width, &theme.help);

        let content_height = lines.len() as u16;
        self.inner_height = bordered_area.height;
        self.max_scroll = content_height.saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        if self.max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);

            Paragraph::new(lines)
                .style(style)
                .scroll((scroll, 0))
                .render(content_area, frame.buffer_mut());

            self.scrollbar_view.render(
                scrollbar_area,
                frame.buffer_mut(),
                scroll as usize,
                self.max_scroll as usize,
                self.inner_height as usize,
            );
        } else {
            self.scrollbar_view
                .render(Rect::default(), frame.buffer_mut(), 0, 0, 0);
            Paragraph::new(lines)
                .style(style)
                .render(bordered_area, frame.buffer_mut());
        }
    }
}
