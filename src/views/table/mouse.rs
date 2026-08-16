use ratatui::crossterm::event::MouseEvent;

use super::TableView;
use crate::command::result::CommandResult;

impl TableView {
    pub(super) fn click_header(&mut self, x: u16) -> CommandResult {
        self.columns
            .sort_column_for_click(x)
            .map_or(CommandResult::Handled, |column| self.sort_by(column))
    }

    pub(super) fn click_table(&mut self, y: u16) -> CommandResult {
        let y = y as usize - 1; // -1 for the header
        let line = self.mapper.first_visible_line() + y;
        if line >= self.mapper.total_lines_count() {
            // Clicked past the table
            return CommandResult::Handled;
        }

        let item = self.mapper.item(line);
        let Some(path) = self.content.get(item) else {
            return CommandResult::Handled;
        };
        if self.double_click.click_and_is_double_click(path) {
            return self.open_selected();
        }

        self.select(item)
    }

    pub(super) fn handle_scroll(&mut self, event: MouseEvent) -> CommandResult {
        // Use the same scale as the rendered thumb (line offset over
        // `total - visible`, see `render_scrollbar`). The dragged-to line
        // becomes the top of the window, snapped forward across wrapped rows
        // so the track bottom always reaches the bottom-most window; the
        // thumb meanwhile renders at `drag_line` so it stays on the cursor.
        let max_position = self
            .mapper
            .total_lines_count()
            .saturating_sub(self.mapper.visible_lines_count());
        let result = self
            .scrollbar_view
            .handle_mouse(event, max_position)
            .map_or(CommandResult::Handled, |line| {
                self.drag_line = Some(line);
                let item = self.mapper.snap_to_item_start(line);
                self.first_visible_item = item;
                self.select(item)
            });
        if !self.scrollbar_view.is_dragging() {
            self.drag_line = None;
        }
        result
    }
}
