use ratatui::{
    prelude::{Constraint, Stylize},
    symbols::{block, line},
    text::{Line, Span},
    widgets::{Cell, Row, Scrollbar, ScrollbarOrientation, Table},
};

use super::{
    columns::{SortColumn, SortDirection},
    style::{
        clipboard_or_default_style, header_style, modified_date_style, name_style, size_style,
    },
    Clipboard,
};
use crate::{
    app::config::theme::Theme, file_system::path_info::PathInfo, utf8::split_with_ellipsis,
};
use chrono::{DateTime, Local};

pub(super) fn table_widget<'a>(
    theme: &Theme,
    column_constraints: Vec<Constraint>,
    rows: Vec<Row<'a>>,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Table<'a> {
    let header = header_widget(theme, sort_column, sort_direction);
    Table::new(rows, vec![Constraint::Percentage(100)])
        .header(header)
        .row_highlight_style(theme.table_selected())
        .style(theme.table_body())
        .widths(&column_constraints)
}

fn header_widget<'a>(
    theme: &Theme,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Row<'a> {
    let mut cells: Vec<_> = [SortColumn::Name, SortColumn::Modified, SortColumn::Size]
        .into_iter()
        .map(|header| {
            Cell::from(header_label(sort_column, sort_direction, &header)).style(header_style(
                theme,
                sort_column,
                &header,
            ))
        })
        .collect();
    cells.push(Cell::from("Mode").style(theme.table_header())); // Mode cannot be sorted
    Row::new(cells).style(theme.table_header())
}

fn header_label<'a>(
    sort_column: &SortColumn,
    sort_direction: &SortDirection,
    column: &SortColumn,
) -> Span<'a> {
    let is_sorted = sort_column == column;
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
    if is_sorted {
        text.bold()
    } else {
        Span::raw(text)
    }
}

pub(super) fn row_and_height<'a>(
    theme: &'a Theme,
    clipboard: &'a Clipboard,
    name_column_width: u16,
    item: &'a PathInfo,
    relative_to_datetime: DateTime<Local>,
) -> (Row<'a>, u16) {
    let name_style =
        clipboard_or_default_style(theme, clipboard, item, name_style(&theme.file_types, item));
    let size_style = clipboard_or_default_style(theme, clipboard, item, size_style(theme, item));
    let date_style = clipboard_or_default_style(
        theme,
        clipboard,
        item,
        modified_date_style(theme, item, relative_to_datetime),
    );
    let row_style = clipboard_or_default_style(theme, clipboard, item, theme.table_body());

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
        .map(|part| Line::from(part))
        .collect()
}

pub(super) fn scrollbar(theme: &Theme) -> Scrollbar<'_> {
    let mut scrollbar = Scrollbar::default()
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_style(theme.table_scrollbar_thumb())
        .thumb_symbol(block::FULL)
        .track_style(theme.table_scrollbar_track())
        .track_symbol(Some(line::VERTICAL));

    if theme.table_scrollbar_begin_end_enabled() {
        scrollbar = scrollbar
            .begin_symbol(Some("▲"))
            .begin_style(theme.table_scrollbar_begin())
            .end_symbol(Some("▼"))
            .end_style(theme.table_scrollbar_end());
    }

    scrollbar
}
