use chrono::Local;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, StatefulWidget, Widget},
    Frame,
};

use super::{
    row_map::LineItemMap,
    widgets::{row_widget_and_height, table_widget},
    TableView,
};
use crate::{app::{config::Config, AppState}, views::View};

const MIN_HEIGHT: u16 = 3; // header + 1 data row + scrollbar
const MIN_WIDTH: u16 = 8;

impl View for TableView {
    fn constraint(&self, _: Rect, _: &AppState) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, state: &AppState) {
        if area.height < MIN_HEIGHT || area.width < MIN_WIDTH {
            return;
        }

        let (block_area, scrollbar_area, table_area) = layout(area);

        // We must render the table first to initialize the mapper, which is used by the scrollbar
        self.render_table_and_init_mapper(table_area, frame.buffer_mut(), state);
        // Must be rendered after render_table_and_init_mapper, because it depends on the mapper
        self.render_scrollbar(scrollbar_area, frame.buffer_mut());
        self.render_1x1_block(block_area, frame.buffer_mut());
    }
}

impl TableView {
    fn render_1x1_block(&self, area: Rect, buf: &mut Buffer) {
        let theme = Config::global().theme();
        // Extend the table header above the scrollbar as a 1x1 block
        let block = Block::default().style(theme.table.header());
        block.render(Rect { height: 1, ..area }, buf);
    }

    fn render_scrollbar(&mut self, area: Rect, buf: &mut Buffer) {
        let total = self.mapper.total_lines_count();
        let visible = self.mapper.visible_lines_count();
        let max_position = total.saturating_sub(visible);
        self.scrollbar_view.render(
            area,
            buf,
            self.mapper.first_visible_line(),
            max_position,
            visible,
        );
    }

    fn render_table_and_init_mapper(&mut self, area: Rect, buf: &mut Buffer, state: &AppState) {
        let theme = Config::global().theme();
        self.table_area = area;

        let column_constraints = self.columns.constraints(self.table_area.width);
        let relative_to_datetime = Local::now();

        let has_pending_delete = !self.pending_delete.is_empty();
        let (rows, item_heights): (Vec<_>, Vec<usize>) = self
            .content
            .items_sorted()
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let is_pending_delete =
                    has_pending_delete && self.pending_delete.iter().any(|p| p == item);
                let (row, height) = row_widget_and_height(
                    theme,
                    &state.clipboard_entry,
                    self.columns.name_width(),
                    relative_to_datetime,
                    item,
                    self.marks.contains(&i),
                    is_pending_delete,
                );
                (row, height as usize)
            })
            .unzip();

        let table = table_widget(
            theme,
            column_constraints,
            rows,
            self.columns.sort_column(),
            self.columns.sort_direction(),
        );
        StatefulWidget::render(table, area, buf, &mut self.table_state);

        // -1 for table header
        let visible_lines_count = self.table_area.height as usize - 1;
        // Must occur after rendering the table, because that's when `self.table_state.offset` is updated.
        let first_visible_item = self.table_state.offset();
        self.mapper = LineItemMap::new(item_heights, visible_lines_count, first_visible_item);
    }
}

fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let [table_area, mut scrollbar_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
        .areas(area);
    // Make room for the 1x1 block
    let block_area = Rect {
        height: 1,
        ..scrollbar_area
    };
    scrollbar_area.y += 1;
    scrollbar_area.height -= 1;
    (block_area, scrollbar_area, table_area)
}
