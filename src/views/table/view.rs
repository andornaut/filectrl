use super::{
    column_constraints,
    sort::{SortColumn, SortDirection},
    style::{header_style, name_style},
    TableView,
};
use crate::{
    app::config::theme::Theme,
    command::mode::InputMode,
    file_system::human::HumanPath,
    views::{split_with_ellipsis, View},
};
use ratatui::{
    prelude::{Alignment, Backend, Constraint, Direction, Layout, Rect},
    symbols::scrollbar::VERTICAL,
    text::{Line, Span, Text},
    widgets::{Block, Cell, Row, Scrollbar, ScrollbarOrientation, Table},
    Frame,
};

impl<B: Backend> View<B> for TableView {
    fn render(&mut self, frame: &mut Frame, rect: Rect, _: &InputMode, theme: &Theme) {
        if rect.height < 2 || rect.width < 8 {
            return;
        }
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
            .split(rect);
        self.table_rect = chunks[0];
        self.scrollbar_rect = chunks[1];

        // Extend the table header above the scrollbar as a 1x1 block
        let block = Block::default().style(theme.table_header());
        frame.render_widget(
            block,
            Rect {
                height: 1,
                ..self.scrollbar_rect
            },
        );

        // Make room for the above
        self.scrollbar_rect.y += 1;
        self.scrollbar_rect.height -= 1;

        // Render the table
        let (column_constraints, name_column_width) = column_constraints(self.table_rect.width);
        self.name_column_width = name_column_width;
        self.table_visual_rows.clear();
        let rows = self
            .directory_items_sorted
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let (row, height) = row(item, name_column_width, theme);
                for _ in 0..height {
                    self.table_visual_rows.push(i);
                }
                row
            });
        let table_header = header(theme, &self.sort_column, &self.sort_direction);
        let table = Table::new(rows, vec![Constraint::Percentage(100)])
            .header(table_header)
            .row_highlight_style(theme.table_selected())
            .style(theme.table_body())
            .widths(&column_constraints);
        frame.render_stateful_widget(table, self.table_rect, &mut self.table_state);

        // Adjust row heights to account for overflow
        let window_min = self.position(self.table_state.offset());
        let window_max = self.window_max(window_min);
        // Skip the first element, because if it overflows, then the window is empty anyway
        for item_index in 1..self.directory_items_sorted.len() {
            let item_min = self
                .table_visual_rows
                .iter()
                .position(|&i| i == item_index)
                .unwrap();
            let item_height = self
                .table_visual_rows
                .iter()
                .filter(|&&i| i == item_index)
                .count();
            let item_max = item_min + item_height - 1;
            if item_min < window_min {
                continue;
            }
            if item_min > window_max {
                break;
            }
            if item_min <= window_max && item_max > window_max {
                let overflow = window_max - item_min + 1;
                if overflow > 0 {
                    for _ in 0..overflow {
                        self.table_visual_rows.insert(item_min - 1, item_index - 1);
                    }
                }
            }
        }

        // Render the scrollbar
        let visual_content_length = self.table_visual_rows.len() as u16;
        if visual_content_length > self.scrollbar_rect.height {
            let selected_visual_index = self
                .table_state
                .selected()
                .map(|item_index| self.position(item_index))
                .unwrap_or_default();

            self.scrollbar_state = self
                .scrollbar_state
                .content_length(visual_content_length.into())
                .position(selected_visual_index.into());
            frame.render_stateful_widget(
                scrollbar(theme),
                self.scrollbar_rect,
                &mut self.scrollbar_state,
            );
        }
    }
}

fn scrollbar(theme: &Theme) -> Scrollbar<'_> {
    Scrollbar::default()
        .begin_symbol(None)
        .end_symbol(None)
        .thumb_style(theme.table_scrollbar_thumb())
        .track_style(theme.table_scrollbar_track())
        .orientation(ScrollbarOrientation::VerticalRight)
        .symbols(VERTICAL)
}

fn header_label(
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

fn row<'a>(item: &'a HumanPath, name_column_width: u16, theme: &Theme) -> (Row<'a>, u16) {
    let name = split_name(&item, name_column_width, theme);
    let size = Line::from(item.size()).alignment(Alignment::Right);
    let height = name.len() as u16;
    let row = Row::new(vec![
        Cell::from(Text::from(name)),
        Cell::from(item.modified().unwrap_or_default()),
        Cell::from(size),
        Cell::from(item.mode()),
    ]);
    let row = row.height(height);
    (row, height)
}

fn split_name<'a>(path: &'a HumanPath, width: u16, theme: &Theme) -> Vec<Line<'a>> {
    let style = name_style(path, &theme.files);
    split_with_ellipsis(&path.name(), width)
        .into_iter()
        .map(|part| Line::from(Span::styled(part, style)))
        .collect()
}
