use log::debug;
use ratatui::{layout::Rect, widgets::ScrollbarState};

/// Controls scrollbar drag operations and calculates optimal viewport positions
pub struct ScrollbarController {
    /// The rectangular area where the scrollbar is drawn
    pub area: Rect,
    /// The visual state of the scrollbar
    pub state: ScrollbarState,
}

impl Default for ScrollbarController {
    fn default() -> Self {
        Self {
            area: Rect::default(),
            state: ScrollbarState::default(),
        }
    }
}

/// Represents the result of a scrollbar drag operation
#[derive(Debug)]
pub struct ScrollResult {
    /// The new offset to set in the table state
    pub new_offset: usize,
    /// The new selected item index
    pub selected_item: usize,
    /// The updated scrollbar state
    pub scrollbar_state: ScrollbarState,
}

impl ScrollbarController {
    /// Updates the scrollbar area
    pub fn set_area(&mut self, area: Rect) {
        self.area = area;
    }

    /// Updates the scrollbar state
    pub fn set_state(&mut self, state: ScrollbarState) {
        self.state = state;
    }

    /// Checks if a click is within the scrollbar area
    pub fn is_clicked(&self, x: u16, y: u16) -> bool {
        self.area.intersects(Rect::new(x, y, 1, 1))
    }

    /// Handles a drag operation on the scrollbar
    ///
    /// # Arguments
    /// * `y` - The y-coordinate where the scrollbar was dragged to
    /// * `total_lines` - The total number of lines in the content
    /// * `visible_height` - The number of lines visible in the viewport
    /// * `current_offset` - The current first visible item index
    /// * `item_count` - The total number of items in the list
    /// * `mapper` - A function that maps line positions to item indices
    ///
    /// # Returns
    /// A `ScrollResult` containing the new offset, selected item, and scrollbar state
    pub fn handle_drag<F>(
        &self,
        y: u16,
        total_lines: usize,
        visible_height: usize,
        current_offset: usize,
        item_count: usize,
        line_to_item: F,
    ) -> Option<ScrollResult>
    where
        F: Fn(usize) -> usize,
    {
        // Early return if scrolling isn't needed
        if total_lines <= visible_height {
            return None;
        }

        // Calculate relative position in scrollbar
        let scrollbar_height = self.area.height;
        let scrollbar_y = self.area.y;
        let relative_y = y.saturating_sub(scrollbar_y);
        let effective_height = scrollbar_height.saturating_sub(2); // Slightly reduced height for better control

        // Map scrollbar position to content percentage (0.0 to 1.0)
        let percentage = (relative_y as f32 / effective_height as f32).min(1.0);

        // Map percentage to line and item position
        let target_line = (percentage * (total_lines - 1) as f32).round() as usize;
        let target_item = line_to_item(target_line).min(item_count.saturating_sub(1));

        // Get information about the viewport
        let is_last_item = target_item + 1 >= item_count;

        // For debugging only
        if log::log_enabled!(log::Level::Debug) {
            let first_visible = current_offset;
            let last_visible = (first_visible + visible_height).min(item_count) - 1;

            debug!("Scrollbar Debug:");
            debug!(
                "  total_lines: {}, visible_height: {}",
                total_lines, visible_height
            );
            debug!(
                "  current_offset: {}, visible_items: {}..={}",
                current_offset, first_visible, last_visible
            );
            debug!(
                "  target_item: {}, target_line: {}, percentage: {:.2}",
                target_item, target_line, percentage
            );
        }

        // Calculate new offset with clear decision tree
        let new_offset = if target_item == 0 {
            // First item - always show from the beginning
            debug!("  case: first item selected");
            0
        } else if is_last_item || percentage > 0.97 {
            // Last item or near bottom - show the last page
            debug!("  case: last item/page selected");
            item_count.saturating_sub(visible_height)
        } else if target_item < current_offset {
            // Target is above viewport - position at top
            debug!("  case: scroll up, position target at top");
            target_item
        } else if target_item >= current_offset + visible_height.saturating_sub(1) {
            // Target is below viewport - position at bottom of view
            debug!("  case: scroll down, position target at bottom");
            target_item.saturating_sub(visible_height.saturating_sub(1))
        } else {
            // Target is already visible - maintain current view
            debug!("  case: target already visible, maintain position");
            current_offset
        };

        debug!("  final offset: {}", new_offset);

        // Create updated state
        Some(ScrollResult {
            new_offset,
            selected_item: target_item,
            scrollbar_state: self.state.content_length(total_lines).position(target_line),
        })
    }
}
