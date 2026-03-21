use std::collections::BTreeSet;

use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

use super::TableView;

/// Result of a click interaction with the mark system.
pub(super) enum ClickMarkResult {
    /// Item was marked and is now unmarked.
    Unmarked,
    /// Marks were modified (range expanded or item added).
    MarksChanged,
    /// No marks involved — caller should do normal select.
    Ignored,
}

#[derive(Default)]
pub(super) struct Marks {
    set: BTreeSet<usize>,
    range_anchor: Option<usize>,
}

impl Marks {
    /// Handle a mouse click on `item`.
    ///
    /// - If range mode is active, expand the range from anchor to `item`.
    /// - Else if `item` is already marked, unmark it.
    /// - Else if other marks exist, add `item` to the discrete mark set.
    /// - Otherwise, return `Ignored` so the caller can do a normal select.
    pub(super) fn click(&mut self, item: usize) -> ClickMarkResult {
        // Range mode: always extend the range, even if the clicked item is already marked
        if let Some(anchor) = self.range_anchor {
            let start = anchor.min(item);
            let end = anchor.max(item);
            self.set = (start..=end).collect();
            ClickMarkResult::MarksChanged
        } else if self.set.remove(&item) {
            ClickMarkResult::Unmarked
        } else if !self.set.is_empty() {
            self.set.insert(item);
            ClickMarkResult::MarksChanged
        } else {
            ClickMarkResult::Ignored
        }
    }

    /// Toggle a mark on `item`. Returns true if the item is now marked.
    pub(super) fn toggle(&mut self, item: usize) -> bool {
        if !self.set.remove(&item) {
            self.set.insert(item);
            true
        } else {
            false
        }
    }

    /// Enter range mode at `item`. If already in range mode, exit it.
    /// Returns `true` if range mode is now active.
    pub(super) fn enter_range(&mut self, item: usize) -> bool {
        if self.range_anchor.is_some() {
            self.range_anchor = None;
            return false;
        }
        self.range_anchor = Some(item);
        self.set.insert(item);
        true
    }

    /// Update marks to span from the range anchor to `cursor`.
    /// No-op if not in range mode.
    pub(super) fn update_range(&mut self, cursor: usize) {
        if let Some(anchor) = self.range_anchor {
            let start = anchor.min(cursor);
            let end = anchor.max(cursor);
            self.set = (start..=end).collect();
        }
    }

    pub(super) fn in_range_mode(&self) -> bool {
        self.range_anchor.is_some()
    }

    pub(super) fn clear(&mut self) {
        self.set.clear();
        self.range_anchor = None;
    }

    pub(super) fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.set.len()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &usize> {
        self.set.iter()
    }

    pub(super) fn contains(&self, item: &usize) -> bool {
        self.set.contains(item)
    }
}

impl TableView {
    pub(super) fn toggle_mark(&mut self) -> CommandResult {
        if let Some(i) = self.table_state.selected() {
            if self.marks.in_range_mode() {
                self.marks.enter_range(i); // toggles range mode off
            } else {
                self.marks.toggle(i);
            }
        }
        Command::SetMarkCount(self.marks.len()).into()
    }

    pub(super) fn enter_range_mode(&mut self) -> CommandResult {
        if let Some(i) = self.table_state.selected() {
            self.marks.enter_range(i);
        }
        Command::SetMarkCount(self.marks.len()).into()
    }

    pub(super) fn clear_marks(&mut self) {
        self.marks.clear();
    }

    pub(super) fn has_marks(&self) -> bool {
        !self.marks.is_empty()
    }

    pub(super) fn marked_paths(&self) -> Vec<PathInfo> {
        self.marks
            .iter()
            .filter_map(|&i| self.content.get(i).cloned())
            .collect()
    }

    pub(super) fn update_range_marks(&mut self) {
        if let Some(cursor) = self.table_state.selected() {
            self.marks.update_range(cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_no_marks_returns_ignored() {
        let mut marks = Marks::default();
        assert!(matches!(marks.click(3), ClickMarkResult::Ignored));
        assert!(marks.is_empty());
    }

    #[test]
    fn click_marked_item_unmarks() {
        let mut marks = Marks::default();
        marks.toggle(3);
        assert!(matches!(marks.click(3), ClickMarkResult::Unmarked));
        assert!(marks.is_empty());
    }

    #[test]
    fn click_marked_item_leaves_other_marks() {
        let mut marks = Marks::default();
        marks.toggle(1);
        marks.toggle(3);
        assert!(matches!(marks.click(3), ClickMarkResult::Unmarked));
        assert_eq!(marks.len(), 1);
        assert!(marks.contains(&1));
    }

    #[test]
    fn click_in_range_mode_extends_range_even_on_anchor() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        assert!(marks.in_range_mode());
        // Clicking the anchor itself still extends (anchor..=anchor), keeping range mode
        assert!(matches!(marks.click(2), ClickMarkResult::MarksChanged));
        assert!(marks.in_range_mode());
        assert!(marks.contains(&2));
    }

    #[test]
    fn click_unmarked_in_range_expands_range() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        assert!(matches!(marks.click(5), ClickMarkResult::MarksChanged));
        assert_eq!(marks.len(), 4);
        for i in 2..=5 {
            assert!(marks.contains(&i));
        }
    }

    #[test]
    fn click_unmarked_with_marks_adds() {
        let mut marks = Marks::default();
        marks.toggle(1);
        marks.toggle(3);
        assert!(matches!(marks.click(5), ClickMarkResult::MarksChanged));
        assert_eq!(marks.len(), 3);
        assert!(marks.contains(&1));
        assert!(marks.contains(&3));
        assert!(marks.contains(&5));
    }

    #[test]
    fn toggle_adds_and_removes() {
        let mut marks = Marks::default();
        assert!(marks.toggle(3));
        assert!(marks.contains(&3));
        assert!(!marks.toggle(3));
        assert!(!marks.contains(&3));
    }

    #[test]
    fn enter_range_sets_anchor() {
        let mut marks = Marks::default();
        assert!(marks.enter_range(2));
        assert!(marks.in_range_mode());
        assert!(marks.contains(&2));
    }

    #[test]
    fn enter_range_twice_exits() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        assert!(!marks.enter_range(2));
        assert!(!marks.in_range_mode());
    }

    #[test]
    fn update_range_fills_between_anchor_and_cursor() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        marks.update_range(5);
        assert_eq!(marks.len(), 4);
        for i in 2..=5 {
            assert!(marks.contains(&i));
        }
    }

    #[test]
    fn clear_resets_everything() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        marks.update_range(5);
        marks.clear();
        assert!(marks.is_empty());
        assert!(!marks.in_range_mode());
    }
}
