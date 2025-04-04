use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, StatefulWidget, Widget},
};

use super::{
    line_item_map::LineItemMap,
    widgets::{row_and_height, scrollbar, table_widget},
    TableView,
};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};

impl View for TableView {
    fn render(&mut self, area: Rect, buf: &mut Buffer, _: &InputMode, theme: &Theme) {
        if area.height < 2 || area.width < 8 {
            return;
        }

        let (block_area, scrollbar_area, table_area) = layout(area);
        self.table_area = table_area;
        self.scrollbar_area = scrollbar_area;

        // We must render the table first to initialize the mapper, which is used by the scrollbar
        self.render_table_and_init_mapper(buf, theme);
        // Must be rendered after the table, because it depends on the mapper
        self.render_scrollbar(buf, theme);
        self.render_1x1_block(buf, theme, block_area);
    }
}

impl TableView {
    fn render_1x1_block(&mut self, buf: &mut Buffer, theme: &Theme, area: Rect) {
        // Extend the table header above the scrollbar as a 1x1 block
        let block = Block::default().style(theme.table_header());
        block.render(Rect { height: 1, ..area }, buf);
    }

    fn render_scrollbar(&mut self, buf: &mut Buffer, theme: &Theme) {
        // Render the scrollbar
        let total_number_of_lines = self.mapper.total_number_of_lines();
        let has_scroll = total_number_of_lines > self.scrollbar_area.height as usize;
        if !has_scroll {
            return;
        }

        let selected_line = self
            .table_state
            .selected()
            .map_or(0, |item_index| self.mapper.first_line(item_index));

        self.scrollbar_state = self
            .scrollbar_state
            .content_length(total_number_of_lines)
            .position(selected_line);
        let scrollbar = scrollbar(theme);
        StatefulWidget::render(
            scrollbar,
            self.scrollbar_area,
            buf,
            &mut self.scrollbar_state,
        );
    }

    fn render_table_and_init_mapper(&mut self, buf: &mut Buffer, theme: &Theme) {
        let column_constraints = self.columns.constraints(self.table_area.width);
        let (rows, item_heights): (Vec<_>, Vec<_>) = self
            .directory_items_sorted
            .iter()
            .map(|item| {
                let (row, height) = row_and_height(theme, self.columns.name_width(), item);
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
        StatefulWidget::render(table, self.table_area, buf, &mut self.table_state);

        // -1 for table header
        let number_of_visible_lines = self.table_area.height as usize - 1;
        // Must occur after rendering the table, because that's when `self.table_state.offset` is updated.
        let first_visible_item = self.table_state.offset();
        self.mapper = LineItemMap::new(item_heights, number_of_visible_lines, first_visible_item);
    }
}

fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
        .split(area);
    let table_area = chunks[0];
    let mut scrollbar_area = chunks[1];
    // Make room for the 1x1 block
    let block_area = Rect {
        height: 1,
        ..scrollbar_area
    };
    scrollbar_area.y += 1;
    scrollbar_area.height -= 1;
    (block_area, scrollbar_area, table_area)
}
