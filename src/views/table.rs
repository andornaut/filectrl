mod columns;
mod double_click;
mod handler;
mod line_item_map;
mod pager;
mod scrollbar;
mod style;
mod view;
mod widgets;

use ratatui::{crossterm::event::MouseEvent, layout::Rect, widgets::TableState};

use self::{
    columns::{Columns, SortColumn, SortDirection},
    double_click::DoubleClick,
    line_item_map::LineItemMap,
    scrollbar::ScrollbarView,
};
use crate::{
    app::config::Config,
    clipboard::Clipboard,
    command::{result::CommandResult, Command, PromptKind},
    file_system::path_info::PathInfo,
};

#[derive(Default)]
pub(super) struct TableView {
    directory: Option<PathInfo>,
    directory_items: Vec<PathInfo>,
    directory_items_sorted: Vec<PathInfo>,
    filter: String,

    table_area: Rect,
    table_state: TableState,

    clipboard: Clipboard,
    columns: Columns,
    double_click: DoubleClick,
    mapper: LineItemMap,
    scrollbar_view: ScrollbarView,
}

impl TableView {
    pub fn new(config: &Config) -> Self {
        Self {
            double_click: DoubleClick::new(config),
            ..Self::default()
        }
    }

    fn clear_clipboard(&mut self) -> CommandResult {
        if !self.clipboard.is_enabled() {
            return CommandResult::Handled;
        }

        match self.clipboard.clear() {
            Ok(_) => Command::ClearedClipboard.into(),
            Err(e) => Command::AlertError(format!("Failed to clear clipboard: {}", e)).into(),
        }
    }

    fn copy_to_clipboard(&mut self) -> CommandResult {
        if !self.clipboard.is_enabled() {
            return CommandResult::Handled;
        }

        match self.selected_path() {
            None => Command::AlertWarn("No file selected".into()).into(),
            Some(path) => match self.clipboard.copy_file(path.path.as_str()) {
                Ok(_) => Command::CopiedToClipboard(path.clone()).into(),
                Err(e) => Command::AlertError(format!("Failed to copy: {}", e)).into(),
            },
        }
    }

    fn cut_to_clipboard(&mut self) -> CommandResult {
        if !self.clipboard.is_enabled() {
            return CommandResult::Handled;
        }

        match self.selected_path() {
            None => Command::AlertWarn("No file selected".into()).into(),
            Some(path) => match self.clipboard.cut_file(path.path.as_str()) {
                Ok(_) => Command::CutToClipboard(path.clone()).into(),
                Err(e) => Command::AlertError(format!("Failed to cut: {}", e)).into(),
            },
        }
    }

    fn paste_from_clipboard(&mut self) -> CommandResult {
        if !self.clipboard.is_enabled() {
            return CommandResult::Handled;
        }

        let destination = self.directory.as_ref().expect("Directory is always set");
        match self.clipboard.get_command(destination.clone()) {
            Some(command) => command.into(),
            None => CommandResult::Handled,
        }
    }

    fn click_header(&mut self, x: u16) -> CommandResult {
        self.columns
            .sort_column_for_click(x)
            .map_or(CommandResult::Handled, |column| self.sort_by(column))
    }

    fn click_table(&mut self, y: u16) -> CommandResult {
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

        self.select(item)
    }

