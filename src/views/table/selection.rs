use super::{TableView, scroll, view::visible_window};
use crate::views::ListingCount;
use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    /// Puts the cursor on `item`, clamped to the last row; an empty listing gets
    /// no cursor. The clamp covers a listing that shrank since the last render.
    pub(super) fn select(&mut self, item: usize) -> CommandResult {
        let len = self.content.len();
        let selected = (len != 0).then(|| item.min(len - 1));
        if selected != self.table_state.selected() {
            self.wheel_scrolled = false;
        }
        self.table_state.select(selected);
        self.update_range_marks();
        self.selection_snapshot()
    }

    /// What the table lists, counted for the status bar and the notices.
    pub(in crate::views) fn listing_count(&self) -> ListingCount {
        let shown = self.content.len();
        if self.content.is_searching() {
            ListingCount::Results {
                shown,
                total: self.content.total_len(),
            }
        } else if self.content.is_showing_bookmarks() {
            ListingCount::Bookmarks { shown }
        } else {
            ListingCount::Directory { shown }
        }
    }

    /// The current selection and mark count as a single snapshot command.
    pub(super) fn selection_snapshot(&self) -> CommandResult {
        self.selection_changed().into()
    }

    pub(super) fn selection_changed(&self) -> Command {
        Command::SelectionChanged {
            selected: self.selected_path().cloned(),
            mark_count: self.marks.len(),
            range: self.marks.in_range_mode(),
        }
    }

    pub(super) fn select_next(&mut self) -> CommandResult {
        let last = self.content.len().saturating_sub(1);
        let next = self.table_state.selected().map_or(0, |i| (i + 1).min(last));
        self.select(next)
    }

    pub(super) fn select_previous(&mut self) -> CommandResult {
        let previous = self
            .table_state
            .selected()
            .map_or(0, |i| i.saturating_sub(1));
        self.select(previous)
    }

    pub(super) fn select_first(&mut self) -> CommandResult {
        self.select(0)
    }

    pub(super) fn select_last(&mut self) -> CommandResult {
        self.select(self.content.len().saturating_sub(1))
    }

    pub(super) fn select_middle_item(&mut self) -> CommandResult {
        self.select(self.content.len().saturating_sub(1) / 2)
    }

    pub(super) fn select_first_visible_item(&mut self) -> CommandResult {
        self.select(self.mapper.item(self.mapper.first_visible_line()))
    }

    pub(super) fn select_middle_visible_item(&mut self) -> CommandResult {
        self.select(self.mapper.item(self.mapper.middle_visible_line()))
    }

    pub(super) fn select_last_visible_item(&mut self) -> CommandResult {
        self.select(self.mapper.item(self.mapper.last_visible_line()))
    }

    /// Puts a window the wheel moved back around the cursor, as the next render
    /// would.
    fn window_around_cursor(&mut self) {
        if !self.wheel_scrolled {
            return;
        }
        let visible_lines_count = self.mapper.visible_lines_count();
        let (start, _) = visible_window(
            &self.cached_heights,
            visible_lines_count,
            self.table_state.selected().unwrap_or_default(),
            self.first_visible_item,
        );
        self.first_visible_item = start;
        self.mapper.set_window(start, visible_lines_count);
    }

    pub(super) fn next_page(&mut self) -> CommandResult {
        self.window_around_cursor();
        scroll::next_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.content.len(),
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    pub(super) fn previous_page(&mut self) -> CommandResult {
        self.window_around_cursor();
        scroll::previous_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.first_visible_item,
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    pub(super) fn selected_path(&self) -> Option<&PathInfo> {
        self.table_state
            .selected()
            .and_then(|i| self.content.get(i))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{TableView, display_names, marked_table, row_map::LineItemMap};

    fn marked(table: &TableView) -> Vec<String> {
        display_names(&table.marked_paths())
    }

    #[test]
    fn the_listing_count_says_what_the_table_lists() {
        use crate::command::{Command, handler::CommandHandler};
        use crate::views::ListingCount;

        let (dir, mut table) = marked_table();
        assert_eq!(ListingCount::Directory { shown: 3 }, table.listing_count());
        table.handle_command(&Command::FilterChanged("a".to_string()));
        assert_eq!(ListingCount::Directory { shown: 1 }, table.listing_count());

        let results = table.content.items_sorted().to_vec();
        table.content.start_search();
        table.content.append(&results);
        table.content.set_filter("b".to_string());
        table.content.sort(
            super::super::columns::SortColumn::Name,
            super::super::columns::SortDirection::Ascending,
        );
        assert_eq!(
            ListingCount::Results { shown: 0, total: 1 },
            table.listing_count()
        );

        table.content.set_bookmarks(vec![
            crate::file_system::path_info::PathInfo::try_from(dir.path()).unwrap(),
        ]);
        table.content.sort(
            super::super::columns::SortColumn::Name,
            super::super::columns::SortDirection::Ascending,
        );
        assert_eq!(ListingCount::Bookmarks { shown: 1 }, table.listing_count());
    }

    fn selected(table: &TableView) -> Option<String> {
        table.selected_path().map(|p| p.display_name.clone())
    }

    /// The listing is `a`, `b`, `c`; `marked_table` leaves the cursor on `c`.
    fn table() -> (crate::test_support::TempDir, TableView) {
        let (dir, mut table) = marked_table();
        table.clear_marks();
        (dir, table)
    }

    /// A four entry listing, so `(len - 1) / 2` and `len / 2` differ.
    fn even_table() -> (crate::test_support::TempDir, TableView) {
        use crate::file_system::path_info::PathInfo;

        let (dir, mut table) = table();
        std::fs::write(dir.join("d"), b"x").unwrap();
        let items: Vec<PathInfo> = ["a", "b", "c", "d"]
            .iter()
            .map(|name| PathInfo::try_from(dir.join(name).as_path()).unwrap())
            .collect();
        table
            .content
            .set_items(PathInfo::try_from(dir.path()).unwrap(), items);
        table.sort();
        (dir, table)
    }

    #[test]
    fn the_middle_of_an_even_listing_is_the_upper_of_the_two_centre_rows() {
        let (_dir, mut table) = even_table();

        table.select_middle_item();

        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn the_last_item_is_the_final_row_rather_than_one_past_it() {
        let (_dir, mut table) = even_table();

        table.select_last();

        assert_eq!(Some("d".to_string()), selected(&table));
    }

    #[test]
    fn the_visible_row_keys_land_on_the_rows_the_window_shows() {
        let (_dir, mut table) = even_table();
        table.mapper = LineItemMap::new(&[1; 4], 4, 0);

        table.select_first_visible_item();
        let first = selected(&table);
        table.select_middle_visible_item();
        let middle = selected(&table);
        table.select_last_visible_item();
        let last = selected(&table);

        assert_eq!(
            [Some("a"), Some("b"), Some("d")].map(|name| name.map(String::from)),
            [first, middle, last]
        );
    }

    #[test]
    fn page_up_lands_on_the_top_of_a_scrolled_window() {
        let (_dir, mut table) = table();
        table.first_visible_item = 1;
        table.mapper = LineItemMap::new(&[1; 3], 2, 1);
        table.select(2);

        table.previous_page();

        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn a_row_past_the_end_puts_the_cursor_on_the_last_row() {
        let (_dir, mut table) = table();

        table.select(7);

        assert_eq!(Some(2), table.table_state.selected());
        assert_eq!(Some("c".to_string()), selected(&table));
    }

    /// A filter applied since the last render shrank the listing below the
    /// row the line map names.
    #[test]
    fn a_screen_relative_move_after_the_listing_shrank_lands_on_a_real_row() {
        use crate::command::{Command, handler::CommandHandler};

        let (_dir, mut table) = table();
        table.mapper = LineItemMap::new(&[1; 3], 3, 0);
        table.handle_command(&Command::FilterChanged("a".to_string()));
        table.enter_range_mode();

        table.select_last_visible_item();

        assert_eq!(Some("a".to_string()), selected(&table));
        assert_eq!(vec!["a"], marked(&table));
        assert_eq!(1, table.marks.len());
    }

    #[test]
    fn the_mark_key_ends_range_mode_and_keeps_the_range() {
        let (_dir, mut table) = table();
        table.select(0);
        table.enter_range_mode();
        table.select_next();

        table.toggle_mark();
        table.select_next();

        assert!(!table.marks.in_range_mode());
        assert_eq!(vec!["a", "b"], marked(&table));
    }

    #[test]
    fn moving_the_cursor_in_range_mode_sweeps_the_marks() {
        let (_dir, mut table) = table();
        table.select(0);
        table.enter_range_mode();
        assert_eq!(vec!["a"], marked(&table));

        table.select_next();
        table.select_next();

        assert_eq!(vec!["a", "b", "c"], marked(&table));
    }

    #[test]
    fn moving_back_toward_the_anchor_shrinks_the_range() {
        let (_dir, mut table) = table();
        table.select(0);
        table.enter_range_mode();
        table.select_last();
        assert_eq!(vec!["a", "b", "c"], marked(&table));

        table.select_previous();

        assert_eq!(vec!["a", "b"], marked(&table));
    }

    #[test]
    fn a_cursor_move_outside_range_mode_marks_nothing() {
        let (_dir, mut table) = table();
        table.select(0);

        table.select_next();

        assert!(table.marked_paths().is_empty());
        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn the_cursor_stops_at_both_ends_of_the_listing() {
        let (_dir, mut table) = table();
        table.select_last();
        assert_eq!(Some("c".to_string()), selected(&table));
        table.select_next();
        assert_eq!(Some("c".to_string()), selected(&table));

        table.select_first();
        table.select_previous();
        assert_eq!(Some("a".to_string()), selected(&table));

        table.select_middle_item();
        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn the_cursor_keys_are_safe_on_an_empty_listing() {
        crate::app::config::Config::init_test();
        let mut table = TableView::default();

        table.select_last();
        table.select_next();
        table.select_previous();
        table.select_middle_item();

        assert_eq!(None, selected(&table));
    }
}
