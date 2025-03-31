use std::iter::repeat;

#[derive(Default)]
pub(super) struct LineToItemMapper {
    /// Maps each line (y offset) to its corresponding item index
    lines_to_items: Vec<usize>,

    number_of_visible_lines: usize,
    /// The first item index that should be visible
    first_visible_item: usize,
}

impl LineToItemMapper {
    pub(super) fn new(
        item_heights: Vec<u16>,
        number_of_visible_lines: usize,
        first_visible_item: usize,
    ) -> Self {
        let lines_to_items = item_heights
            .iter()
            .enumerate()
            .flat_map(|(i, &height)| repeat(i).take(height as usize))
            .collect();
        Self {
            lines_to_items,
            number_of_visible_lines,
            first_visible_item,
        }
    }

    pub(super) fn get_line(&self, item_index: usize) -> usize {
        self.lines_to_items
            .iter()
            .position(|&i| i == item_index)
            .unwrap_or_default()
    }

    pub(super) fn first_visible_line(&self) -> usize {
        self.get_line(self.first_visible_item)
    }

    pub(super) fn last_visible_item(&self) -> usize {
        self[self.last_visible_line(self.first_visible_line())]
    }

    pub(super) fn last_visible_line(&self, first_line: usize) -> usize {
        first_line + self.number_of_visible_lines.saturating_sub(1)
    }

    pub(super) fn total_number_of_lines(&self) -> usize {
        self.lines_to_items.len()
    }
}

impl std::ops::Index<usize> for LineToItemMapper {
    type Output = usize;

    fn index(&self, index: usize) -> &Self::Output {
        &self.lines_to_items[index]
    }
}
