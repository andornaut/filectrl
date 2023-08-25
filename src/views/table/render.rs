use ratatui::{
    prelude::Alignment,
    symbols::scrollbar::VERTICAL,
    text::{Line, Span, Text},
    widgets::{Cell, Row, Scrollbar, ScrollbarOrientation},
};

use crate::{app::theme::Theme, file_system::human::HumanPath, views::split_utf8_with_reservation};

use super::{
    sort::{SortColumn, SortDirection},
    style::{header_style, name_style},
};

const LINE_SEPARATOR: &str = "\n…";

pub(super) fn scrollbar(theme: &Theme) -> Scrollbar<'_> {
    Scrollbar::default()
        .begin_style(theme.table_scrollbar_begin())
        .begin_symbol(None) // TODO remove
        .end_style(theme.table_scrollbar_end())
        .end_symbol(None) // TODO remove
        .thumb_style(theme.table_scrollbar_thumb())
        .track_style(theme.table_scrollbar_track())
        .orientation(ScrollbarOrientation::VerticalRight)
        .symbols(VERTICAL)
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
    cells.push(Cell::from("Mode").style(theme.table_header())); // Mode cannot be sorted
    Row::new(cells).style(theme.table_header())
}

pub(super) fn row<'a>(
    item: &'a HumanPath,
    name_column_width: u16,
    theme: &Theme,
) -> (Row<'a>, u16) {
    let name = split_name(&item, name_column_width, theme);
    let size = Line::from(item.size()).alignment(Alignment::Right);
    let height = name.len() as u16;
    let row = Row::new(vec![
        Cell::from(Text::from(name)),
        Cell::from(item.modified().unwrap_or_default()),
        Cell::from(size),
        Cell::from(item.mode()),
    ]);
    (row.height(height), height)
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