    fn handle_scroll(&mut self, event: &MouseEvent) -> CommandResult {
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

    fn select_next(&mut self) -> CommandResult {
        self.table_state.scroll_down_by(1);
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    fn select_previous(&mut self) -> CommandResult {
        self.table_state.scroll_up_by(1);
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    fn select_first(&mut self) -> CommandResult {
        self.select(0)
    }

    fn select_last(&mut self) -> CommandResult {
        self.select(self.directory_items_sorted.len().saturating_sub(1))
    }

    fn select_middle_visible_item(&mut self) -> CommandResult {
        let first_line = self.mapper.first_visible_line();
        let last_line = self.mapper.last_visible_line();
        let middle_line = first_line + (last_line - first_line) / 2;
        self.select(self.mapper.item(middle_line))
    }

    fn next_page(&mut self) -> CommandResult {
        pager::next_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.directory_items_sorted.len(),
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    fn previous_page(&mut self) -> CommandResult {
        pager::previous_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.table_state.offset(),
        )
        .map_or(CommandResult::Handled, |item| self.select(item))
    }

    fn delete(&self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }

    fn navigate_to_home_directory(&mut self) -> CommandResult {
        match etcetera::home_dir() {
            Ok(path) => match PathInfo::try_from(&path) {
                Ok(path) => Command::Open(path).into(),
                Err(_) => Command::AlertError("Could not access home directory".into()).into(),
            },
            Err(_) => Command::AlertError("Could not determine home directory".into()).into(),
        }
    }

    fn open_filter_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Filter).into()
    }

    fn open_rename_prompt(&self) -> CommandResult {
        Command::OpenPrompt(PromptKind::Rename).into()
    }

    fn open_selected(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::Open(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }

    fn open_selected_in_custom_program(&mut self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::OpenCustom(path.clone()).into(),
            None => CommandResult::Handled,
        }
    }

    fn select(&mut self, item: usize) -> CommandResult {
        self.table_state.select(Some(item));
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    fn selected_path(&self) -> Option<&PathInfo> {
        self.table_state
            .selected()
            .filter(|&i| i < self.directory_items_sorted.len())
            .map(|i| &self.directory_items_sorted[i])
    }

    fn set_directory(
        &mut self,
        new_directory: PathInfo,
        new_children: Vec<PathInfo>,
    ) -> CommandResult {
        let prev_directory = self.directory.clone();

        // Check if we're refreshing the same directory or navigating to a different one
        let is_refresh = prev_directory
            .as_ref()
            .map_or(false, |prev_directory| prev_directory == &new_directory);

        if !is_refresh {
            self.filter.clear();
            self.table_state.select(None); // An optimization to avoid attempting to re-select an item if we're in a different directory
        }

        // We must update self.directory and self.directory_items before sorting, because sort() depends on them
        self.directory = Some(new_directory.clone());
        self.directory_items = new_children;
        let sort_result = self.sort(is_refresh);

        // If we navigated to an ancestor directory, then select the item that was previously in the path
        if let Some(prev_directory) = prev_directory {
            let prev_path = prev_directory.as_path();
            let new_path = new_directory.as_path();

            if prev_path.starts_with(new_path) && prev_path != new_path {
                let new_components_count = new_path.components().count();

                // .nth() is 0-indexed, so target_child is a child of new_path
                if let Some(target_child) = prev_path.components().nth(new_components_count) {
                    let target_ancestor_path = new_path.join(target_child);
                    let target_ancestor_path = target_ancestor_path.as_path();

                    // Try to find this ancestor in the newly sorted list
                    if let Some(item) = self
                        .directory_items_sorted
                        .iter()
                        .position(|item| item.as_path() == target_ancestor_path)
                    {
                        return self.select(item);
                    }
                }
            }
        }

        // Fallback: Return the default selection result from sort()
        sort_result
    }

    fn set_filter(&mut self, filter: String) -> CommandResult {
        // Avoid performing an extra SetFilter(None)
        // set_directory() -> sort() -> SetFilter(None) -> set_filter() -> sort() -> SetFilter(None)
        if self.filter.is_empty() && filter.is_empty() {
            return CommandResult::Handled;
        }
        self.filter = filter;
        self.sort(false)
    }

    fn sort(&mut self, is_refresh: bool) -> CommandResult {
        // Clone the self.directory_items, so we can sort without affecting the original items
        let mut items = self.directory_items.clone();

        match self.columns.sort_column() {
            SortColumn::Name => items.sort_by_cached_key(|path| path.name_comparator()),
            SortColumn::Modified => items.sort_by_cached_key(|path| path.modified_comparator()),
            SortColumn::Size => items.sort_by_cached_key(|path| path.size),
        };
        if *self.columns.sort_direction() == SortDirection::Descending {
            items.reverse();
        }

        if !self.filter.is_empty() {
            let filter_lowercase = self.filter.to_ascii_lowercase();
            items.retain(|path| path.name().to_ascii_lowercase().contains(&filter_lowercase));
        }

        // sort() is called when navigating or refreshing or sorting.
        // Sometimes, we won't be able to retain the currently selected item, b/c it
        // may no longer be present in the `items` (such as when navigating, or if it's been deleted),
        // but other times it is present (such as when refreshing),
        // though possibly at a different position (if another process has modified the directory).
        // We handle these cases by storing the currently selected item before assigning the new items,
        // and then attempting to restore the selection afterward by comparing inodes.
        let selected = self.selected_path().cloned();
        let selected_index = self.table_state.selected();

        // Must assign self.directory_items_sorted only after retrieving the previously selected item
        self.directory_items_sorted = items;

        // Try to restore selection based on inode after sorting/filtering
        // This will only work if this was a refresh, not a navigation
        if let Some(selected_path) = selected {
            if let Some(new_index) = self
                .directory_items_sorted
                .iter()
                .position(|p| p.is_same_inode(&selected_path))
            {
                // Select the previously selected item if it still exists after sort/filter
                return self.select(new_index);
            }

            // If we're refreshing the directory and couldn't find the file - it was likely deleted
            // Select next item (same index) or index 0
            if is_refresh {
                if let Some(idx) = selected_index {
                    return self
                        .select(idx.min(self.directory_items_sorted.len().saturating_sub(1)));
                }
            }
        }

        // Fallback: Select the first item if previous selection not found or none existed
        self.select(0)
    }

    fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        self.columns.sort_by(column);
        self.sort(false)
    }
}
