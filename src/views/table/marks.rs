use std::collections::BTreeSet;

use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

use super::TableView;

#[derive(Default)]
pub(super) struct Marks {
    set: BTreeSet<usize>,
    range_anchor: Option<usize>,
}

impl Marks {
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

}
