// Text shown here goes through `crate::visible`; see clippy.toml.
#![warn(clippy::disallowed_methods)]

mod alerts;
mod breadcrumbs;
mod help;
mod notices;
mod open_with;
mod prompt;
pub mod root;
mod scrollbar;
mod status;
mod table;
mod unicode;

pub use help::keybindings_help_text;
pub use scrollbar::ScrollbarView;

use ratatui::buffer::CellWidth;
use ratatui::{
    Frame,
    buffer::Buffer,
    crossterm::event::MouseEvent,
    layout::{Alignment, Constraint, Layout, Margin, Position, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Widget},
};

/// A count as a terminal dimension, saturating at `u16::MAX` where `as` would wrap.
pub(crate) fn as_dimension(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

fn contains(area: Rect, event: MouseEvent) -> bool {
    area.contains(Position {
        x: event.column,
        y: event.row,
    })
}

/// The scroll offset that keeps `index` in a `viewport`-long view at `scroll`, moving minimally.
fn scroll_to_show(viewport: usize, scroll: usize, index: usize) -> usize {
    if viewport == 0 {
        return 0;
    }
    if index < scroll {
        index
    } else if index >= scroll + viewport {
        index + 1 - viewport
    } else {
        scroll
    }
}

/// `area` split into content and a one-column scrollbar, or left whole when nothing scrolls.
/// The zero-size scrollbar area clears the scrollbar's hit region.
fn split_scrollbar(area: Rect, scrollable: bool) -> (Rect, Rect) {
    if !scrollable {
        return (area, Rect::default());
    }
    let [content, scrollbar] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    (content, scrollbar)
}

/// Draws `lines` from `scroll` down, like an unwrapped left-aligned `Paragraph` but without cloning
/// the lines.
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
            y: area.y + as_dimension(row),
            height: 1,
            ..area
        };
        line.render(line_area, buf);
    }
}

use crate::app::config::theme::Theme;
use crate::command::{Command, handler::CommandHandler};

pub(super) trait View: CommandHandler {
    fn constraint(&self, area: Rect) -> Constraint;
    fn render(&mut self, theme: &Theme, area: Rect, frame: &mut Frame<'_>);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ListingCount {
    /// The directory's entries, `shown` of them in the table.
    Directory {
        shown: usize,
    },
    Results {
        shown: usize,
        total: usize,
    },
    /// The bookmarks, all shown.
    Bookmarks {
        shown: usize,
    },
}

impl Default for ListingCount {
    fn default() -> Self {
        Self::Directory { shown: 0 }
    }
}

/// Which listing the table shows. Views derive transitions from [`ListingMode::transition`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ListingMode {
    #[default]
    Normal,
    Search,
    Bookmarks,
}

impl ListingMode {
    /// The mode after `command`, or `None` when it does not change the mode.
    pub(super) fn transition(command: &Command) -> Option<Self> {
        match command {
            Command::NavigatedDirectory { .. } | Command::ResetView => Some(Self::Normal),
            Command::StartSearch(_) => Some(Self::Search),
            Command::Bookmarks { .. } => Some(Self::Bookmarks),
            _ => None,
        }
    }
}

fn bordered(
    area: Rect,
    buf: &mut Buffer,
    style: Style,
    title_left: &str,
    title_right: &str,
) -> Rect {
    let fits = right_hint_fits(
        area.width as usize,
        title_left.cell_width() as usize,
        title_right.cell_width() as usize,
        2, // left + right border
    );
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Line::from(title_left));
    if fits {
        block = block.title(Line::from(title_right).alignment(Alignment::Right));
    }
    block.render(area, buf);
    area.inner(Margin::new(1, 1))
}

/// Whether the right-aligned hint fits beside the full left content; the hint never shortens it.
/// `reserved` counts non-content columns (2 for borders).
fn right_hint_fits(
    total_width: usize,
    left_width: usize,
    right_width: usize,
    reserved: usize,
) -> bool {
    total_width > left_width + right_width + reserved
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

    use super::{ListingMode, render_lines, right_hint_fits, scroll_to_show, split_scrollbar};
    use crate::{command::Command, file_system::path_info::PathInfo};

    /// Styled spans, a blank separator, and a line wider than the render area.
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

    #[test_case(true => (Rect::new(2, 3, 9, 4), Rect::new(11, 3, 1, 4)) ; "the last column is the scrollbar")]
    #[test_case(false => (Rect::new(2, 3, 10, 4), Rect::default()) ; "nothing to scroll leaves the area whole")]
    fn split_scrollbar_produces(scrollable: bool) -> (Rect, Rect) {
        split_scrollbar(Rect::new(2, 3, 10, 4), scrollable)
    }

    #[test_case(0, 3, 5 => 0 ; "an unmeasured viewport pins the offset to the start")]
    #[test_case(5, 0, 2 => 0 ; "already visible, no movement")]
    #[test_case(5, 0, 4 => 0 ; "the last visible index does not scroll")]
    #[test_case(5, 0, 5 => 1 ; "one past the end scrolls by one")]
    #[test_case(5, 0, 9 => 5 ; "a jump past the end scrolls just far enough")]
    #[test_case(5, 4, 2 => 2 ; "before the viewport scrolls back to the index")]
    #[test_case(5, 6, 6 => 6 ; "the first visible index does not scroll")]
    fn scroll_to_show_keeps_the_index_in_view(
        viewport: usize,
        scroll: usize,
        index: usize,
    ) -> usize {
        scroll_to_show(viewport, scroll, index)
    }

    #[test_case(0 ; "unscrolled")]
    #[test_case(1 ; "scrolled past the first line")]
    #[test_case(3 ; "scrolled to the last line")]
    #[test_case(9 ; "scrolled past the end")]
    fn render_lines_paints_what_paragraph_painted(scroll: u16) {
        // A non-zero origin, so a row-offset error shows rather than being clipped.
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

    #[test]
    fn a_count_too_large_for_the_terminal_saturates() {
        assert_eq!(u16::MAX, super::as_dimension(usize::from(u16::MAX) + 1));
    }

    #[test]
    fn listing_mode_transitions_cover_the_mode_changing_commands() {
        let dir = PathInfo::try_from("/tmp").unwrap();
        assert_eq!(
            Some(ListingMode::Normal),
            ListingMode::transition(&Command::NavigatedDirectory {
                directory: dir.clone(),
                generation: 1,
            })
        );
        assert_eq!(
            Some(ListingMode::Normal),
            ListingMode::transition(&Command::ResetView)
        );
        assert_eq!(
            Some(ListingMode::Search),
            ListingMode::transition(&Command::StartSearch("q".into()))
        );
        assert_eq!(
            Some(ListingMode::Bookmarks),
            ListingMode::transition(&Command::Bookmarks { bookmarks: vec![] })
        );
        assert_eq!(
            None,
            ListingMode::transition(&Command::RefreshedDirectory {
                directory: dir,
                generation: 1,
            })
        );
    }

    #[test_case(20, 10, 9, 0, true; "borderless: fits with a spare column")]
    #[test_case(20, 10, 10, 0, false; "borderless: no spare column drops the hint")]
    #[test_case(20, 18, 5, 0, false; "borderless: long left content drops the hint")]
    #[test_case(20, 10, 7, 2, true; "bordered: fits once borders are reserved")]
    #[test_case(20, 10, 8, 2, false; "bordered: borders push the hint out")]
    fn right_hint_fits_respects_left_precedence(
        total: usize,
        left: usize,
        right: usize,
        reserved: usize,
        expected: bool,
    ) {
        assert_eq!(expected, right_hint_fits(total, left, right, reserved));
    }
}
