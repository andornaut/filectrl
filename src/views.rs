mod alerts;
mod breadcrumbs;
mod help;
mod notices;
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

use crate::command::handler::CommandHandler;

pub(super) trait View: CommandHandler {
    fn constraint(&self, area: Rect) -> Constraint;
    fn render(&mut self, area: Rect, frame: &mut Frame<'_>);
}

/// The left title/message always takes precedence over the right-aligned
/// hint: the hint is only rendered when it fits alongside the full left
/// content, so it never causes the left content to be shortened. `reserved`
/// accounts for non-content columns (e.g. 2 for left + right borders, 0 for a
/// borderless block).
pub(super) fn right_hint_fits(
    total_width: usize,
    left_width: usize,
    right_width: usize,
    reserved: usize,
) -> bool {
    total_width > left_width + right_width + reserved
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

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::right_hint_fits;

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
