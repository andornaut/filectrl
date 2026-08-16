use chrono::Local;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Fill, StatefulWidget, TableState, Widget},
};

use super::{
    TableView,
    row_map::LineItemMap,
    widget::{item_height, row_widget_and_height, table_widget},
};
use crate::{app::config::Config, views::View};

const MIN_HEIGHT: u16 = 3; // header + 1 data row + scrollbar
const MIN_WIDTH: u16 = 8;

impl View for TableView {
    fn constraint(&self, _: Rect) -> Constraint {
        Constraint::Min(MIN_HEIGHT)
    }

    fn render(&mut self, area: Rect, frame: &mut Frame<'_>) {
        if area.height < MIN_HEIGHT || area.width < MIN_WIDTH {
            return;
        }

        let (block_area, scrollbar_area, table_area) = layout(area);

        // We must render the table first to initialize the mapper, which is used by the scrollbar
        self.render_table_and_init_mapper(table_area, frame.buffer_mut());
        // Must be rendered after render_table_and_init_mapper, because it depends on the mapper
        self.render_scrollbar(scrollbar_area, frame.buffer_mut());
        Self::render_1x1_block(block_area, frame.buffer_mut());
    }
}

impl TableView {
    fn render_1x1_block(area: Rect, buf: &mut Buffer) {
        let theme = Config::global().theme();
        // Extend the table header above the scrollbar as a 1x1 block
        Fill::new(" ")
            .style(theme.table.header())
            .render(Rect { height: 1, ..area }, buf);
    }

    fn render_scrollbar(&mut self, area: Rect, buf: &mut Buffer) {
        let total = self.mapper.total_lines_count();
        let visible = self.mapper.visible_lines_count();
        let max_position = total.saturating_sub(visible);
        let position = scrollbar_position(
            self.drag_line,
            self.mapper.first_visible_line(),
            max_position,
        );
        self.scrollbar_view
            .render(area, buf, position, max_position, visible);
    }

    fn render_table_and_init_mapper(&mut self, area: Rect, buf: &mut Buffer) {
        let theme = Config::global().theme();
        self.table_area = area;

        let column_constraints = self.columns.constraints(self.table_area.width);
        let relative_to_datetime = Local::now();
        let search_root = self.content.search_root();
        let is_bookmarks = self.content.is_showing_bookmarks();
        let name_width = self.columns.name_width();
        // -1 for the table header.
        let visible_lines_count = self.table_area.height.saturating_sub(1) as usize;

        let items = self.content.items_sorted();

        // Per-item heights drive the window math and the line<->item mapper the
        // scrollbar and mouse code use. They depend only on the name column
        // width and the listing, so caching them across frames keeps a height
        // per item plus the mapper's line map off every keystroke.
        let key = (name_width, self.content.revision());
        if self.height_cache_key != Some(key) {
            self.cached_heights = items
                .iter()
                .map(|item| item_height(name_width, item, is_bookmarks, search_root) as usize)
                .collect();
            self.mapper = LineItemMap::new(&self.cached_heights.clone(), visible_lines_count, 0);
            self.height_cache_key = Some(key);
        }

        // Own the scroll offset (rather than letting ratatui derive it from all
        // rows) so we can build Row widgets for only the visible window.
        let selected = self.table_state.selected();
        let (start, end) = visible_window(
            &self.cached_heights,
            visible_lines_count,
            selected.unwrap_or(0),
            self.first_visible_item,
        );
        self.first_visible_item = start;

        let has_pending_delete = !self.pending_delete.is_empty();
        let rows: Vec<_> = items[start..end]
            .iter()
            .enumerate()
            .map(|(offset, item)| {
                let i = start + offset;
                let is_pending_delete =
                    has_pending_delete && self.pending_delete.iter().any(|p| p == item);
                let (row, _height) = row_widget_and_height(
                    theme,
                    self.clipboard_entry.as_ref(),
                    name_width,
                    relative_to_datetime,
                    item,
                    self.marks.contains(i),
                    is_pending_delete,
                    is_bookmarks,
                    search_root,
                );
                row
            })
            .collect();

        let table = table_widget(
            theme,
            column_constraints,
            rows,
            self.columns.sort_column(),
            self.columns.sort_direction(),
        );

        // Render the window with a throwaway state: offset 0 (we already sliced
        // to the window) and the selection translated to be window-relative.
        // Using a local state keeps ratatui from mutating our own offset model.
        let mut render_state = TableState::default();
        if let Some(selected) = selected
            && selected >= start
            && selected < end
        {
            render_state.select(Some(selected - start));
        }
        StatefulWidget::render(table, area, buf, &mut render_state);

        // The cached mapper's line<->item data is still valid; only the viewport
        // window (which depends on the current selection and table height) moves.
        self.mapper.set_window(start, visible_lines_count);
    }
}

