use chrono::Local;
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, StatefulWidget, Widget},
    Frame,
};

use super::{
    line_item_map::LineItemMap,
    widgets::{row_widget_and_height, table_widget},
    TableView,
};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

const MIN_HEIGHT: u16 = 3;

impl View for TableView {
    fn constraint(&self, _: Rect, _: &InputMode) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>, _: &InputMode, theme: &Theme) {
        if area.height < MIN_HEIGHT || area.width < 8 {
            return;
        }

        let (block_area, scrollbar_area, table_area) = layout(area);

        // We must render the table first to initialize the mapper, which is used by the scrollbar
        self.render_table_and_init_mapper(table_area, frame.buffer_mut(), theme);
        // Must be rendered after render_table_and_init_mapper, because it depends on the mapper
        self.render_scrollbar(scrollbar_area, frame.buffer_mut(), theme);
        self.render_1x1_block(block_area, frame.buffer_mut(), theme);
    }
}

impl TableView {
    fn render_1x1_block(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        // Extend the table header above the scrollbar as a 1x1 block
        let block = Block::default().style(theme.table_header());
        block.render(Rect { height: 1, ..area }, buf);
    }

    fn render_scrollbar(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let selected_line = self
            .table_state
            .selected()
            .map_or(0, |item_index| self.mapper.first_line(item_index));
        let total_lines_count = self.mapper.total_lines_count();

        self.scrollbar_view
            .render(area, buf, theme, selected_line, total_lines_count);
    }

    fn render_table_and_init_mapper(&mut self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        self.table_area = area;

        let column_constraints = self.columns.constraints(self.table_area.width);
        let relative_to_datetime = Local::now();
        let clipboard_command = self.clipboard.get_clipboard_command();

        let (rows, item_heights): (Vec<_>, Vec<_>) = self
            .directory_items_sorted
            .iter()
            .map(|item| {
                let (row, height) = row_widget_and_height(
                    theme,
                    &clipboard_command,
                    self.columns.name_width(),
                    relative_to_datetime,
                    item,
                );
                (row, height)
            })
            .unzip();

        let table = table_widget(
            theme,
            column_constraints,
            rows,
            self.columns.sort_column(),
            self.columns.sort_direction(),
        );
        let table = table.style(theme.table_body());
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
