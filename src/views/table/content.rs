use std::path::{Path, PathBuf};

use super::columns::{SortColumn, SortDirection};
use crate::app::config::Config;
use crate::file_system::path_info::PathInfo;

#[derive(Default)]
pub(super) struct DirectoryContent {
    directory: Option<PathInfo>,
    filter: String,
    items: Vec<PathInfo>,
    items_sorted: Vec<PathInfo>,
    search_root: Option<PathBuf>,
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
        let mut indices: Vec<usize> = (0..self.items.len()).collect();

        match sort_column {
            SortColumn::Name => {
                let keys: Vec<_> = self.items.iter().map(|p| p.name_comparator()).collect();
                indices.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
            }
            SortColumn::Modified => {
                let keys: Vec<_> = self.items.iter().map(|p| p.modified_comparator()).collect();
                indices.sort_by(|a, b| keys[*a].cmp(&keys[*b]));
            }
            SortColumn::Size => {
                indices.sort_by_key(|i| self.items[*i].size);
            }
        };
        if *sort_direction == SortDirection::Descending {
            indices.reverse();
        }

        if *sort_column == SortColumn::Name && Config::global().ui.sort_directories_first {
            indices.sort_by_key(|i| !self.items[*i].is_directory());
        }

        self.items_sorted = indices.into_iter().map(|i| self.items[i].clone()).collect();

        if !self.filter.is_empty() {
            let filter_lowercase = self.filter.to_lowercase();
            self.items_sorted
                .retain(|path| path.name().to_lowercase().contains(&filter_lowercase));
        }
    }

    pub(super) fn start_search(&mut self) {
        // The search root is always the current directory.
        self.search_root = self.directory.as_ref().map(|d| PathBuf::from(&d.path));
        self.items.clear();
        self.items_sorted.clear();
        self.filter.clear();
    }

    pub(super) fn append_search_result(&mut self, item: PathInfo) {
        self.items.push(item.clone());
        self.items_sorted.push(item);
    }

    pub(super) fn clear_search(&mut self) {
        self.search_root = None;
    }

    pub(super) fn is_searching(&self) -> bool {
        self.search_root.is_some()
    }

    pub(super) fn search_root(&self) -> Option<&Path> {
        self.search_root.as_deref()
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
