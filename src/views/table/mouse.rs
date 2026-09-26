use ratatui::crossterm::event::MouseEvent;

use super::{TableView, view::highest_start_keeping_visible};
use crate::command::{Command, result::CommandResult};

/// Rows one notch of the wheel scrolls the window by.
const WHEEL_ROWS: usize = 3;

impl TableView {
    /// Scrolls the window up by [`WHEEL_ROWS`], leaving the cursor where it is.
    pub(super) fn scroll_window_up(&mut self) -> CommandResult {
        self.first_visible_item = self.first_visible_item.saturating_sub(WHEEL_ROWS);
        self.wheel_scrolled = true;
        CommandResult::Handled
    }

    /// Scrolls the window down by [`WHEEL_ROWS`], leaving the cursor where it
    /// is.
    pub(super) fn scroll_window_down(&mut self) -> CommandResult {
        let heights = &self.cached_heights;
        let last_window = match heights.len() {
            0 => 0,
            n => highest_start_keeping_visible(heights, n - 1, self.mapper.visible_lines_count()),
        };
        self.first_visible_item = (self.first_visible_item + WHEEL_ROWS).min(last_window);
        self.wheel_scrolled = true;
        CommandResult::Handled
    }

    pub(super) fn click_header(&mut self, x: u16) -> CommandResult {
        self.columns
            .sort_column_for_click(x)
            .map_or(CommandResult::Handled, |column| self.sort_by(column))
    }

    pub(super) fn click_table(&mut self, y: u16) -> CommandResult {
        let y = y as usize - 1; // -1 for the header
        let line = self.mapper.first_visible_line() + y;
        if line >= self.mapper.total_lines_count() {
            return CommandResult::Handled;
        }

        let item = self.mapper.item(line);
        let Some(path) = self.content.get(item) else {
            return CommandResult::Handled;
        };
        // Open the entry clicked, not the cursor's, which may have moved.
        if self.double_click.click_and_is_double_click(path) {
            return Command::Open(path.clone()).into();
        }

        self.select(item)
    }

    pub(super) fn handle_scroll(&mut self, event: MouseEvent) -> CommandResult {
        // Same scale as the rendered thumb (see `render_scrollbar`). The window
        // top snaps forward across wrapped rows; the thumb renders at `drag_line`.
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

    use super::super::{TableView, columns::SortDirection, marked_table, row_map::LineItemMap};
    use crate::command::{Command, handler::CommandHandler, result::CommandResult};

    /// A three row listing (`a`, `b`, `c`) laid out as a render would, starting
    /// below the top of the screen.
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
    fn a_click_after_a_frame_too_small_for_the_table_selects_nothing() {
        use ratatui::{Terminal, backend::TestBackend};

        use crate::{app::config::Config, views::View};

        let (_dir, mut table) = table_for_clicks();
        let before = selected(&table);
        let mut terminal = Terminal::new(TestBackend::new(80, 2)).unwrap();
        terminal
            .draw(|frame| table.render(Config::global().theme(), frame.area(), frame))
            .unwrap();

        assert_eq!(CommandResult::NotHandled, {
            let event = mouse(MouseEventKind::Down(MouseButton::Left), 1, 4);
            if table.should_handle_mouse(event) {
                table.handle_mouse(event)
            } else {
                CommandResult::NotHandled
            }
        });
        assert_eq!(before, selected(&table));
    }

    #[test]
    fn a_click_on_a_row_moves_the_cursor_to_it() {
        let (_dir, mut table) = table_for_clicks();
        table.select(2);

        click(&mut table, 1);

        assert_eq!(Some("a".to_string()), selected(&table));
    }

    /// The rows `a`, `b`, `c` in a one-line viewport, with the cursor on `a`.
    fn table_for_the_wheel() -> (crate::test_support::TempDir, TableView) {
        let (dir, mut table) = table_for_clicks();
        table.cached_heights = vec![1, 1, 1];
        table.mapper = LineItemMap::new(&table.cached_heights, 1, 0);
        table.select(0);
        (dir, table)
    }

    #[test]
    fn the_wheel_scrolls_the_window_and_leaves_the_cursor() {
        let (_dir, mut table) = table_for_the_wheel();

        table.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 5));

