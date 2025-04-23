mod columns;
mod double_click;
mod handler;
mod line_item_map;
mod pager;
mod render;
mod scrollbar;
mod style;
mod widgets;

use ratatui::{crossterm::event::MouseEvent, layout::Rect, widgets::TableState};
use std::path::Path;

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
        self.clipboard
            .clear()
            .map_err(|e| Command::AlertError(format!("Failed to clear clipboard: {}", e)))
            .map(|_| Command::ClearedClipboard.into())
            .unwrap_or_else(|error_command| error_command.into())
    }

    fn copy_to_clipboard(&mut self) -> CommandResult {
        self.selected_path().map_or(
            Command::AlertWarn("No file selected".into()).into(),
            |path| {
                self.clipboard
                    .copy_file(path.path.as_str())
                    .map_err(|e| Command::AlertError(format!("Failed to copy: {}", e)))
                    .map(|_| Command::CopiedToClipboard(path.clone()).into())
                    .unwrap_or_else(|error_command| error_command.into())
            },
        )
    }

    fn cut_to_clipboard(&mut self) -> CommandResult {
        self.selected_path().map_or(
            Command::AlertWarn("No file selected".into()).into(),
            |path| {
                self.clipboard
                    .cut_file(path.path.as_str())
                    .map_err(|e| Command::AlertError(format!("Failed to cut: {}", e)))
                    .map(|_| Command::CutToClipboard(path.clone()).into())
                    .unwrap_or_else(|error_command| error_command.into())
            },
        )
    }

    fn paste_from_clipboard(&mut self) -> CommandResult {
        let destination = self
            .directory
            .as_ref()
            .expect("Directory should always be set");
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
        let clicked_line = self.mapper.first_visible_line() + y;
        if clicked_line >= self.mapper.total_lines_count() {
            // Clicked past the table
            return CommandResult::Handled;
        }

        let clicked_item = self.mapper.item(clicked_line);
        let clicked_path = &self.directory_items_sorted[clicked_item];
        if self.double_click.click_and_is_double_click(clicked_path) {
            return self.open_selected();
        }

        self.table_state.select(Some(clicked_item));
        Command::SetSelected(Some(self.selected_path().unwrap().clone())).into()
    }

    fn handle_scroll(&mut self, event: &MouseEvent) -> CommandResult {
        self.scrollbar_view
            .handle_mouse(
                event,
                self.mapper.total_lines_count(),
                self.mapper.visible_lines_count(),
            )
            .map_or(CommandResult::Handled, |selected_item| {
                self.select(self.mapper.item(selected_item))
            })
    }

    // Navigate
    fn next(&mut self) -> CommandResult {
        self.table_state.scroll_down_by(1);
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    fn previous(&mut self) -> CommandResult {
        self.table_state.scroll_up_by(1);
        match self.selected_path() {
            Some(path) => Command::SetSelected(Some(path.clone())).into(),
            None => CommandResult::Handled,
        }
    }

    fn first(&mut self) -> CommandResult {
        self.select(0)
    }

    fn last(&mut self) -> CommandResult {
        self.select(self.directory_items_sorted.len().saturating_sub(1))
    }

    fn next_page(&mut self) -> CommandResult {
        pager::next_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.directory_items_sorted.len(),
        )
        .map_or(CommandResult::Handled, |selected_item| {
            self.select(selected_item)
        })
    }

    fn previous_page(&mut self) -> CommandResult {
        pager::previous_page(
            &self.mapper,
            self.table_state.selected().unwrap_or_default(),
            self.table_state.offset(),
        )
        .map_or(CommandResult::Handled, |selected_item| {
            self.select(selected_item)
        })
    }

    fn delete(&self) -> CommandResult {
        match self.selected_path() {
            Some(path) => Command::DeletePath(path.clone()).into(),
            None => CommandResult::Handled,
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

    fn set_directory(&mut self, new_directory: PathInfo, children: Vec<PathInfo>) -> CommandResult {
        let prev_directory: Option<PathInfo> = self.directory.clone(); // Store previous directory

        // Clear the filter if we're navigating to a different directory
        let is_different_dir = prev_directory
            .as_ref()
            .map_or(true, |prev_directory| prev_directory != &new_directory);
        if is_different_dir {
            self.filter.clear();
        }

        // We must set self.directory and self.directory_items before sorting, because sort() uses them
        self.directory = Some(new_directory.clone());
        self.directory_items = children;
        let sort_result = self.sort();

        // If we navigated to an ancestor directory, then select the item that was previously in the path
        if let Some(prev_directory) = prev_directory {
            let prev_path = Path::new(&prev_directory.path);
            let new_path = Path::new(&new_directory.path);

            if prev_path.starts_with(new_path) && prev_path != new_path {
                let new_path_components_count = new_path.components().count();
                if let Some(target_component) =
                    prev_path.components().nth(new_path_components_count)
                {
                    let target_ancestor_path = new_path.join(target_component.as_os_str());

                    // Try to find this ancestor in the newly sorted list
                    if let Some(item) = self
                        .directory_items_sorted
                        .iter()
                        .position(|item| Path::new(&item.path) == target_ancestor_path.as_path())
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
        self.sort()
    }

    fn sort(&mut self) -> CommandResult {
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
        }
        // Fallback: Select the first item if previous selection not found or none existed
        self.select(0)
    }

    fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        self.columns.sort_by(column);
        self.sort()
    }
}
