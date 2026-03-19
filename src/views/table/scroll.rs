use super::LineItemMap;

/// Calculates the target item for the next page movement
pub(super) fn next_page(mapper: &LineItemMap, selected_item: usize, items_count: usize) -> Option<usize> {
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

    // Calculate new position based on current selection
    let new_last_item_first_line = mapper.first_line(selected_item);
    let new_first_line = mapper.first_visible_line_ending_at(new_last_item_first_line);
    let mut new_first_item = mapper.item(new_first_line);

    // Adjust if necessary to keep the selected item visible
    // If the first item overflows, ratatui will scroll up until it is fully visible,
    // so we need to "scroll down" `new_first_item`, so that the `current_first_item` remains visible.
    let new_first_item_first_line = mapper.first_line(new_first_item);
    if new_first_item_first_line < new_first_line {
        new_first_item = new_first_item.saturating_add(1);
    }
    Some(new_first_item)
}

#[cfg(test)]
mod tests {
    use super::{next_page, previous_page, LineItemMap};

    fn map(heights: Vec<usize>, visible: usize, first: usize) -> LineItemMap {
        LineItemMap::new(heights, visible, first)
    }

    // --- next_page ---

    #[test]
    fn next_page_at_last_item_is_a_noop() {
        let m = map(vec![1; 5], 3, 0);
        assert_eq!(None, next_page(&m, 4, 5));
    }

    #[test]
    fn next_page_jumps_to_last_visible_when_not_already_there() {
        // viewport shows items 0-2; selected=0 — should jump to item 2, not a full page
        let m = map(vec![1; 5], 3, 0);
        assert_eq!(Some(2), next_page(&m, 0, 5));
    }

    #[test]
    fn next_page_advances_a_full_page_when_at_last_visible() {
        // viewport shows items 0-2; selected=2 (last visible) — pages to item 4
        let m = map(vec![1; 5], 3, 0);
        assert_eq!(Some(4), next_page(&m, 2, 5));
    }

    #[test]
    fn next_page_backs_off_when_new_last_item_overflows_viewport() {
        // item 3 is 4 lines tall (> viewport 3); selecting it causes ratatui to scroll
        // beyond the intended position, so the result is adjusted back by one item
        let m = map(vec![1, 1, 1, 4, 1], 3, 2);
        assert_eq!(Some(2), next_page(&m, 3, 5));
    }

    // --- previous_page ---

    #[test]
    fn previous_page_at_first_item_is_a_noop() {
        let m = map(vec![1; 5], 3, 0);
        assert_eq!(None, previous_page(&m, 0, 0));
    }

    #[test]
    fn previous_page_jumps_to_first_visible_when_not_already_there() {
        // first visible item = 2; selected=4 — should jump to item 2, not a full page
        let m = map(vec![1; 5], 3, 2);
        assert_eq!(Some(2), previous_page(&m, 4, 2));
    }

    #[test]
    fn previous_page_retreats_a_full_page_when_at_first_visible() {
        // viewport shows items 2-4; selected=2 (first visible) — retreats to item 0
        let m = map(vec![1; 5], 3, 2);
        assert_eq!(Some(0), previous_page(&m, 2, 2));
    }

    #[test]
    fn previous_page_advances_when_new_first_item_overflows_viewport() {
        // item 0 is 4 lines tall (> viewport 3); selecting it causes ratatui to scroll
        // beyond the intended position, so the result is adjusted forward by one item
        let m = map(vec![4, 1, 1], 3, 2);
        assert_eq!(Some(1), previous_page(&m, 2, 2));
    }
}
