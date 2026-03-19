use super::{columns::SortColumn, TableView};
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
        let prev_directory = self.content.directory().cloned();

        if !is_refresh {
            self.content.clear_filter();
            self.clear_marks();
            self.table_state.select(None); // An optimization to avoid attempting to re-select an item if we're in a different directory
        }

        // We must set items before sorting, because sort() depends on them
        self.content.set_items(new_directory.clone(), new_children);
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

                    if let Some(item) = self.content.find_by_path(&target_ancestor_path) {
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
        if self.content.filter().is_empty() && filter.is_empty() {
            return CommandResult::Handled;
        }
        self.content.set_filter(filter);
        self.sort(false)
    }

    pub(super) fn sort(&mut self, is_refresh: bool) -> CommandResult {
        self.clear_marks();

        // sort() is called when navigating or refreshing or sorting.
        // Sometimes, we won't be able to retain the currently selected item, b/c it
        // may no longer be present in the `items` (such as when navigating, or if it's been deleted),
        // but other times it is present (such as when refreshing),
        // though possibly at a different position (if another process has modified the directory).
        // We handle these cases by storing the currently selected item before assigning the new items,
        // and then attempting to restore the selection afterward by comparing inodes.
        let selected = self.selected_path().cloned();
        let selected_index = self.table_state.selected();

        self.content
            .sort(self.columns.sort_column(), self.columns.sort_direction());

        // Try to restore selection based on inode after sorting/filtering
        // This will only work if this was a refresh, not a navigation
        if let Some(selected_path) = selected {
            if let Some(new_index) = self.content.find_by_inode(&selected_path) {
                // Select the previously selected item if it still exists after sort/filter
                return self.select(new_index);
            }

            // If we're refreshing the directory and couldn't find the file - it was likely deleted
            // Select next item (same index) or index 0
            if is_refresh && let Some(idx) = selected_index {
                return self.select(idx.min(self.content.len().saturating_sub(1)));
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
