use super::{
    column_constraints,
    line_to_item::LineToItemMapper,
    widgets::{row, scrollbar, table},
    TableView,
};
use crate::{app::config::theme::Theme, command::mode::InputMode, views::View};
use ratatui::{
    prelude::{Backend, Constraint, Direction, Layout, Rect},
    widgets::Block,
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
        // Make room for the 1x1 block
        self.scrollbar_rect.y += 1;
        self.scrollbar_rect.height -= 1;

        let (column_constraints, name_column_width) = column_constraints(self.table_rect.width);
        self.name_column_width = name_column_width;

        // We must render the table first to initialize the mapper, which is used by the scrollbar
        self.render_table_and_init_mapper(frame, theme, column_constraints);
        // Must be rendered after the table, because it depends on the mapper
        self.render_scrollbar(frame, theme);
        self.render_1x1_block(frame, theme);
    }
}

impl TableView {
    fn render_1x1_block(&mut self, frame: &mut Frame<'_>, theme: &Theme) {
        // Extend the table header above the scrollbar as a 1x1 block
        let block = Block::default().style(theme.table_header());
        frame.render_widget(
            block,
            Rect {
                height: 1,
                ..self.scrollbar_rect
            },
        );
    }

    fn render_scrollbar(&mut self, frame: &mut Frame<'_>, theme: &Theme) {
        // Render the scrollbar
        let total_number_of_lines = self.mapper.total_number_of_lines();
        let has_scroll = total_number_of_lines > self.scrollbar_rect.height as usize;
        if !has_scroll {
            return;
        }

        let selected_line = self
            .table_state
            .selected()
            .map_or(0, |item_index| self.mapper.get_line(item_index));

        self.scrollbar_state = self
            .scrollbar_state
            .content_length(total_number_of_lines)
            .position(selected_line);
        frame.render_stateful_widget(
            scrollbar(theme),
            self.scrollbar_rect,
            &mut self.scrollbar_state,
        );
    }

    fn render_table_and_init_mapper(
        &mut self,
        frame: &mut Frame<'_>,
        theme: &Theme,
        column_constraints: Vec<Constraint>,
    ) {
        let (rows, item_heights): (Vec<_>, Vec<_>) = self
            .directory_items_sorted
            .iter()
            .map(|item| {
                let (row, height) = row(item, self.name_column_width, theme);
                (row, height)
            })
            .unzip();

        let table = table(
            theme,
            column_constraints,
            rows,
            &self.sort_column,
            &self.sort_direction,
        );
        frame.render_stateful_widget(table, self.table_rect, &mut self.table_state);

        // -1 for table header
        let number_of_visible_lines = self.table_rect.height as usize - 1;
        // Must occur after rendering the table, because that's when the offset is updated.
        self.mapper = LineToItemMapper::new(
            item_heights,
            number_of_visible_lines,
            self.table_state.offset(),
        );
    }
}
