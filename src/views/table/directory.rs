use super::{
    columns::{SortColumn, SortDirection},
    TableView,
};
use crate::{
    command::result::CommandResult,
    file_system::path_info::PathInfo,
};

impl TableView {
    pub(super) fn set_directory(
        &mut self,
        new_directory: PathInfo,
        new_children: Vec<PathInfo>,
        is_refresh: bool,
    ) -> CommandResult {
        let prev_directory = self.directory.clone();

        if !is_refresh {
            self.filter.clear();
            self.clear_marks();
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

    pub(super) fn set_filter(&mut self, filter: String) -> CommandResult {
        // Avoid performing an extra SetFilter(None)
        // set_directory() -> sort() -> SetFilter(None) -> set_filter() -> sort() -> SetFilter(None)
        if self.filter.is_empty() && filter.is_empty() {
            return CommandResult::Handled;
        }
        self.filter = filter;
        self.sort(false)
    }

    pub(super) fn sort(&mut self, is_refresh: bool) -> CommandResult {
        self.clear_marks();

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
            if is_refresh && let Some(idx) = selected_index {
                return self.select(idx.min(self.directory_items_sorted.len().saturating_sub(1)));
            }
        }

        // Fallback: Select the first item if previous selection not found or none existed
        self.select(0)
    }

    pub(super) fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        self.columns.sort_by(column);
        self.sort(false)
    }
}
