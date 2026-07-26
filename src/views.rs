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
    use test_case::test_case;

    use super::{ListingMode, right_hint_fits};
    use crate::{command::Command, file_system::path_info::PathInfo};

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
