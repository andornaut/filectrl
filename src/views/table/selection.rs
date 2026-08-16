use super::{TableView, scroll};
use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    pub(super) fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        self.update_range_marks();
        self.selection_snapshot()
    }

    /// The current selection and mark count as a single snapshot command.
    pub(super) fn selection_snapshot(&self) -> CommandResult {
        Command::SelectionChanged {
            selected: self.selected_path().cloned(),
            mark_count: self.marks.len(),
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
    use super::super::{TableView, display_names, marked_table};

    fn marked(table: &TableView) -> Vec<String> {
        display_names(&table.marked_paths())
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
