use super::{TableView, scroll};
use crate::{
    command::{Command, result::CommandResult},
    file_system::path_info::PathInfo,
};

impl TableView {
    pub(super) fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        self.update_range_marks();
        if self.marks.in_range_mode() {
            return Command::MarkCountChanged(self.marks.len()).into();
        }
        match self.selected_path() {
            Some(path) => Command::SelectionChanged(Some(path.clone())).into(),
            None => Command::SelectionChanged(None).into(),
        }
    }

    pub(super) fn select_next(&mut self) -> CommandResult {
        // The render pass owns the scroll offset, so moving the selection is
        // enough; the next render re-derives the window to keep it visible.
        let last = self.content.len().saturating_sub(1);
        let next = self.table_state.selected().map_or(0, |i| (i + 1).min(last));
        self.select(next)
    }

    pub(super) fn select_previous(&mut self) -> CommandResult {
        let previous = self
            .table_state
            .selected()
            .map_or(0, |i| i.saturating_sub(1));
        self.select(previous)
    }

    pub(super) fn select_first(&mut self) -> CommandResult {
        self.select(0)
    }

    pub(super) fn select_last(&mut self) -> CommandResult {
        self.select(self.content.len().saturating_sub(1))
    }

    pub(super) fn select_middle_item(&mut self) -> CommandResult {
        self.select(self.content.len().saturating_sub(1) / 2)
    }

    pub(super) fn select_first_visible_item(&mut self) -> CommandResult {
        self.select(self.mapper.item(self.mapper.first_visible_line()))
    }

    pub(super) fn select_middle_visible_item(&mut self) -> CommandResult {
        self.select(self.mapper.item(self.mapper.middle_visible_line()))
    }

    pub(super) fn select_last_visible_item(&mut self) -> CommandResult {
        self.select(self.mapper.item(self.mapper.last_visible_line()))
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
            self.first_visible_item,
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    pub(super) fn selected_path(&self) -> Option<&PathInfo> {
        self.table_state
            .selected()
            .and_then(|i| self.content.get(i))
    }
}
