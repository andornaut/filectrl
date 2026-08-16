use std::path::MAIN_SEPARATOR_STR;

use ratatui::buffer::CellWidth;
use ratatui::{style::Style, text::Span};

#[derive(Debug)]
pub(super) struct Position {
    x_start: u16,
    x_end: u16, // inclusive end of the name; excludes the trailing separator
    index: usize,
}

impl Position {
    pub(super) fn intersects(&self, x: u16) -> bool {
        x >= self.x_start && x <= self.x_end
    }

    pub(super) fn index(&self) -> usize {
        self.index
    }
}

/// Maps a click at column `x` on one rendered row to an index into
/// `BreadcrumbsView::breadcrumbs`, or `None` when the column addresses no
/// navigable entry.
///
/// When a tag is present it occupies position 0 and names no directory, so it
/// is not navigable and every following position addresses the breadcrumb one
/// place before it.
pub(super) fn clicked_index(row: &[Position], x: u16, has_tag: bool) -> Option<usize> {
    row.iter().find_map(|position| {
        if !position.intersects(x) {
            return None;
        }
        let index = position.index();
        if has_tag {
            index.checked_sub(1)
        } else {
            Some(index)
        }
    })
}

pub(super) fn spans<'a>(
    breadcrumbs: &[String],
    width: u16,
    tag_style: Option<Style>,
    basename_style: Style,
    ancestor_style: Style,
    separator_style: Style,
) -> (Vec<Vec<Span<'a>>>, Vec<Vec<Position>>) {
    let mut container: Vec<Vec<Span<'a>>> = Vec::new();
    let mut positions: Vec<Vec<Position>> = Vec::new();
    let mut row_len: u16 = 0;

    let mut it = breadcrumbs.iter().enumerate().peekable();
    while let Some((i, name)) = it.next() {
        let is_last = it.peek().is_none();
        let is_tag = i == 0 && tag_style.is_some();
        let name_style = if is_tag {
            // is_tag is only true when tag_style.is_some(), so this never panics.
            tag_style.unwrap()
        } else if is_last {
            basename_style
        } else {
            ancestor_style
        };

        let display_name = if is_last && name.is_empty() {
            MAIN_SEPARATOR_STR
        } else {
            name
        };
        let name_len = display_name.cell_width();
        // Tags and the last entry have no trailing separator. Path components
        // between them occupy name_len + 1 columns (name + separator).
        let entry_len = name_len + u16::from(!(is_last || is_tag));

        if container.is_empty() || (row_len + entry_len > width && row_len > 0) {
            row_len = 0;
            container.push(Vec::new());
            positions.push(Vec::new());
        }

        let x_start = row_len;
        let x_end = row_len + name_len.saturating_sub(1);
        row_len += entry_len;

        // The block above pushes a new row whenever the container is empty, so
        // there is always at least one row here.
        let container_row = container.last_mut().unwrap();
        container_row.push(Span::styled(display_name.to_owned(), name_style));
        if !is_last && !is_tag {
            container_row.push(Span::styled(MAIN_SEPARATOR_STR, separator_style));
        }

        // positions grows in lockstep with container above, so this is non-empty.
        let positions_row = positions.last_mut().unwrap();
        positions_row.push(Position {
            x_start,
            x_end,
            index: i,
        });
    }
    (container, positions)
}

#[cfg(test)]
mod tests {
    use std::path::MAIN_SEPARATOR;

    use ratatui::style::Style;
    use test_case::test_case;

    use super::{Position, clicked_index, spans};

    fn bc(parts: &[&str]) -> Vec<String> {
        parts.iter().map(std::string::ToString::to_string).collect()
    }

    const SEP: &str = if MAIN_SEPARATOR == '/' { "/" } else { "\\" };

    fn run_spans(parts: &[&str], width: u16) -> (Vec<Vec<String>>, Vec<Vec<Position>>) {
        let (rows, positions) = spans(
            &bc(parts),
            width,
            None,
            Style::default(),
            Style::default(),
            Style::default(),
        );
        let content = rows
            .into_iter()
            .map(|row| row.into_iter().map(|s| s.content.into_owned()).collect())
            .collect();
        (content, positions)
    }

    fn run_tagged_spans(
        parts: &[&str],
        width: u16,
        tag_style: Style,
    ) -> (Vec<Vec<String>>, Vec<Vec<Position>>) {
        let (rows, positions) = spans(
            &bc(parts),
            width,
            Some(tag_style),
            Style::default(),
            Style::default(),
            Style::default(),
        );
        let content = rows
            .into_iter()
            .map(|row| row.into_iter().map(|s| s.content.into_owned()).collect())
            .collect();
        (content, positions)
    }

    // ── tag display ───────────────────────────────────────────────────────────

