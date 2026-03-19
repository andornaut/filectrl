use ratatui::crossterm::event::MouseEvent;

use super::{marks::ClickMarkResult, TableView};
use crate::command::{Command, result::CommandResult};

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
        let path = &self.directory_items_sorted[item];
        if self.double_click.click_and_is_double_click(path) {
            return self.open_selected();
        }

        match self.marks.click(item) {
            ClickMarkResult::Unmarked => {
                self.table_state.select(Some(item));
                if self.marks.is_empty() {
                    match self.selected_path() {
                        Some(path) => Command::SetSelected(Some(path.clone())).into(),
                        None => Command::SetSelected(None).into(),
                    }
                } else {
                    Command::SetMarkCount(self.marks.len()).into()
                }
            }
            ClickMarkResult::MarksChanged => {
                self.table_state.select(Some(item));
                Command::SetMarkCount(self.marks.len()).into()
            }
            ClickMarkResult::Ignored => self.select(item),
        }
    }

    pub(super) fn handle_scroll(&mut self, event: &MouseEvent) -> CommandResult {
        self.scrollbar_view
            .handle_mouse(
                event,
                self.mapper.total_lines_count(),
                self.mapper.visible_lines_count(),
            )
            .map_or(CommandResult::Handled, |line| {
                self.select(self.mapper.item(line))
            })
    }
}
