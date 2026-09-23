use std::path::Path;

use chrono::{DateTime, Local};
use ratatui::{
    prelude::{Constraint, Stylize},
    text::{Line, Span},
    widgets::{Cell, Row, Table},
};

use super::{
    columns::{SortColumn, SortDirection},
    content::displayed_name,
    style::{
        ClipboardHighlight, clipboard_style, header_style, modified_date_style, name_style,
        size_style,
    },
};
use crate::{
    app::config::theme::Theme,
    file_system::path_info::PathInfo,
    views::{
        as_dimension,
        unicode::{split_line_count, split_with_ellipsis},
    },
};

pub(super) fn table_widget<'a>(
    theme: &'a Theme,
    column_constraints: Vec<Constraint>,
    rows: Vec<Row<'a>>,
    sort_column: SortColumn,
    sort_direction: SortDirection,
) -> Table<'a> {
    let header = header_row_widget(theme, sort_column, sort_direction);
    Table::new(rows, column_constraints)
        .header(header)
        .row_highlight_style(theme.table.selected())
        .style(theme.table.body())
}

fn header_row_widget(
    theme: &Theme,
    sort_column: SortColumn,
    sort_direction: SortDirection,
) -> Row<'_> {
    let mut cells: Vec<_> = [SortColumn::Name, SortColumn::Modified, SortColumn::Size]
        .into_iter()
        .map(|column| header_cell_widget(theme, sort_column, sort_direction, column))
        .collect();
    cells.push(Cell::from("Mode").style(theme.table.header())); // Mode cannot be sorted
    Row::new(cells).style(theme.table.header())
}

fn header_cell_widget(
    theme: &Theme,
    sort_column: SortColumn,
    sort_direction: SortDirection,
    column: SortColumn,
) -> Cell<'_> {
    let is_sorted = sort_column == column;
    let text = match column {
        SortColumn::Name => "[N]ame",
        SortColumn::Modified => "[M]odified",
        SortColumn::Size => "[S]ize",
    };

    // Add direction indicator if this column is sorted
    let text = if is_sorted {
        match sort_direction {
            SortDirection::Ascending => format!("{text}⌃"),
            SortDirection::Descending => format!("{text}⌄"),
        }
    } else {
        text.into()
    };

    // Apply bold styling if this is the sorted column
    let label = if is_sorted {
        text.bold()
    } else {
        Span::raw(text)
    };

    Cell::from(label.style(header_style(&theme.table, sort_column, column)))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn row_widget_and_height<'a>(
    theme: &'a Theme,
    clipboard: Option<&ClipboardHighlight>,
    name_column_width: u16,
    max_lines: usize,
    relative_to_datetime: DateTime<Local>,
    item: &'a PathInfo,
    is_marked: bool,
    is_pending_delete: bool,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> (Row<'a>, u16) {
    let (name_style, date_style, size_style, row_style) = if is_pending_delete {
        let delete = theme.table.delete();
        (delete, delete, delete, delete)
    } else if let Some(clipboard) = clipboard_style(&theme.clipboard, clipboard, item) {
        (clipboard, clipboard, clipboard, clipboard)
    } else if is_marked {
        let marked = theme.table.marked();
        (marked, marked, marked, marked)
    } else if is_bookmarks {
        let bookmark = theme.table.bookmark();
        (bookmark, bookmark, bookmark, bookmark)
    } else {
        (
            name_style(&theme.file_type, item),
            modified_date_style(&theme.file_modified_date, item, relative_to_datetime),
            size_style(&theme.file_size, item),
            theme.table.body(),
        )
    };

    let name = name_lines(
        name_column_width,
        max_lines,
        item,
        is_bookmarks,
        search_root,
    )
    .into_iter()
    .map(Line::from)
    .collect::<Vec<_>>();
    let height = as_dimension(name.len());
    let row = Row::new([
        Cell::from(name).style(name_style),
        Cell::from(item.modified(relative_to_datetime).unwrap_or_default()).style(date_style),
        Cell::from(item.size()).style(size_style),
        Cell::from(item.unix_mode()),
    ])
    .height(height)
    .style(row_style);
    (row, height)
}

/// The wrapped name-column lines for an item, at most `max_lines` of them. A
/// row taller than the viewport is never drawn: ratatui scrolls past a selected
/// row that cannot fit and renders no rows at all. Every line but the last ends
/// in an ellipsis, so the cut keeps the marker that the name continues.
fn name_lines(
    name_column_width: u16,
    max_lines: usize,
    item: &PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> Vec<String> {
    let display = displayed_name(item, is_bookmarks, search_root);
    let mut lines = split_with_ellipsis(&display, name_column_width as usize);
    lines.truncate(max_lines);
    lines
}

/// The rendered height of an item's row: the number of lines `name_lines`
/// returns, counted without building them, so it can be computed for every
/// item to drive the scroll math without building all the `Row` widgets.
pub(super) fn item_height(
    name_column_width: u16,
    max_lines: usize,
    item: &PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> u16 {
    let display = displayed_name(item, is_bookmarks, search_root);
    as_dimension(split_line_count(&display, name_column_width as usize).min(max_lines))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Local;
    use test_case::test_case;

    use super::{item_height, row_widget_and_height};
    use crate::{app::config::Config, file_system::path_info::PathInfo};

    // `item_height` must always agree with the height `row_widget_and_height`
    // actually renders, since the windowing scroll math relies on it.
    #[test_case("short.txt", 40, 10, None ; "fits on one line")]
    #[test_case("a_very_long_file_name_that_must_wrap_across_several_lines.txt", 20, 10, None ; "wraps")]
    #[test_case("中文文件名称非常长非常长非常长.txt", 12, 10, None ; "wide chars")]
    #[test_case("a_very_long_file_name_that_must_wrap_across_several_lines.txt", 20, 2, None ; "capped at the viewport")]
    // Rendered as `sub/name.txt`, which wraps where the bare name would not.
    #[test_case("name.txt", 10, 10, Some("/root") ; "a search result measured by its relative path")]
    fn item_height_matches_rendered_row_height(
        name: &str,
        width: u16,
        max_lines: usize,
        search_root: Option<&str>,
    ) {
        Config::init_test();
        let theme = Config::global().theme();
        let mut item = PathInfo::try_from(Path::new(".")).unwrap();
        item.display_name = name.to_string();
        item.path = Path::new("/root/sub").join(name);
        let search_root = search_root.map(Path::new);

        let (_, rendered_height) = row_widget_and_height(
            theme,
            None,
            width,
            max_lines,
            Local::now(),
            &item,
            false,
            false,
            false,
            search_root,
        );
        assert_eq!(
            item_height(width, max_lines, &item, false, search_root),
            rendered_height
        );
    }
}
