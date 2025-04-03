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
use crate::{app::config::theme::Theme, file_system::human::HumanPath, utf8::split_with_ellipsis};

pub(super) fn table<'a>(
    theme: &Theme,
    column_constraints: Vec<Constraint>,
    rows: Vec<Row<'a>>,
    sort_column: &'a SortColumn,
    sort_direction: &'a SortDirection,
) -> Table<'a> {
    let header = header(theme, sort_column, sort_direction);
    Table::new(rows, vec![Constraint::Percentage(100)])
        .header(header)
        .row_highlight_style(theme.table_selected())
        .style(theme.table_body())
        .widths(&column_constraints)
}

fn header<'a>(
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

pub(super) fn row<'a>(
    theme: &Theme,
    name_column_width: u16,
    item: &'a HumanPath,
) -> (Row<'a>, u16) {
    let name = split_name(theme, name_column_width, item);
    let height = name.len() as u16;
    let size = Line::from(item.size()).alignment(Alignment::Right);
    let row = Row::new(vec![
        Cell::from(Text::from(name)),
        Cell::from(item.modified().unwrap_or_default()),
        Cell::from(size),
        Cell::from(item.mode()),
    ])
    .height(height);
    (row, height)
}

fn split_name<'a>(theme: &Theme, width: u16, path: &'a HumanPath) -> Vec<Line<'a>> {
    let style = name_style(&theme.files, path);
    split_with_ellipsis(&path.name(), width)
        .into_iter()
        .map(|part| Line::from(Span::styled(part, style)))
        .collect()
}

pub(super) fn scrollbar(theme: &Theme) -> Scrollbar<'_> {
    Scrollbar::default()
        .thumb_style(theme.table_scrollbar_thumb())
        .track_style(theme.table_scrollbar_track())
        .orientation(ScrollbarOrientation::VerticalRight)
        .begin_symbol(None)
        .begin_style(theme.table_scrollbar_begin())
        .end_symbol(None)
        .end_style(theme.table_scrollbar_end())
        .thumb_symbol(block::FULL)
        .track_symbol(Some(line::VERTICAL))
}
