use std::collections::BTreeSet;

use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

use super::TableView;

#[derive(Default)]
pub(super) struct Marks {
    set: BTreeSet<usize>,
    range_anchor: Option<usize>,
    /// The marks made before range mode began, kept under the range so that a
    /// range adds to a selection rather than replacing it. Taken afresh each
    /// time range mode begins, so it is only read while `range_anchor` is set.
    range_base: BTreeSet<usize>,
}

impl Marks {
    /// Toggle a mark on `item`. Returns true if the item is now marked.
    pub(super) fn toggle(&mut self, item: usize) -> bool {
        if self.set.remove(&item) {
            false
        } else {
            self.set.insert(item);
            true
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
        self.range_base = self.set.clone();
        self.set.insert(item);
        true
    }

    /// Update marks to the ones made before range mode plus the span from the
    /// range anchor to `cursor`. No-op if not in range mode.
    pub(super) fn update_range(&mut self, cursor: usize) {
        if let Some(anchor) = self.range_anchor {
            let start = anchor.min(cursor);
            let end = anchor.max(cursor);
            self.set = self.range_base.iter().copied().chain(start..=end).collect();
        }
    }

    /// The range anchor and the marks made before the range began, while in
    /// range mode.
    fn range(&self) -> Option<(usize, &BTreeSet<usize>)> {
        Some((self.range_anchor?, &self.range_base))
    }

    /// Resume range mode at `anchor` over the earlier marks `base`, leaving the
    /// current marks as they are until the cursor next moves.
    fn restore_range(&mut self, anchor: usize, base: impl IntoIterator<Item = usize>) {
        self.range_anchor = Some(anchor);
        self.range_base = base.into_iter().collect();
    }

    pub(super) fn in_range_mode(&self) -> bool {
        self.range_anchor.is_some()
    }

    /// Mark `item`, leaving it marked if it already was.
    pub(super) fn insert(&mut self, item: usize) {
        self.set.insert(item);
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

    pub(super) fn contains(&self, item: usize) -> bool {
        self.set.contains(&item)
    }
}

/// Range mode captured by the entries it names, see
/// [`TableView::range_by_path`].
pub(super) struct RangeByPath {
    anchor: PathInfo,
    base: Vec<PathInfo>,
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
        self.selection_snapshot()
    }

    pub(super) fn enter_range_mode(&mut self) -> CommandResult {
        if let Some(i) = self.table_state.selected() {
            self.marks.enter_range(i);
        }
        self.selection_snapshot()
    }

    pub(super) fn clear_marks(&mut self) {
        self.marks.clear();
    }

    /// Clear all marks and return the snapshot that resets the mark-count
    /// notice. Emits only when marks were actually cleared, so callers can
    /// `return self.clear_marks_notifying()` to keep the NoticesView in sync
    /// (the marks set is the source of truth) without firing spurious commands
    /// when there was nothing marked.
    pub(super) fn clear_marks_notifying(&mut self) -> CommandResult {
        let had_marks = self.has_marks();
        self.clear_marks();
        if had_marks {
            self.selection_snapshot()
        } else {
            CommandResult::Handled
        }
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

    /// Range mode as the entries it names rather than their positions, to
    /// carry it across a rebuilt listing with [`Self::restore_range_by_path`].
    pub(super) fn range_by_path(&self) -> Option<RangeByPath> {
        let (anchor, base) = self.marks.range()?;
        Some(RangeByPath {
            anchor: self.content.get(anchor)?.clone(),
            base: base
                .iter()
                .filter_map(|&i| self.content.get(i).cloned())
                .collect(),
        })
    }

    /// Resume range mode captured by [`Self::range_by_path`], found again by
    /// path as the marks are. Range mode stays ended if its anchor is gone,
    /// since there is nothing left to measure the range from.
    pub(super) fn restore_range_by_path(&mut self, range: Option<RangeByPath>) {
        let Some(range) = range else { return };
        if let Some(anchor) = self.content.find_by_path(range.anchor.as_path()) {
            let base = self.content.find_all_by_path(&range.base);
            self.marks.restore_range(anchor, base);
        }
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
        assert!(marks.contains(3));
        assert!(!marks.toggle(3));
        assert!(!marks.contains(3));
    }

    #[test]
    fn enter_range_sets_anchor() {
        let mut marks = Marks::default();
        assert!(marks.enter_range(2));
        assert!(marks.in_range_mode());
        assert!(marks.contains(2));
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
            assert!(marks.contains(i));
        }
    }

    fn marked(marks: &Marks) -> Vec<usize> {
        marks.iter().copied().collect()
    }

    #[test]
    fn a_range_adds_to_the_marks_made_before_it() {
        let mut marks = Marks::default();
        marks.toggle(0);
        marks.enter_range(4);
        marks.update_range(6);
        assert_eq!(vec![0, 4, 5, 6], marked(&marks));
    }

    #[test]
    fn a_second_range_keeps_the_first() {
        let mut marks = Marks::default();
        marks.enter_range(0);
        marks.update_range(1);
        marks.enter_range(1); // exit, keeping 0-1
        marks.enter_range(5);
        marks.update_range(6);
        assert_eq!(vec![0, 1, 5, 6], marked(&marks));
    }

    /// Shrinking the range unmarks only what the range marked: an entry marked
    /// beforehand stays marked when the range no longer covers it.
    #[test]
    fn shrinking_a_range_keeps_an_earlier_mark_it_had_covered() {
        let mut marks = Marks::default();
        marks.toggle(5);
        marks.enter_range(3);
        marks.update_range(7);
        marks.update_range(3);
        assert_eq!(vec![3, 5], marked(&marks));
    }

    #[test]
    fn update_range_is_a_noop_when_not_in_range_mode() {
        let mut marks = Marks::default();
        marks.toggle(1);
        marks.update_range(9);
        assert_eq!(marks.len(), 1);
        assert!(marks.contains(1));
        assert!(!marks.contains(9));
    }

    #[test]
    fn update_range_works_when_cursor_is_before_anchor() {
        let mut marks = Marks::default();
        marks.enter_range(5);
        marks.update_range(2);
        assert_eq!(marks.len(), 4);
        for i in 2..=5 {
            assert!(marks.contains(i));
        }
    }

    #[test]
    fn update_range_shrinks_when_cursor_moves_back_toward_anchor() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        marks.update_range(5);
        marks.update_range(3);
        assert_eq!(marks.len(), 2);
        assert!(marks.contains(2));
        assert!(marks.contains(3));
        assert!(!marks.contains(4));
    }

    #[test]
    fn clear_resets_both_the_set_and_the_anchor() {
        let mut marks = Marks::default();
        marks.enter_range(2);
        marks.update_range(5);
        marks.clear();
        assert!(marks.is_empty());
        assert!(!marks.in_range_mode());
        // After clearing, a stale anchor must not resurrect a range.
        marks.update_range(9);
        assert!(marks.is_empty());
    }
}
