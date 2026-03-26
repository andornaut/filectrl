use std::path::Path;

use super::columns::{SortColumn, SortDirection};
use crate::app::config::Config;
use crate::file_system::path_info::PathInfo;

#[derive(Default)]
pub(super) struct DirectoryContent {
    directory: Option<PathInfo>,
    filter: String,
    items: Vec<PathInfo>,
    items_sorted: Vec<PathInfo>,
}

impl DirectoryContent {
    pub(super) fn get(&self, index: usize) -> Option<&PathInfo> {
        self.items_sorted.get(index)
    }

    pub(super) fn len(&self) -> usize {
        self.items_sorted.len()
    }

    pub(super) fn directory(&self) -> Option<&PathInfo> {
        self.directory.as_ref()
    }

    pub(super) fn filter(&self) -> &str {
        &self.filter
    }

    pub(super) fn items_sorted(&self) -> &[PathInfo] {
        &self.items_sorted
    }

    pub(super) fn set_items(&mut self, directory: PathInfo, items: Vec<PathInfo>) {
        self.directory = Some(directory);
        self.items = items;
    }

    pub(super) fn set_filter(&mut self, filter: String) {
        self.filter = filter;
    }

    pub(super) fn clear_filter(&mut self) {
        self.filter.clear();
    }

    /// Sort and filter items into `items_sorted`.
    pub(super) fn sort(&mut self, sort_column: &SortColumn, sort_direction: &SortDirection) {
        let mut items = self.items.clone();

        match sort_column {
            SortColumn::Name => items.sort_by_cached_key(|path| path.name_comparator()),
            SortColumn::Modified => items.sort_by_cached_key(|path| path.modified_comparator()),
            SortColumn::Size => items.sort_by_cached_key(|path| path.size),
        };
        if *sort_direction == SortDirection::Descending {
            items.reverse();
        }

        if *sort_column == SortColumn::Name && Config::global().ui.sort_directories_first {
            items.sort_by_key(|path| !path.is_directory());
        }

        if !self.filter.is_empty() {
            let filter_lowercase = self.filter.to_ascii_lowercase();
            items.retain(|path| path.name().to_ascii_lowercase().contains(&filter_lowercase));
        }

        self.items_sorted = items;
    }

    pub(super) fn find_by_inode(&self, path: &PathInfo) -> Option<usize> {
        self.items_sorted.iter().position(|p| p.is_same_inode(path))
    }

    pub(super) fn find_by_path(&self, target: &Path) -> Option<usize> {
        self.items_sorted
            .iter()
            .position(|item| item.as_path() == target)
    }
}
