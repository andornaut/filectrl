use std::path::Path;

use chrono::{DateTime, Local};
use ratatui::{
    prelude::{Constraint, Stylize},
    text::{Line, Span},
    widgets::{Cell, Row, Table},
};

use super::{
    columns::{SortColumn, SortDirection},
    style::{clipboard_style, header_style, modified_date_style, name_style, size_style},
};
use crate::{
    app::{clipboard::ClipboardEntry, config::theme::Theme},
    file_system::path_info::PathInfo,
    views::unicode::split_with_ellipsis,
};

pub(super) fn table_widget<'a>(
    theme: &'a Theme,
    column_constraints: Vec<Constraint>,
    rows: Vec<Row<'a>>,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Table<'a> {
    let header = header_row_widget(theme, sort_column, sort_direction);
    Table::new(rows, column_constraints)
        .header(header)
        .row_highlight_style(theme.table.selected())
        .style(theme.table.body())
}

fn header_row_widget<'a>(
    theme: &'a Theme,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Row<'a> {
    let mut cells: Vec<_> = [SortColumn::Name, SortColumn::Modified, SortColumn::Size]
        .into_iter()
        .map(|column| header_cell_widget(theme, sort_column, sort_direction, column))
        .collect();
    cells.push(Cell::from("Mode").style(theme.table.header())); // Mode cannot be sorted
    Row::new(cells).style(theme.table.header())
}

fn header_cell_widget<'a>(
    theme: &'a Theme,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
    column: SortColumn,
) -> Cell<'a> {
    let is_sorted = *sort_column == column;
    let text = match column {
        SortColumn::Name => "[N]ame",
        SortColumn::Modified => "[M]odified",
        SortColumn::Size => "[S]ize",
    };

    // Add direction indicator if this column is sorted
    let text = if is_sorted {
        match sort_direction {
            SortDirection::Ascending => format!("{}⌃", text),
            SortDirection::Descending => format!("{}⌄", text),
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

    Cell::from(label.style(header_style(&theme.table, sort_column, &column)))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn row_widget_and_height<'a>(
    theme: &'a Theme,
    clipboard_entry: &'a Option<ClipboardEntry>,
    name_column_width: u16,
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
    } else if let Some(clipboard) = clipboard_style(&theme.clipboard, clipboard_entry, item) {
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

    let name = name_lines(name_column_width, item, is_bookmarks, search_root)
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let height = name.len() as u16;
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

/// The wrapped name-column lines for an item. Shared by `row_widget_and_height`
/// (which renders them) and `item_height` (which only needs the count), so the
/// two can never disagree about how tall a row is.
fn name_lines(
    name_column_width: u16,
    item: &PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> Vec<String> {
    let display = if is_bookmarks {
        item.display_name.clone()
    } else {
        display_name(item, search_root)
    };
    split_with_ellipsis(&display, name_column_width as usize)
}

/// The rendered height (number of wrapped name lines) of an item's row. Cheap
/// (no styling or `Cell`/`Line` allocation), so it can be computed for every
/// item each frame to drive scroll math without building all the `Row` widgets.
pub(super) fn item_height(
    name_column_width: u16,
    item: &PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> u16 {
    name_lines(name_column_width, item, is_bookmarks, search_root).len() as u16
}

fn display_name(path: &PathInfo, search_root: Option<&Path>) -> String {
    match search_root {
        Some(root) => {
            let relative = path.path.strip_prefix(root).unwrap_or(&path.path);
            let name = relative.to_string_lossy().to_string();
            if path.is_directory() && !name.ends_with('/') {
                format!("{name}/")
            } else {
                name
            }
        }
        None => path.name().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use chrono::Local;
    use test_case::test_case;

    use super::{item_height, row_widget_and_height};
    use crate::{app::config::Config, file_system::path_info::PathInfo};

    fn ensure_config_initialized() {
        let config = Config::load(None, vec![]).unwrap();
        Config::init(config);
    }

    // `item_height` must always agree with the height `row_widget_and_height`
    // actually renders, since the windowing scroll math relies on it.
    #[test_case("short.txt", 40 ; "fits on one line")]
    #[test_case("a_very_long_file_name_that_must_wrap_across_several_lines.txt", 20 ; "wraps")]
    #[test_case("中文文件名称非常长非常长非常长.txt", 12 ; "wide chars")]
    fn item_height_matches_rendered_row_height(name: &str, width: u16) {
        ensure_config_initialized();
        let theme = Config::global().theme();
        let mut item = PathInfo::try_from(Path::new(".")).unwrap();
        item.display_name = name.to_string();

        let (_, rendered_height) = row_widget_and_height(
            theme,
            &None,
            width,
            Local::now(),
            &item,
            false,
            false,
            false,
            None,
        );
        assert_eq!(item_height(width, &item, false, None), rendered_height);
    }
}
