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
    layout::{Alignment, Constraint, Margin, Rect},
    style::Style,
    text::Line,
    widgets::{Block, Borders, Widget},
};

/// A count as a terminal dimension. Terminal geometry is `u16` throughout, and
/// a count that does not fit is one the terminal could not draw regardless, so
/// saturating at the maximum is what an oversized listing should render as. An
/// `as` cast would wrap instead, turning 65_536 rows into none.
pub(crate) fn as_dimension(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// Draw `lines`, starting at the one `scroll` lines down. Equivalent to an
/// unwrapped, left-aligned `Paragraph` (see the equivalence test below), but
/// takes the lines by reference: `Paragraph` owns its text, so handing it
/// cached content would clone every span each frame.
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

use crate::command::{Command, handler::CommandHandler};

pub(super) trait View: CommandHandler {
    fn constraint(&self, area: Rect) -> Constraint;
    fn render(&mut self, area: Rect, frame: &mut Frame<'_>);
}

/// Which listing the table is showing. Search and bookmarks are mutually
/// exclusive. Every view with mode-dependent state must derive transitions
/// from [`ListingMode::transition`] so the rules are written exactly once.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum ListingMode {
    #[default]
    Normal,
    Search,
    Bookmarks,
}

impl ListingMode {
    /// The mode in effect after `command`, or `None` when the command does
    /// not change the mode.
    pub(super) fn transition(command: &Command) -> Option<Self> {
        match command {
            Command::NavigatedDirectory { .. } | Command::ResetView => Some(Self::Normal),
            // The table rejects an empty query, so it must not enter search mode.
            Command::StartSearch(query) if !query.is_empty() => Some(Self::Search),
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

/// The left title/message always takes precedence over the right-aligned
/// hint: the hint is only rendered when it fits alongside the full left
/// content, so it never causes the left content to be shortened. `reserved`
/// accounts for non-content columns (e.g. 2 for left + right borders, 0 for a
/// borderless block).
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

    use super::{ListingMode, render_lines, right_hint_fits};
    use crate::{command::Command, file_system::path_info::PathInfo};

    /// Shaped like real cached content: styled label/value spans, a blank
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
        // An empty query never starts a search.
        assert_eq!(
            None,
            ListingMode::transition(&Command::StartSearch(String::new()))
        );
        assert_eq!(
            Some(ListingMode::Bookmarks),
            ListingMode::transition(&Command::Bookmarks { bookmarks: vec![] })
        );
        // A refresh keeps the current mode.
        assert_eq!(
            None,
            ListingMode::transition(&Command::RefreshedDirectory {
                directory: dir,
                generation: 1,
            })
        );
    }

    // Borderless (reserved = 0): the hint needs at least one spare column
    // beyond the full left content.
    #[test_case(20, 10, 9, 0, true; "borderless: fits with a spare column")]
    #[test_case(20, 10, 10, 0, false; "borderless: no spare column drops the hint")]
    #[test_case(20, 18, 5, 0, false; "borderless: long left content drops the hint")]
    // Bordered (reserved = 2): the two borders also count against the width.
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
