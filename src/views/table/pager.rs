use super::LineItemMap;

/// Calculates the target item for the next page movement
pub fn next_page(mapper: &LineItemMap, selected_item: usize, items_count: usize) -> Option<usize> {
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
pub fn previous_page(
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
