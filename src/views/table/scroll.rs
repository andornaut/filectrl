use super::LineItemMap;

/// Calculates the target item for the next page movement
pub(super) fn next_page(
    mapper: &LineItemMap,
    selected_item: usize,
    items_count: usize,
) -> Option<usize> {
    // If already at the last item, then no-op
    if selected_item == items_count.saturating_sub(1) {
        return None;
    }

    // If not at the last visible item, then move to it
    let current_last_line = mapper.last_visible_line();
    let current_last_item = mapper.item(current_last_line);
    if selected_item != current_last_item {
        return Some(current_last_item);
    }

    // Calculate new position based on current selection
    let new_first_line = mapper.first_line(selected_item);
    let new_last_line = mapper.last_visible_line_starting_at(new_first_line);
    let mut new_last_item = mapper.item(new_last_line);

    // Adjust if necessary to keep the selected item visible
    // If the last item overflows, ratatui will scroll down until it is fully visible,
    // so we need to "scroll up" `new_last_item`, so that the `current_last_item` remains visible.
    let new_last_item_last_line = mapper.last_line(new_last_item);
    if new_last_item_last_line > new_last_line {
        new_last_item = new_last_item.saturating_sub(1);
    }
    Some(new_last_item)
}

/// Calculates the target item for the previous page movement
pub(super) fn previous_page(
    mapper: &LineItemMap,
    selected_item: usize,
    viewport_offset: usize,
) -> Option<usize> {
    // If already at the first item, then no-op
    if selected_item == 0 {
        return None;
    }

    // If not at the first visible item, then move to it
    if selected_item != viewport_offset {
        return Some(viewport_offset);
    }

    // Calculate the new position based on the current selection. A first
    // line inside a wrapped row snaps forward, so the current first item
    // stays visible instead of being scrolled past.
    let new_last_item_first_line = mapper.first_line(selected_item);
    let new_first_line = mapper.first_visible_line_ending_at(new_last_item_first_line);
    Some(mapper.snap_to_item_start(new_first_line))
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{LineItemMap, next_page, previous_page};

    fn map(heights: Vec<usize>, visible: usize, first: usize) -> LineItemMap {
        LineItemMap::new(heights, visible, first)
    }

    // Five single-line items in a viewport of three, so the window shows items
    // 0-2. The first press lands on the last visible item; only once the cursor
    // is already there does a press advance a whole page.
    #[test_case(vec![1; 5], 3, 0, 4 => None ; "at the last item")]
    #[test_case(vec![1; 5], 3, 0, 0 => Some(2) ; "jumps to the last visible item")]
    #[test_case(vec![1; 5], 3, 0, 2 => Some(4) ; "pages once already at the last visible item")]
    // Item 3 is four lines tall, taller than the viewport. Selecting it would
    // make ratatui scroll until it fits, carrying the window past the item the
    // page was measured from, so the target backs off by one.
    #[test_case(vec![1, 1, 1, 4, 1], 3, 2, 3 => Some(2) ; "backs off when the new last item overflows")]
    fn next_page_target(
        heights: Vec<usize>,
        visible: usize,
        first: usize,
        selected: usize,
    ) -> Option<usize> {
        next_page(&map(heights, visible, first), selected, 5)
    }

    // The mirror of the above: the first press lands on the first visible item.
    #[test_case(vec![1; 5], 3, 0, 0, 0 => None ; "at the first item")]
    #[test_case(vec![1; 5], 3, 2, 4, 2 => Some(2) ; "jumps to the first visible item")]
    #[test_case(vec![1; 5], 3, 2, 2, 2 => Some(0) ; "pages once already at the first visible item")]
    // Item 0 is four lines tall, so a window anchored on it would scroll past
    // the item the page was measured from; the snap moves forward instead.
    #[test_case(vec![4, 1, 1], 3, 2, 2, 2 => Some(1) ; "advances when the new first item overflows")]
    fn previous_page_target(
        heights: Vec<usize>,
        visible: usize,
        first: usize,
        selected: usize,
        viewport_offset: usize,
    ) -> Option<usize> {
        previous_page(&map(heights, visible, first), selected, viewport_offset)
    }
}
