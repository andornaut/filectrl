use ratatui::crossterm::event::MouseEvent;

use super::TableView;
use crate::command::result::CommandResult;

impl TableView {
    pub(super) fn click_header(&mut self, x: u16) -> CommandResult {
        self.columns
            .sort_column_for_click(x)
            .map_or(CommandResult::Handled, |column| self.sort_by(column))
    }

    pub(super) fn click_table(&mut self, y: u16) -> CommandResult {
        let y = y as usize - 1; // -1 for the header
        let line = self.mapper.first_visible_line() + y;
        if line >= self.mapper.total_lines_count() {
            // Clicked past the table
            return CommandResult::Handled;
        }

        let item = self.mapper.item(line);
        let Some(path) = self.content.get(item) else {
            return CommandResult::Handled;
        };
        if self.double_click.click_and_is_double_click(path) {
            return self.open_selected();
        }

        self.select(item)
    }

    pub(super) fn handle_scroll(&mut self, event: MouseEvent) -> CommandResult {
        // Use the same scale as the rendered thumb (line offset over
        // `total - visible`, see `render_scrollbar`). The dragged-to line
        // becomes the top of the window, snapped forward across wrapped rows
        // so the track bottom always reaches the bottom-most window; the
        // thumb meanwhile renders at `drag_line` so it stays on the cursor.
        let max_position = self
            .mapper
            .total_lines_count()
            .saturating_sub(self.mapper.visible_lines_count());
        let result = self
            .scrollbar_view
            .handle_mouse(event, max_position)
            .map_or(CommandResult::Handled, |line| {
                self.drag_line = Some(line);
                let item = self.mapper.snap_to_item_start(line);
                self.first_visible_item = item;
                self.select(item)
            });
        if !self.scrollbar_view.is_dragging() {
            self.drag_line = None;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use ratatui::{
        crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
        layout::Rect,
    };

    use super::super::{TableView, columns::SortDirection, marked_table, row_map::LineItemMap};
    use crate::command::{handler::CommandHandler, result::CommandResult};

    /// A three row listing (`a`, `b`, `c`) laid out the way a render would: the
    /// header on row 0 and one line per item below it, in a viewport with room
    /// to spare so the rows past the end are still inside the table area.
    fn table_for_clicks() -> (crate::test_support::TempDir, TableView) {
        let (dir, mut table) = marked_table();
        table.clear_marks();
        table.table_area = Rect {
            x: 0,
            y: 0,
            width: 80,
            height: 10,
        };
        table.mapper = LineItemMap::new(&[1, 1, 1], 9, 0);
        (dir, table)
    }

    fn click(table: &mut TableView, row: u16) -> CommandResult {
        table.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    fn selected(table: &TableView) -> Option<String> {
        table.selected_path().map(|p| p.display_name.clone())
    }

    #[test]
    fn a_click_on_a_row_moves_the_cursor_to_it() {
        let (_dir, mut table) = table_for_clicks();
        table.select(2);

        // Row 1 is the first entry: row 0 is the header.
        click(&mut table, 1);

        assert_eq!(Some("a".to_string()), selected(&table));
    }

    #[test]
    fn a_click_below_the_last_row_leaves_the_cursor_alone() {
        let (_dir, mut table) = table_for_clicks();
        table.select(1);

        // Inside the table area but past the last entry. Moving the cursor
        // here would be a selection the user never aimed at.
        let result = click(&mut table, 8);

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn a_click_on_the_header_sorts_instead_of_selecting() {
        let (_dir, mut table) = table_for_clicks();
        table.select(1);

        // Row 0 is the header, whatever the listing below it holds.
        click(&mut table, 0);

        assert_eq!(SortDirection::Descending, table.columns.sort_direction());
        // The sort carries the cursor with the entry it was on rather than
        // leaving it on the row number, which now holds a different entry.
        assert_eq!(Some("b".to_string()), selected(&table));
    }
}
