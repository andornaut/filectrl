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
    app::config::theme::Theme, clipboard::ClipboardCommand, file_system::path_info::PathInfo,
    utf8::split_with_ellipsis,
};

pub(super) fn table_widget<'a>(
    theme: &'a Theme,
    column_constraints: Vec<Constraint>,
    rows: Vec<Row<'a>>,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Table<'a> {
    let header = header_row_widget(theme, sort_column, sort_direction);
    Table::new(rows, vec![Constraint::Percentage(100)])
        .header(header)
        .row_highlight_style(theme.table_selected())
        .style(theme.table_body())
        .widths(&column_constraints)
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
    cells.push(Cell::from("Mode").style(theme.table_header())); // Mode cannot be sorted
    Row::new(cells).style(theme.table_header())
}

fn header_cell_widget<'a>(
    theme: &'a Theme,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
    column: SortColumn,
) -> Cell<'a> {
    let is_sorted = *sort_column == column;
    let text = match (is_sorted, sort_direction) {
        (true, SortDirection::Ascending) => match column {
            SortColumn::Name => "[N]ame⌃",
            SortColumn::Modified => "[M]odified⌃",
            SortColumn::Size => "[S]ize⌃",
        },
        (true, SortDirection::Descending) => match column {
            SortColumn::Name => "[N]ame⌄",
            SortColumn::Modified => "[M]odified⌄",
            SortColumn::Size => "[S]ize⌄",
        },
        (false, _) => match column {
            SortColumn::Name => "[N]ame",
            SortColumn::Modified => "[M]odified",
            SortColumn::Size => "[S]ize",
        },
    };
    let label = if is_sorted {
        text.bold()
    } else {
        Span::raw(text)
    };
    Cell::from(label.style(header_style(theme, sort_column, &column)))
}

pub(super) fn row_widget_and_height<'a>(
    theme: &'a Theme,
    clipboard_command: &'a Option<ClipboardCommand>,
    name_column_width: u16,
    relative_to_datetime: DateTime<Local>,
    item: &'a PathInfo,
) -> (Row<'a>, u16) {
    let (name_style, date_style, size_style, row_style) =
        if let Some(clipboard_style) = clipboard_style(theme, clipboard_command, item) {
            (
                clipboard_style,
                clipboard_style,
                clipboard_style,
                clipboard_style,
            )
        } else {
            (
                name_style(&theme.file_types, item),
                modified_date_style(theme, item, relative_to_datetime),
                size_style(theme, item),
                theme.table_body(),
            )
        };

    let name = split_name(name_column_width, item);
    let height = name.len() as u16;
    let row = Row::new([
        Cell::from(name).style(name_style),
        Cell::from(
            item.modified_relative_to(relative_to_datetime)
                .unwrap_or_default(),
        )
        .style(date_style),
        Cell::from(item.size()).style(size_style),
        Cell::from(item.mode()),
    ])
    .height(height)
    .style(row_style);
    (row, height)
}

fn split_name<'a>(width: u16, path: &'a PathInfo) -> Vec<Line<'a>> {
    split_with_ellipsis(&path.name(), width)
        .into_iter()
        .map(Line::from)
        .collect()
}
