#[derive(Default)]
pub(super) struct LineItemMap {
    first_visible_item: usize,
    visible_lines_count: usize,

    /// Maps each line index (y offset) to its corresponding item index
    lines_to_items: Vec<usize>,
    /// Maps each item index to the index of its first line — O(1) alternative to scanning lines_to_items
    item_first_lines: Vec<usize>,
}

impl LineItemMap {
    pub(super) fn new(
        item_heights: Vec<u16>,
        visible_lines_count: usize,
        first_visible_item: usize,
    ) -> Self {
        let mut lines_to_items = Vec::new();
        let mut item_first_lines = Vec::with_capacity(item_heights.len());
        for (i, &height) in item_heights.iter().enumerate() {
            item_first_lines.push(lines_to_items.len());
            lines_to_items.extend(std::iter::repeat_n(i, height as usize));
        }
        Self {
            first_visible_item,
            lines_to_items,
            visible_lines_count,
            item_first_lines,
        }
    }

    pub(super) fn item(&self, line: usize) -> usize {
        self.lines_to_items.get(line).copied().unwrap_or(0)
    }

    pub(super) fn first_line(&self, item: usize) -> usize {
        self.item_first_lines.get(item).copied().unwrap_or(0)
    }

    pub(super) fn last_line(&self, item: usize) -> usize {
        self.item_first_lines
            .get(item + 1)
            .map(|&next_first| next_first - 1)
            .unwrap_or_else(|| self.total_lines_count().saturating_sub(1))
    }

    pub(super) fn first_visible_line(&self) -> usize {
        self.first_line(self.first_visible_item)
    }

    pub(super) fn first_visible_line_ending_at(&self, last_line: usize) -> usize {
        last_line.saturating_sub(self.visible_lines_count.saturating_sub(1))
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
    use super::*;

    fn map(heights: Vec<u16>, visible: usize) -> LineItemMap {
        LineItemMap::new(heights, visible, 0)
    }

    #[test]
    fn single_line_items_map_each_line_to_its_own_item() {
        let m = map(vec![1, 1, 1], 3);
        assert_eq!(0, m.item(0));
        assert_eq!(1, m.item(1));
        assert_eq!(2, m.item(2));
        assert_eq!(3, m.total_lines_count());
    }

    #[test]
    fn multi_line_items_map_all_their_lines_to_the_same_item() {
        // item 0: lines 0-1, item 1: line 2, item 2: lines 3-5
        let m = map(vec![2, 1, 3], 6);
        assert_eq!(0, m.item(0));
        assert_eq!(0, m.item(1));
        assert_eq!(1, m.item(2));
        assert_eq!(2, m.item(3));
        assert_eq!(2, m.item(5));
        assert_eq!(6, m.total_lines_count());
    }

    #[test]
    fn first_line_returns_the_starting_line_of_each_item() {
        let m = map(vec![2, 1, 3], 6);
        assert_eq!(0, m.first_line(0));
        assert_eq!(2, m.first_line(1));
        assert_eq!(3, m.first_line(2));
    }

    #[test]
    fn last_line_returns_the_ending_line_of_each_item() {
        let m = map(vec![2, 1, 3], 6);
        assert_eq!(1, m.last_line(0));
        assert_eq!(2, m.last_line(1));
        assert_eq!(5, m.last_line(2)); // last item: falls back to total_lines - 1
    }

    #[test]
    fn last_line_of_single_item_is_total_lines_minus_one() {
        let m = map(vec![4], 3);
        assert_eq!(3, m.last_line(0));
    }

    #[test]
    fn first_visible_line_ending_at_calculates_viewport_start() {
        // viewport of 3 lines, last visible is line 4 → first visible is line 2
        let m = map(vec![1; 5], 3);
        assert_eq!(2, m.first_visible_line_ending_at(4));
    }

    #[test]
    fn first_visible_line_ending_at_saturates_when_viewport_exceeds_last_line() {
        // viewport of 10, last line is 2 → cannot start before 0
        let m = map(vec![1; 5], 10);
        assert_eq!(0, m.first_visible_line_ending_at(2));
    }

    #[test]
    fn last_visible_line_starting_at_advances_by_viewport_height() {
        let m = map(vec![1; 5], 3);
        assert_eq!(3, m.last_visible_line_starting_at(1));
    }

    #[test]
    fn last_visible_line_starting_at_clamps_to_total_lines() {
        // viewport is larger than total content
        let m = map(vec![1, 1, 1], 10);
        assert_eq!(2, m.last_visible_line_starting_at(0));
    }
}