    #[test]
    fn tagged_breadcrumb_includes_tag_without_trailing_separator() {
        let (rows, _) = run_tagged_spans(&["[Search] ", "home", "user"], 80, Style::default());
        assert_eq!(
            rows,
            vec![vec![
                "[Search] ".to_string(),
                "home".to_string(),
                SEP.to_string(),
                "user".to_string()
            ]]
        );
    }

    #[test]
    fn tagged_breadcrumb_shows_root_separator_at_last_position() {
        let (rows, _) = run_tagged_spans(&["[Search] ", ""], 80, Style::default());
        assert_eq!(rows, vec![vec!["[Search] ".to_string(), SEP.to_string()]]);
    }

    // ── click hit-test with a tag ─────────────────────────────────────────────
    //
    // Layout for &["[Search] ", "", "home", "user"] at width=80. The tag has no
    // trailing separator, so it occupies exactly its own 9 columns:
    //   col 0..=8   → "[Search] " (position 0, not navigable)
    //   col 9       → "" (root, width=0, x_end=9 via saturating_sub) + "/" sep
    //   col 10..=13 → "home" (position 2)
    //   col 14      → "/" separator
    //   col 15..=18 → "user" (position 3)
    //
    // `clicked_index` returns an index into the untagged breadcrumbs, so every
    // position after the tag shifts down by one.

    #[test_case(0  => None    ; "click on the tag is not navigable")]
    #[test_case(8  => None    ; "click on the last column of the tag")]
    #[test_case(9  => Some(0) ; "click on root maps past the tag to breadcrumb 0")]
    #[test_case(10 => Some(1) ; "click on the first char of home")]
    #[test_case(13 => Some(1) ; "click on the last char of home")]
    #[test_case(14 => None    ; "click on the separator")]
    #[test_case(15 => Some(2) ; "click on the first char of user")]
    #[test_case(19 => None    ; "click past the end")]
    fn tagged_click_index(x: u16) -> Option<usize> {
        let (_, positions) =
            run_tagged_spans(&["[Search] ", "", "home", "user"], 80, Style::default());
        clicked_index(&positions[0], x, true)
    }

    // ── row count ─────────────────────────────────────────────────────────────

    #[test_case(&[], 80 => 0 ; "empty input yields no rows")]
    #[test_case(&["", "home", "user"], 80 => 1 ; "all fit in one row")]
    #[test_case(&["", "home", "user"], 1 => 3 ; "each entry on its own row when width=1")]
    fn row_count(parts: &[&str], width: u16) -> usize {
        run_spans(parts, width).0.len()
    }

    // ── span content ──────────────────────────────────────────────────────────

    #[test_case(
        &[""], 80
        => vec![vec![SEP.to_string()]]
        ; "root only displays separator"
    )]
    #[test_case(
        &["", "home", "user"], 80
        => vec![vec![String::new(), SEP.to_string(), "home".to_string(), SEP.to_string(), "user".to_string()]]
        ; "single row: root sep home sep user, no trailing separator"
    )]
    #[test_case(
        &["", "home", "user"], 3
        => vec![
            vec![String::new(), SEP.to_string()],
            vec!["home".to_string(), SEP.to_string()],
            vec!["user".to_string()],
        ]
        ; "wraps when too narrow: no trailing separator on last row"
    )]
    fn span_content(parts: &[&str], width: u16) -> Vec<Vec<String>> {
        run_spans(parts, width).0
    }

    // ── click hit-test ────────────────────────────────────────────────────────
    //
    // The same column addresses a different breadcrumb depending on whether a
    // tag is present, so `has_tag` is what shifts the index, not the position.
    //
    // Layout for &["", "home", "user"] at width=80:
    //   col 0       → "" (root, width=0, x_start=0, x_end=0 via saturating_sub) + "/" sep
    //   col 1..=4   → "home" (x_start=1, x_end=4)
    //   col 5       → "/" separator
    //   col 6..=9   → "user" (x_start=6, x_end=9)

    #[test_case(&["", "home", "user"], 80, 0, 0  => Some(0) ; "click on root (col 0)")]
    #[test_case(&["", "home", "user"], 80, 0, 1  => Some(1) ; "click on first char of home")]
    #[test_case(&["", "home", "user"], 80, 0, 4  => Some(1) ; "click on last char of home")]
    #[test_case(&["", "home", "user"], 80, 0, 5  => None    ; "click on separator between home and user")]
    #[test_case(&["", "home", "user"], 80, 0, 6  => Some(2) ; "click on first char of user")]
    #[test_case(&["", "home", "user"], 80, 0, 9  => Some(2) ; "click on last char of user")]
    #[test_case(&["", "home", "user"], 80, 0, 10 => None    ; "click past end")]
    fn click_index(parts: &[&str], width: u16, row: usize, x: u16) -> Option<usize> {
        let positions = run_spans(parts, width).1;
        clicked_index(&positions[row], x, false)
    }
}
