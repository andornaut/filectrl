use super::{scroll, TableView};
use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    pub(super) fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        self.update_range_marks();
        if self.marks.in_range_mode() {
            return Command::SetMarkCount(self.marks.len()).into();
        }
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => Command::SetSelected(None).into(),
        }
    }

    pub(super) fn select_next(&mut self) -> CommandResult {
        self.table_state.scroll_down_by(1);
        self.update_range_marks();
        if self.marks.in_range_mode() {
            return Command::SetMarkCount(self.marks.len()).into();
        }
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    pub(super) fn select_previous(&mut self) -> CommandResult {
        self.table_state.scroll_up_by(1);
        self.update_range_marks();
        if self.marks.in_range_mode() {
            return Command::SetMarkCount(self.marks.len()).into();
        }
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    pub(super) fn select_first(&mut self) -> CommandResult {
        self.select(0)
    }

    pub(super) fn select_last(&mut self) -> CommandResult {
        self.select(self.content.len().saturating_sub(1))
    }

    pub(super) fn select_middle_visible_item(&mut self) -> CommandResult {
        let first_line = self.mapper.first_visible_line();
        let last_line = self.mapper.last_visible_line();
        let middle_line = first_line + (last_line - first_line) / 2;
        self.select(self.mapper.item(middle_line))
    }

    pub(super) fn next_page(&mut self) -> CommandResult {
        scroll::next_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.content.len(),
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    pub(super) fn previous_page(&mut self) -> CommandResult {
        scroll::previous_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.table_state.offset(),
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    pub(super) fn selected_path(&self) -> Option<&PathInfo> {
        self.table_state
            .selected()
            .and_then(|i| self.content.get(i))
    }
}