/// Returns the `[start, end)` range of item indices to render so the `selected`
/// item stays within `viewport_lines`, preferring to keep `prev_first` as the
/// top item (stable scrolling). Walks at most a viewport's worth of items, so it
/// is O(viewport), not O(items).
fn visible_window(
    item_heights: &[usize],
    viewport_lines: usize,
    selected: usize,
    prev_first: usize,
) -> (usize, usize) {
    let n = item_heights.len();
    if n == 0 || viewport_lines == 0 {
        return (0, 0);
    }
    let selected = selected.min(n - 1);
    // `prev_first.min(selected)` handles scrolling up: if the selection moved
    // above the previous window top, anchor the window at the selection.
    let mut start = prev_first.min(selected);
    if !fits_from(item_heights, start, selected, viewport_lines) {
        // Scrolling down: place `start` as high as possible while still showing
        // the selected item's last line.
        start = highest_start_keeping_visible(item_heights, selected, viewport_lines);
    }

    let mut end = start;
    let mut lines = 0;
    while end < n && lines < viewport_lines {
        lines += item_heights[end];
        end += 1;
    }
    (start, end)
}

/// Whether items `start..=selected` fit within `viewport_lines`. Bounded by the
/// viewport: it stops as soon as the running total exceeds it.
fn fits_from(item_heights: &[usize], start: usize, selected: usize, viewport_lines: usize) -> bool {
    let mut lines = 0;
    for &height in &item_heights[start..=selected] {
        lines += height;
        if lines > viewport_lines {
            return false;
        }
    }
    true
}

/// The highest (smallest-index) `start` that still shows the selected item's
/// last line within `viewport_lines`. If the selected item is taller than the
/// viewport, returns `selected` (its top is shown).
fn highest_start_keeping_visible(
    item_heights: &[usize],
    selected: usize,
    viewport_lines: usize,
) -> usize {
    let mut start = selected;
    let mut lines = item_heights[selected];
    while start > 0 && lines + item_heights[start - 1] <= viewport_lines {
        lines += item_heights[start - 1];
        start -= 1;
    }
    start
}

/// The thumb position: the dragged-to line while a drag is active, so the
/// thumb tracks the cursor exactly even when the window top snaps across a
/// wrapped row; otherwise the first visible line. Clamped because the
/// bottom-most window can underfill (a tall row forces an earlier anchor),
/// pushing its first line past `max_position`.
fn scrollbar_position(
    drag_line: Option<usize>,
    first_visible_line: usize,
    max_position: usize,
) -> usize {
    drag_line.unwrap_or(first_visible_line).min(max_position)
}

fn layout(area: Rect) -> (Rect, Rect, Rect) {
    let [table_area, mut scrollbar_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(1)].as_ref())
        .areas(area);
    // Make room for the 1x1 block
    let block_area = Rect {
        height: 1,
        ..scrollbar_area
    };
    scrollbar_area.y += 1;
    scrollbar_area.height -= 1;
    (block_area, scrollbar_area, table_area)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::{scrollbar_position, visible_window};

    // While a drag is live the thumb tracks the pointer, so it stays under the
    // cursor even where the window top snaps past a wrapped row. On release it
    // settles on the first visible line, clamped to the scale's end.
    #[test_case(Some(3), 5, 4 => 3 ; "a live drag tracks the cursor")]
    #[test_case(None, 5, 4 => 4 ; "after release, clamped to the end of the scale")]
    #[test_case(None, 2, 4 => 2 ; "with no drag, follows the first visible line")]
    fn scrollbar_position_cases(
        drag_line: Option<usize>,
        first_visible_line: usize,
        max_position: usize,
    ) -> usize {
        scrollbar_position(drag_line, first_visible_line, max_position)
    }

    // The window is a half-open range of items. It holds still while the
    // selection is already inside it, and otherwise moves the shortest distance
    // that brings the selection back into view.
    #[test_case(&[], 5, 0, 0 => (0, 0) ; "no items")]
    #[test_case(&[1, 1, 1], 0, 0, 0 => (0, 0) ; "no viewport")]
    #[test_case(&[1, 1, 1], 5, 0, 0 => (0, 3) ; "everything fits, filled from the top")]
    #[test_case(&[1; 10], 3, 3, 2 => (2, 5) ; "the selection is already visible")]
    #[test_case(&[1; 10], 3, 1, 5 => (1, 4) ; "the selection moved above the window")]
    #[test_case(&[1; 10], 3, 7, 0 => (5, 8) ; "the selection moved below, so it sits at the bottom")]
    #[test_case(&[1; 100], 3, 99, 0 => (97, 100) ; "a jump to the end anchors at the end")]
    // A row taller than the viewport can never fit whole, so the window shows
    // its top rather than scrolling past it.
    #[test_case(&[1, 5, 1], 3, 1, 0 => (1, 2) ; "an item taller than the viewport")]
    // After a resize an earlier row wraps, so the window reclamps around the
    // selection rather than keeping a top that no longer fits.
    #[test_case(&[1, 1, 3, 1], 3, 2, 0 => (2, 3) ; "narrowing grows the heights")]
    fn visible_window_cases(
        heights: &[usize],
        viewport_lines: usize,
        selected: usize,
        prev_first: usize,
    ) -> (usize, usize) {
        visible_window(heights, viewport_lines, selected, prev_first)
    }
}
