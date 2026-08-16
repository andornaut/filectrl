#[derive(Default)]
pub(super) struct LineItemMap {
    first_visible_item: usize,
    visible_lines_count: usize,

    /// Maps each line index (y offset) to its corresponding item index
    lines_to_items: Vec<usize>,
    /// Maps each item index to the index of its first line: an O(1) alternative to scanning lines_to_items
    item_first_lines: Vec<usize>,
}

impl LineItemMap {
    pub(super) fn new(
        item_heights: &[usize],
        visible_lines_count: usize,
        first_visible_item: usize,
    ) -> Self {
        let mut lines_to_items = Vec::new();
        let mut item_first_lines = Vec::with_capacity(item_heights.len());
        for (i, &height) in item_heights.iter().enumerate() {
            item_first_lines.push(lines_to_items.len());
            lines_to_items.extend(std::iter::repeat_n(i, height));
        }
        Self {
            first_visible_item,
            visible_lines_count,
            lines_to_items,
            item_first_lines,
        }
    }

    /// Update only the viewport window (top item + visible line count) without
    /// rebuilding the line<->item mapping, which depends solely on item heights.
    /// Lets the view reuse a cached mapper across frames while scrolling.
    pub(super) fn set_window(&mut self, first_visible_item: usize, visible_lines_count: usize) {
        self.first_visible_item = first_visible_item;
        self.visible_lines_count = visible_lines_count;
    }

    pub(super) fn item(&self, line: usize) -> usize {
        self.lines_to_items.get(line).copied().unwrap_or(0)
    }

    pub(super) fn first_line(&self, item: usize) -> usize {
        self.item_first_lines.get(item).copied().unwrap_or(0)
    }

    /// The first item whose row starts at or after `line`, clamped to the
    /// last item. A line inside a wrapped (multi-line) row snaps forward to
    /// the next row boundary: pinning the wrapped row to the top would make
    /// the trailing items unreachable when scrolling to the bottom.
    pub(super) fn snap_to_item_start(&self, line: usize) -> usize {
        let item = self.item(line);
        if self.first_line(item) < line {
            (item + 1).min(self.item_first_lines.len().saturating_sub(1))
        } else {
            item
        }
    }

    pub(super) fn last_line(&self, item: usize) -> usize {
        self.item_first_lines.get(item + 1).map_or_else(
            || self.total_lines_count().saturating_sub(1),
            |&next_first| next_first - 1,
        )
    }

    pub(super) fn first_visible_line(&self) -> usize {
        self.first_line(self.first_visible_item)
    }

    pub(super) fn first_visible_line_ending_at(&self, last_line: usize) -> usize {
        last_line.saturating_sub(self.visible_lines_count.saturating_sub(1))
    }

    pub(super) fn middle_visible_line(&self) -> usize {
        let first = self.first_visible_line();
        first + self.last_visible_line().saturating_sub(first) / 2
    }

    pub(super) fn last_visible_line(&self) -> usize {
        self.last_visible_line_starting_at(self.first_visible_line())
    }

    pub(super) fn last_visible_line_starting_at(&self, first_line: usize) -> usize {
        (first_line + self.visible_lines_count.saturating_sub(1))
            .min(self.total_lines_count().saturating_sub(1))
    }

    pub(super) fn total_lines_count(&self) -> usize {
        self.lines_to_items.len()
    }

    pub(super) fn visible_lines_count(&self) -> usize {
        self.visible_lines_count
    }
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    fn map(heights: &[usize], visible: usize) -> LineItemMap {
        LineItemMap::new(heights, visible, 0)
    }

    // Every line of a wrapped row belongs to the one item that row renders.
    // heights [2, 1, 3]: item 0 owns lines 0-1, item 1 line 2, item 2 lines 3-5.
    #[test_case(&[1, 1, 1], 0 => 0 ; "single-line item")]
    #[test_case(&[1, 1, 1], 2 => 2 ; "the last single-line item")]
    #[test_case(&[2, 1, 3], 1 => 0 ; "the second line of a wrapped row")]
    #[test_case(&[2, 1, 3], 2 => 1 ; "the row after a wrapped one")]
    #[test_case(&[2, 1, 3], 5 => 2 ; "the last line of a trailing wrapped row")]
    fn item_owns_every_line_of_its_row(heights: &[usize], line: usize) -> usize {
        map(heights, 6).item(line)
    }

    #[test_case(&[1, 1, 1] => 3 ; "one line per item")]
    #[test_case(&[2, 1, 3] => 6 ; "wrapped rows are counted in full")]
    fn total_lines_count_sums_the_heights(heights: &[usize]) -> usize {
        map(heights, 6).total_lines_count()
    }

    // heights [2, 1, 3]: the rows start at lines 0, 2 and 3 and end at 1, 2
    // and 5. The last item has no successor, so its end is the final line.
    #[test_case(0 => (0, 1) ; "a wrapped row")]
    #[test_case(1 => (2, 2) ; "a single-line row")]
    #[test_case(2 => (3, 5) ; "the trailing row, whose end falls back to the total")]
    fn first_and_last_line_bound_each_row(item: usize) -> (usize, usize) {
        let m = map(&[2, 1, 3], 6);
        (m.first_line(item), m.last_line(item))
    }

    #[test]
    fn last_line_of_a_lone_wrapped_item_is_the_final_line() {
        assert_eq!(3, map(&[4], 3).last_line(0));
    }

    // Returns an item, not a line. heights [1, 1, 3, 1, 1]: item 2 spans lines
    // 2-4, so a line inside it snaps forward to item 3 rather than pinning the
    // wrapped row to the top, which would leave the trailing items unreachable.
    #[test_case(&[1, 1, 3, 1, 1], 0 => 0 ; "a line already at a row start")]
    #[test_case(&[1, 1, 3, 1, 1], 2 => 2 ; "the first line of a wrapped row")]
    #[test_case(&[1, 1, 3, 1, 1], 3 => 3 ; "inside a wrapped row snaps to the next")]
    #[test_case(&[1, 1, 3, 1, 1], 4 => 3 ; "the last line of a wrapped row")]
    #[test_case(&[1, 5], 3 => 1 ; "past the last row start clamps to the last item")]
    fn snap_to_item_start(heights: &[usize], line: usize) -> usize {
        map(heights, 3).snap_to_item_start(line)
    }

    // The viewport is `visible` lines tall and cannot run past either end.
    #[test_case(3, 4 => 2 ; "a full viewport ending at line 4 starts at 2")]
    #[test_case(10, 2 => 0 ; "a viewport taller than the content starts at 0")]
    fn first_visible_line_ending_at(visible: usize, last_line: usize) -> usize {
        map(&[1; 5], visible).first_visible_line_ending_at(last_line)
    }

    #[test_case(&[1; 5], 3, 1 => 3 ; "a full viewport starting at line 1 ends at 3")]
    #[test_case(&[1, 1, 1], 10, 0 => 2 ; "a viewport taller than the content clamps to the last line")]
    fn last_visible_line_starting_at(
        heights: &[usize],
        visible: usize,
        first_line: usize,
    ) -> usize {
        map(heights, visible).last_visible_line_starting_at(first_line)
    }
}