        assert_eq!(2, table.first_visible_item);
        assert_eq!(Some("a".to_string()), selected(&table));
        assert!(table.wheel_scrolled);

        table.handle_mouse(mouse(MouseEventKind::ScrollUp, 1, 5));

        assert_eq!(0, table.first_visible_item);
        assert_eq!(Some("a".to_string()), selected(&table));
    }

    #[test]
    fn the_wheel_in_range_mode_marks_nothing() {
        let (_dir, mut table) = table_for_the_wheel();
        table.enter_range_mode();
        let marks = table.marks.len();

        table.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 5));

        assert_eq!(marks, table.marks.len());
    }

    #[test]
    fn a_key_brings_the_cursor_back_into_view_and_a_reload_does_not() {
        let (_dir, mut table) = table_for_the_wheel();
        table.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 5));

        table.select(0);
        assert!(table.wheel_scrolled);

        table.handle_key(ratatui::crossterm::event::KeyCode::F(5), KeyModifiers::NONE);
        assert!(!table.wheel_scrolled);
        assert_eq!(Some("a".to_string()), selected(&table));
    }

    #[test]
    fn page_down_after_the_wheel_pages_from_the_cursor() {
        let (_dir, mut table) = table_for_the_wheel();
        table.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 5));
        table.mapper.set_window(table.first_visible_item, 1);

        table.next_page();

        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn page_up_after_the_wheel_pages_from_the_cursor() {
        let (_dir, mut table) = table_for_the_wheel();
        table.select(2);
        table.first_visible_item = 2;
        table.handle_mouse(mouse(MouseEventKind::ScrollUp, 1, 5));
        table.mapper.set_window(table.first_visible_item, 1);

        table.previous_page();

        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn moving_the_cursor_shows_it() {
        let (_dir, mut table) = table_for_the_wheel();
        table.handle_mouse(mouse(MouseEventKind::ScrollDown, 1, 5));

        table.select(1);

        assert!(!table.wheel_scrolled);
    }

    /// Lines `a`, `b` x3, `c` in a two-line viewport: three thumb positions.
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

        table.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 80, top + 1));
        assert_eq!(Some("b".to_string()), selected(&table));
        assert_eq!(1, table.first_visible_item);
        assert_eq!(Some(1), table.drag_line);

        // Line 2 is inside `b`'s wrapped row, which snaps forward.
        table.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 80, top + 2));
        assert_eq!(Some("c".to_string()), selected(&table));
        assert_eq!(2, table.first_visible_item);

        let release = mouse(MouseEventKind::Up(MouseButton::Left), 200, 50);
        assert!(table.should_handle_mouse(release));
        table.handle_mouse(release);
        assert_eq!(None, table.drag_line);
    }

    #[test]
    fn two_clicks_on_one_row_open_it() {
        let (_dir, mut table) = table_for_clicks();

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

        // The first row past the last entry.
        let result = click(&mut table, 4);

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn a_click_on_the_header_sorts_instead_of_selecting() {
        let (_dir, mut table) = table_for_clicks();
        table.select(1);

        click(&mut table, 0);

        assert_eq!(SortDirection::Descending, table.columns.sort_direction());
        assert_eq!(Some("b".to_string()), selected(&table));
    }

    /// The release of a scrollbar drag can go to another view, so the next
    /// press elsewhere must not be taken as a header click.
    #[test]
    fn a_press_above_the_table_after_a_lost_release_ends_the_drag_without_sorting() {
        let (_dir, mut table) = table_for_clicks();
        table.scrollbar_view.begin_drag();
        let direction = table.columns.sort_direction();

        let press = mouse(MouseEventKind::Down(MouseButton::Left), 1, 0);
        assert!(table.should_handle_mouse(press));
        table.handle_mouse(press);

        assert!(!table.scrollbar_view.is_dragging());
        assert_eq!(direction, table.columns.sort_direction());
        assert!(!table.should_handle_mouse(press));
    }
}
