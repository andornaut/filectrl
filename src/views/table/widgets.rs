use ratatui::{
    prelude::{Alignment, Constraint, Stylize},
    symbols::{block, line},
    text::{Line, Span, Text},
    widgets::{Cell, Row, Scrollbar, ScrollbarOrientation, Table},
};

use super::{
    columns::{SortColumn, SortDirection},
    style::{header_style, name_style},
};
use crate::{
    app::config::theme::Theme, file_system::path_info::PathInfo, utf8::split_with_ellipsis,
};

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
    name_column_width: u16,
    item: &'a PathInfo,
) -> (Row<'a>, u16) {
    let name = split_name(theme, name_column_width, item);
    let height = name.len() as u16;
    let size = Line::from(item.size()).alignment(Alignment::Right);
    let row = row_widget(theme, item, name, size).height(height);
    (row, height)
}

fn row_widget<'a>(
    theme: &'a Theme,
    item: &'a PathInfo,
    name: Vec<Line<'a>>,
    size: Line<'a>,
) -> Row<'a> {
    let size = Cell::from(
        Line::styled(
            size.to_string(),
            match item.size_unit_index() {
                0 => theme.size_bytes(),
                1 => theme.size_kib(),
                2 => theme.size_mib(),
                3 => theme.size_gib(),
                4 => theme.size_tib(),
                5 => theme.size_pib(),
                _ => theme.size_pib(),
            },
        )
        .alignment(Alignment::Right),
    );
    Row::new([
        Cell::from(Text::from(name)),
        Cell::from(item.modified().unwrap_or_default()),
        size,
        Cell::from(item.mode()),
    ])
    .style(theme.table_body())
}

fn split_name<'a>(theme: &Theme, width: u16, path: &'a PathInfo) -> Vec<Line<'a>> {
    let style = name_style(&theme.file_types, path);
    split_with_ellipsis(&path.name(), width)
        .into_iter()
        .map(|part| Line::from(Span::styled(part, style)))
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
