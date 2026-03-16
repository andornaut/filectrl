use std::cmp::min;

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
        min(
            first_line + self.visible_lines_count.saturating_sub(1),
            self.total_lines_count().saturating_sub(1),
        )
    }

    pub(super) fn total_lines_count(&self) -> usize {
        self.lines_to_items.len()
    }

    pub(super) fn visible_lines_count(&self) -> usize {
        self.visible_lines_count
    }
}
