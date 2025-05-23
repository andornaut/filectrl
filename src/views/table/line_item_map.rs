use std::cmp::min;

#[derive(Default)]
pub(super) struct LineItemMap {
    first_visible_item: usize,
    visible_lines_count: usize,

    /// Maps each line index (y offset) to its corresponding item index
    lines_to_items: Vec<usize>,
}

impl LineItemMap {
    pub(super) fn new(
        item_heights: Vec<u16>,
        visible_lines_count: usize,
        first_visible_item: usize,
    ) -> Self {
        let lines_to_items = item_heights
            .iter()
            .enumerate()
            .flat_map(|(i, &height)| std::iter::repeat_n(i, height as usize))
            .collect();
        Self {
            first_visible_item,
            lines_to_items,
            visible_lines_count,
        }
    }

    pub(super) fn item(&self, line: usize) -> usize {
        self.lines_to_items.get(line).copied().unwrap_or(0)
    }
    pub(super) fn first_line(&self, item: usize) -> usize {
        // TODO This should probably return None if the item is not found
        self.lines_to_items
            .iter()
            .position(|&i| i == item)
            .unwrap_or_default()
    }

    pub(super) fn last_line(&self, item: usize) -> usize {
        // TODO This should probably return None if the item is not found
        self.lines_to_items
            .iter()
            .rposition(|&i| i == item)
            .unwrap_or_default()
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
