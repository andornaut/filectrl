use super::{TableView, scroll};
use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    /// Puts the cursor on `item`. An empty listing has no row to put it on, so
    /// it gets no cursor, and nothing that acts on the cursor finds an entry.
    pub(super) fn select(&mut self, item: usize) -> CommandResult {
        self.table_state
            .select((self.content.len() != 0).then_some(item));
        self.update_range_marks();
        self.selection_snapshot()
    }

    /// How many of the directory's entries are shown, or `None` when the
    /// listing is not the directory's (search results, bookmarks), so the
    /// status bar's count does not describe it.
    pub(in crate::views) fn shown_len(&self) -> Option<usize> {
        (!self.content.is_searching() && !self.content.is_showing_bookmarks())
            .then(|| self.content.len())
    }

    /// The current selection and mark count as a single snapshot command.
    pub(super) fn selection_snapshot(&self) -> CommandResult {
        Command::SelectionChanged {
            selected: self.selected_path().cloned(),
            mark_count: self.marks.len(),
            range: self.marks.in_range_mode(),
        }
        .into()
    }

    pub(super) fn select_next(&mut self) -> CommandResult {
        // The render pass owns the scroll offset, so moving the selection is
        // enough; the next render re-derives the window to keep it visible.
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

    pub(super) fn next_page(&mut self) -> CommandResult {
        scroll::next_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.content.len(),
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    pub(super) fn previous_page(&mut self) -> CommandResult {
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

    /// The status bar counts the directory's entries, so only a listing of
    /// that directory reports how many of them it shows.
    #[test]
    fn the_shown_count_describes_only_a_listing_of_the_directory() {
        use crate::command::{Command, handler::CommandHandler};

        let (dir, mut table) = marked_table();
        assert_eq!(Some(3), table.shown_len());
        table.handle_command(&Command::FilterChanged("a".to_string()));
        assert_eq!(Some(1), table.shown_len());

        table.content.set_bookmarks(vec![
            crate::file_system::path_info::PathInfo::try_from(dir.path()).unwrap(),
        ]);
        assert_eq!(None, table.shown_len());
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

    /// A four entry listing. An odd count cannot tell `(len - 1) / 2` from
    /// `len / 2`, which is the whole of what the middle-item rule says.
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

    /// Four one-line rows in a four-line window: the first, middle and last
    /// visible rows are three different entries, and the middle is not the
    /// centre row rounded up.
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
        // Scrolled down one row, as a render leaves it: `b` is the top row.
        table.first_visible_item = 1;
        table.mapper = LineItemMap::new(&[1; 3], 2, 1);
        table.select(2);

        table.previous_page();

        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn the_mark_key_ends_range_mode_and_keeps_the_range() {
        let (_dir, mut table) = table();
        table.select(0);
        table.enter_range_mode();
        table.select_next();

        table.toggle_mark();
        table.select_next();

        // The key that started the range is the one that fixes it: the cursor
        // row stays marked, and moving on no longer sweeps.
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

        // Sweeping as the cursor moves is what range mode is for, and `select`
        // is the one place every cursor move goes through.
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

        // The range spans anchor to cursor, so overshooting is undone by
        // moving back rather than leaving the extra entry marked.
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

        // Of `a`, `b`, `c`, the middle is `b`.
        table.select_middle_item();
        assert_eq!(Some("b".to_string()), selected(&table));
    }

    #[test]
    fn the_cursor_keys_are_safe_on_an_empty_listing() {
        // Every cursor move ends in a selection snapshot, which reads the
        // theme, so this test cannot borrow another one's initialization.
        crate::app::config::Config::init_test();
        let mut table = TableView::default();

        // Each of these bounds itself with `saturating_sub` on a length of
        // zero; a plain `- 1` would panic before anything could be selected.
        table.select_last();
        table.select_next();
        table.select_previous();
        table.select_middle_item();

        assert_eq!(None, selected(&table));
    }
}
