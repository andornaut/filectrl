use super::LineItemMap;

/// The target item for the next page movement.
pub(super) fn next_page(
    mapper: &LineItemMap,
    selected_item: usize,
    items_count: usize,
) -> Option<usize> {
    if selected_item == items_count.saturating_sub(1) {
        return None;
    }

    let current_last_line = mapper.last_visible_line();
    let current_last_item = mapper.item(current_last_line);
    if selected_item != current_last_item {
        return Some(current_last_item);
    }

    let new_first_line = mapper.first_line(selected_item);
    let new_last_line = mapper.last_visible_line_starting_at(new_first_line);
    let mut new_last_item = mapper.item(new_last_line);

    // ratatui scrolls an overflowing last item fully into view, so back off one
    // item to keep the current last item visible.
    let new_last_item_last_line = mapper.last_line(new_last_item);
    if new_last_item_last_line > new_last_line {
        new_last_item = new_last_item.saturating_sub(1);
    }
    // The back-off can land on or before the selection; step one item instead.
    if new_last_item <= selected_item {
        new_last_item = selected_item + 1;
    }
    Some(new_last_item)
}

/// The target item for the previous page movement.
pub(super) fn previous_page(
    mapper: &LineItemMap,
    selected_item: usize,
    viewport_offset: usize,
) -> Option<usize> {
    if selected_item == 0 {
        return None;
    }

    if selected_item != viewport_offset {
        return Some(viewport_offset);
    }

    let new_last_item_first_line = mapper.first_line(selected_item);
    let new_first_line = mapper.first_visible_line_ending_at(new_last_item_first_line);
    let new_first_item = mapper.snap_to_item_start(new_first_line);
    // The snap can land on or after the selection; step one item instead.
    if new_first_item >= selected_item {
        return Some(selected_item - 1);
    }
    Some(new_first_item)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{LineItemMap, next_page, previous_page};

    fn map(heights: &[usize], visible: usize, first: usize) -> LineItemMap {
        LineItemMap::new(heights, visible, first)
    }

    #[test_case(&[1; 5], 3, 0, 4 => None ; "at the last item")]
    #[test_case(&[1; 5], 3, 0, 1 => Some(2) ; "jumps to the last visible item")]
    #[test_case(&[1; 5], 3, 0, 2 => Some(4) ; "pages once already at the last visible item")]
    #[test_case(&[1, 1, 1, 2, 2, 1], 3, 2, 3 => Some(4) ; "backs off when the new last item overflows")]
    #[test_case(&[1, 1, 1, 4, 1], 3, 2, 3 => Some(4) ; "never moves backward")]
    #[test_case(&[1, 2, 2, 2], 3, 0, 1 => Some(2) ; "moves one item when the back-off lands on the cursor")]
    fn next_page_target(
        heights: &[usize],
        visible: usize,
        first: usize,
        selected: usize,
    ) -> Option<usize> {
        next_page(&map(heights, visible, first), selected, heights.len())
    }

    #[test_case(&[1; 5], 3, 0, 0, 0 => None ; "at the first item")]
    #[test_case(&[1; 5], 3, 2, 3, 2 => Some(2) ; "jumps to the first visible item")]
    #[test_case(&[1; 5], 3, 2, 2, 2 => Some(0) ; "pages once already at the first visible item")]
    #[test_case(&[4, 1, 1], 3, 2, 2, 2 => Some(1) ; "advances when the new first item overflows")]
    #[test_case(&[3, 1], 3, 1, 1, 1 => Some(0) ; "moves one item when the snap lands on the cursor")]
    fn previous_page_target(
        heights: &[usize],
        visible: usize,
        first: usize,
        selected: usize,
        viewport_offset: usize,
    ) -> Option<usize> {
        previous_page(&map(heights, visible, first), selected, viewport_offset)
    }
}
