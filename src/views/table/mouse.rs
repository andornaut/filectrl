use ratatui::crossterm::event::MouseEvent;

use super::TableView;
use crate::command::{Command, result::CommandResult};

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
        // Open the entry clicked, not the cursor's: a key or a reload between
        // the two clicks can move the cursor off the row being double-clicked.
        if self.double_click.click_and_is_double_click(path) {
            return Command::Open(path.clone()).into();
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
        buffer::Buffer,
        crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
        layout::Rect,
    };
    use test_case::test_case;

    use super::super::{TableView, columns::SortDirection, marked_table, row_map::LineItemMap};
    use crate::command::{Command, handler::CommandHandler, result::CommandResult};

    /// A three row listing (`a`, `b`, `c`) laid out the way a render would: the
    /// header on the table's first row and one line per item below it, in a
    /// viewport with room to spare so the rows past the end are still inside
    /// the table area. The table starts below the top of the screen, as it does
    /// under the breadcrumbs, so a click's row has to be made table-relative.
    fn table_for_clicks() -> (crate::test_support::TempDir, TableView) {
        let (dir, mut table) = marked_table();
        table.clear_marks();
        table.table_area = Rect {
            x: 0,
            y: 2,
            width: 80,
            height: 10,
        };
        table.mapper = LineItemMap::new(&[1, 1, 1], 9, 0);
        (dir, table)
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A left click on `row` of the table, counted from its header row.
    fn click(table: &mut TableView, row: u16) -> CommandResult {
        let row = table.table_area.y + row;
        table.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 1, row))
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

    #[test_case(MouseEventKind::ScrollUp, "a" ; "up moves the cursor up")]
    #[test_case(MouseEventKind::ScrollDown, "c" ; "down moves the cursor down")]
    fn the_scroll_wheel_moves_the_cursor(kind: MouseEventKind, expected: &str) {
        let (_dir, mut table) = table_for_clicks();
        table.select(1);

        table.handle_mouse(mouse(kind, 1, 5));

        assert_eq!(Some(expected.to_string()), selected(&table));
    }

    /// Row `b` wraps to three lines, so the lines are `a`, `b` x3, `c`, and a
    /// two-line viewport leaves three positions for the thumb. The scrollbar is
    /// four rows tall, one per position, in the column right of the table.
    #[test]
    fn dragging_the_scrollbar_moves_the_window_top_and_the_cursor_together() {
        let (_dir, mut table) = table_for_clicks();
        table.mapper = LineItemMap::new(&[1, 3, 1], 2, 0);
        let scrollbar = Rect::new(80, table.table_area.y + 1, 1, 4);
        let mut buf = Buffer::empty(Rect::new(0, 0, 81, 20));
        table.scrollbar_view.render(
            crate::app::config::Config::global().theme(),
            scrollbar,
            &mut buf,
            0,
            3,
            2,
        );
        let top = scrollbar.y;

        // The second track row is line 1, the first line of `b`.
        table.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 80, top + 1));
        assert_eq!(Some("b".to_string()), selected(&table));
        assert_eq!(1, table.first_visible_item);
        assert_eq!(Some(1), table.drag_line);

        // The third is line 2, inside `b`'s wrapped row, which snaps forward to
        // the next row so the last window is reachable.
        table.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 80, top + 2));
        assert_eq!(Some("c".to_string()), selected(&table));
        assert_eq!(2, table.first_visible_item);

        // A release off the table still reaches it, or the drag would never
        // end and the thumb would stay pinned to the last dragged line.
        let release = mouse(MouseEventKind::Up(MouseButton::Left), 200, 50);
        assert!(table.should_handle_mouse(release));
        table.handle_mouse(release);
        assert_eq!(None, table.drag_line);
    }

    #[test]
    fn two_clicks_on_one_row_open_it() {
        let (_dir, mut table) = table_for_clicks();

        // Back to back, so they fall inside the configured double-click
        // window, which the table is built with rather than reading when the
        // click arrives.
        click(&mut table, 1);
        let result = click(&mut table, 1);

        let Ok(Command::Open(path)) = Command::try_from(result) else {
            panic!("expected the row to open");
        };
        assert_eq!("a", path.display_name);
    }

    #[test]
    fn a_double_click_opens_the_clicked_row_when_the_cursor_moved_between_clicks() {
        let (_dir, mut table) = table_for_clicks();

        click(&mut table, 1);
        // A key press between the clicks moves the cursor off the row.
        table.select(2);
        let result = click(&mut table, 1);

        let Ok(Command::Open(path)) = Command::try_from(result) else {
            panic!("expected the row to open");
        };
        assert_eq!("a", path.display_name);
    }

    #[test]
    fn a_click_below_the_last_row_leaves_the_cursor_alone() {
        let (_dir, mut table) = table_for_clicks();
        table.select(1);

        // The first row past the last entry, which is the row the bound has
        // to exclude: one further out is past any off-by-one. Moving the
        // cursor here would be a selection the user never aimed at.
        let result = click(&mut table, 4);

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
