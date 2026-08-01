use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::Style,
    text::Line,
    widgets::Widget,
};

use super::{HelpView, MIN_HEIGHT};
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

        self.inner_height = bordered_area.height;
        self.max_scroll = self.content_height.saturating_sub(self.inner_height);
        let scroll = self.scroll_offset.min(self.max_scroll);

        if self.max_scroll > 0 {
            let [content_area, scrollbar_area] = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(1)])
                .areas(bordered_area);

            render_lines(&self.lines, content_area, frame.buffer_mut(), style, scroll);

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
            render_lines(
                &self.lines,
                bordered_area,
                frame.buffer_mut(),
                style,
                scroll,
            );
        }
    }
}

/// Draw `lines`, starting at the one `scroll` lines down. Equivalent to an
/// unwrapped, left-aligned `Paragraph` (see the equivalence test below), but
/// takes the lines by reference: `Paragraph` owns its text, so handing it the
/// cached help content would clone every span each frame.
fn render_lines(lines: &[Line<'_>], area: Rect, buf: &mut Buffer, style: Style, scroll: u16) {
    let area = area.intersection(buf.area);
    buf.set_style(area, style);
    for (row, line) in lines
        .iter()
        .skip(scroll as usize)
        .take(area.height as usize)
        .enumerate()
    {
        let line_area = Rect {
            y: area.y + row as u16,
            height: 1,
            ..area
        };
        line.render(line_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Style},
        text::{Line, Span},
        widgets::{Paragraph, Widget},
    };
    use test_case::test_case;

    use super::render_lines;

    /// Shaped like the real help content: styled label/keys spans, a blank
    /// separator, and a line wider than the render area.
    fn lines() -> Vec<Line<'static>> {
        vec![
            Line::from(vec![
                Span::styled("Quit", Style::default().fg(Color::Blue)),
                Span::raw(": "),
                Span::styled("q", Style::default().fg(Color::Red)),
            ]),
            Line::raw(""),
            Line::from(vec![Span::styled(
                "Toggle help: a label wider than the area",
                Style::default().fg(Color::Yellow),
            )]),
            Line::raw("last"),
        ]
    }

    #[test_case(0 ; "unscrolled")]
    #[test_case(1 ; "scrolled past the first line")]
    #[test_case(3 ; "scrolled to the last line")]
    #[test_case(9 ; "scrolled past the end")]
    fn render_lines_paints_what_paragraph_painted(scroll: u16) {
        // A non-zero origin inside a larger buffer, so a row-offset error
        // would show up as a mismatch rather than being clipped away.
        let buffer_area = Rect::new(0, 0, 20, 10);
        let area = Rect::new(2, 3, 12, 3);
        let style = Style::default().fg(Color::Green);

        let mut expected = Buffer::empty(buffer_area);
        Paragraph::new(lines())
            .style(style)
            .scroll((scroll, 0))
            .render(area, &mut expected);

        let mut actual = Buffer::empty(buffer_area);
        render_lines(&lines(), area, &mut actual, style, scroll);

        assert_eq!(expected, actual);
    }

    #[test]
    fn render_lines_clips_an_area_that_overflows_the_buffer() {
        // The area runs past both the right and the bottom edge.
        let buffer_area = Rect::new(0, 0, 8, 2);
        let area = Rect::new(4, 1, 12, 4);
        let style = Style::default().fg(Color::Green);

        let mut expected = Buffer::empty(buffer_area);
        Paragraph::new(lines())
            .style(style)
            .render(area, &mut expected);

        let mut actual = Buffer::empty(buffer_area);
        render_lines(&lines(), area, &mut actual, style, 0);

        assert_eq!(expected, actual);
    }
}
