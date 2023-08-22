use ratatui::{
    prelude::Constraint,
    text::{Line, Span, Text},
    widgets::{Cell, Row},
};

use crate::{app::theme::Theme, file_system::human::HumanPath, views::split_utf8_with_reservation};

use super::{
    sort::{SortColumn, SortDirection},
    style::{header_style, name_style},
};

const NAME_MIN_LEN: u16 = 39;
const MODE_LEN: u16 = 10;
const MODIFIED_LEN: u16 = 12;
const SIZE_LEN: u16 = 7;
const LINE_SEPARATOR: &str = "\n…";

pub(super) fn constraints(width: u16) -> (Vec<Constraint>, u16) {
    let mut constraints = Vec::new();
    let mut name_column_width = width;
    let mut len = NAME_MIN_LEN;
    if width > len {
        name_column_width = width - MODIFIED_LEN - 1; // 1 for the cell padding
        constraints.push(Constraint::Length(MODIFIED_LEN));
    }
    len += MODIFIED_LEN + 1 + SIZE_LEN + 1;
    if width > len {
        name_column_width -= SIZE_LEN + 1;
        constraints.push(Constraint::Length(SIZE_LEN));
    }
    len += MODE_LEN + 1;
    if width > len {
        name_column_width -= MODE_LEN + 1;
        constraints.push(Constraint::Length(MODE_LEN));
    }
    constraints.insert(0, Constraint::Length(name_column_width));
    (constraints, name_column_width)
}

pub(super) fn header_label(
    sort_column: &SortColumn,
    sort_direction: &SortDirection,
    column: &SortColumn,
) -> String {
    let label = match column {
        SortColumn::Name => "[N]ame",
        SortColumn::Modified => "[M]odified",
        SortColumn::Size => "[S]ize",
    };
    if sort_column != column {
        return label.into();
    }
    match sort_direction {
        SortDirection::Ascending => format!("{label}⌃"),
        SortDirection::Descending => format!("{label}⌄"),
    }
}

pub(super) fn header<'a>(
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
    cells.push(Cell::from("Mode").style(theme.table_header())); // Mode cannot be sorted/active
    Row::new(cells).style(theme.table_header())
}

pub(super) fn row<'a>(item: &'a HumanPath, name_column_width: u16, theme: &Theme) -> Row<'a> {
    let lines = split_name(&item, name_column_width, theme);
    let len = lines.len();

    // 7 must match SIZE_LEN
    let size = format!("{: >7}", item.size());
    Row::new(vec![
        Cell::from(Text::from(lines)),
        Cell::from(item.modified()),
        Cell::from(size),
        Cell::from(item.mode()),
    ])
    .height(len as u16)
}

fn split_name<'a>(path: &HumanPath, width: u16, theme: &Theme) -> Vec<Line<'a>> {
    let line = path.name();
    let split = split_utf8_with_reservation(&line, width, LINE_SEPARATOR);
    let mut lines = Vec::new();
    let mut it = split.into_iter().peekable();
    while let Some(part) = it.next() {
        let is_last = it.peek().is_none();
        let part = if is_last { part.clone() } else { part + "…" };
        lines.push(Line::from(Span::styled(part, name_style(path, theme))));
    }
    lines
}
