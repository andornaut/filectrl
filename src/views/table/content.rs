use std::path::{Path, PathBuf};

use super::columns::{SortColumn, SortDirection};
use crate::app::config::Config;
use crate::file_system::path_info::PathInfo;
use crate::views::ListingMode;

#[derive(Default)]
pub(super) struct DirectoryContent {
    directory: Option<PathInfo>,
    filter: String,
    items: Vec<PathInfo>,
    items_sorted: Vec<PathInfo>,
    /// Which listing is shown. Mode membership lives here; `search_root` and
    /// the bookmark items are per-mode data.
    mode: ListingMode,
    search_root: Option<PathBuf>,
    /// True while a directory's entries are still streaming in.
    loading: bool,
    /// Bumped whenever `items_sorted` or display-affecting state (search root,
    /// bookmarks mode) changes. Lets the view cache per-item row heights and
    /// invalidate them with a cheap equality check.
    revision: u64,
    /// Runtime override for showing hidden (dotfile) entries. `None` defers to
    /// the `ui.show_hidden_files` config value.
    show_hidden: Option<bool>,
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

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    #[cfg(test)]
    pub(super) fn set_items(&mut self, directory: PathInfo, items: Vec<PathInfo>) {
        self.directory = Some(directory);
        self.items = items;
    }

    /// Begin a streamed directory load: switch to `directory` and clear the
    /// listing. Entries arrive via `append_listing` and are sorted by
    /// `finalize_listing`. Filter/search/bookmarks state is left untouched (the
    /// caller decides what carries over for a navigate vs. a refresh).
    pub(super) fn start_listing(&mut self, directory: PathInfo) {
        self.directory = Some(directory);
        self.items.clear();
        self.items_sorted.clear();
        self.loading = true;
        self.revision += 1;
    }

    /// Append a streamed batch (directory entries or search results) in read
    /// order so partial results are visible immediately. The visibility
    /// predicate is applied per batch so mid-stream rendering honors it; the
    /// final ordering is applied once by `finalize_listing`.
    pub(super) fn append(&mut self, items: &[PathInfo]) {
        let visibility = self.visibility();
        self.items_sorted.extend(
            items
                .iter()
                .filter(|path| visibility.is_visible(path))
                .cloned(),
        );
        self.items.extend_from_slice(items);
        self.revision += 1;
    }

    /// Apply a listing-mode transition (see `ListingMode::transition`).
    /// Leaving search mode drops the search root, which doubles as
    /// display-affecting state for the relative-path rendering.
    pub(super) fn set_mode(&mut self, mode: ListingMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode != ListingMode::Search {
            self.search_root = None;
        }
        self.revision += 1;
    }

    pub(super) fn mode(&self) -> ListingMode {
        self.mode
    }

    /// The visibility predicate for the current mode, with per-entry state
    /// (lowercased filter, show-hidden setting) computed once. Both `append`
    /// and `sort` derive visibility from this so their semantics cannot drift.
    fn visibility(&self) -> Visibility {
        Visibility {
            // Search results bypass the show-hidden setting: the user
            // explicitly asked for name matches, so a hidden match must not
            // be dropped (neither mid-stream nor on a later re-sort).
            show_hidden: self.show_hidden() || self.is_searching(),
            filter_lowercase: self.filter.to_lowercase(),
        }
    }

    /// Finish a streamed load: sort the entries accumulated (and already
    /// filtered) by `append` once, in place. Visibility changes mid-stream go
    /// through `sort`, which re-derives from the unfiltered items.
    pub(super) fn finalize_listing(
        &mut self,
        sort_column: &SortColumn,
        sort_direction: &SortDirection,
    ) {
        self.loading = false;
        self.sort_in_place(sort_column, sort_direction);
        self.revision += 1;
    }

    pub(super) fn is_loading(&self) -> bool {
        self.loading
    }

    pub(super) fn set_filter(&mut self, filter: String) {
        self.filter = filter;
    }

    pub(super) fn clear_filter(&mut self) {
        self.filter.clear();
    }

    fn show_hidden(&self) -> bool {
        self.show_hidden
            .unwrap_or(Config::global().ui.show_hidden_files)
    }

    pub(super) fn toggle_show_hidden(&mut self) {
        self.show_hidden = Some(!self.show_hidden());
    }

    /// Sort and filter items into `items_sorted`. Visibility is re-derived
    /// from the unfiltered `items`, so a toggled show-hidden setting or filter
    /// change takes effect on the next sort.
    pub(super) fn sort(&mut self, sort_column: &SortColumn, sort_direction: &SortDirection) {
        let visibility = self.visibility();
        self.items_sorted = self
            .items
            .iter()
            .filter(|path| visibility.is_visible(path))
            .cloned()
            .collect();
        self.sort_in_place(sort_column, sort_direction);
        self.revision += 1;
    }

    /// Sort `items_sorted` without re-deriving visibility: `append` applies
    /// the same predicate, so finalizing a stream can skip the re-filter and
    /// re-clone of every entry.
    fn sort_in_place(&mut self, sort_column: &SortColumn, sort_direction: &SortDirection) {
        self.items_sorted.sort_by(|a, b| {
            let ord = match sort_column {
                SortColumn::Name => a.name_comparator().cmp(&b.name_comparator()),
                SortColumn::Modified => a.modified_comparator().cmp(&b.modified_comparator()),
                SortColumn::Size => a.size.cmp(&b.size),
            };
            if *sort_direction == SortDirection::Descending {
                ord.reverse()
            } else {
                ord
            }
        });

        if *sort_column == SortColumn::Name && Config::global().ui.sort_directories_first {
            self.items_sorted.sort_by_key(|path| !path.is_directory());
        }
    }

    pub(super) fn start_search(&mut self) {
        self.set_mode(ListingMode::Search);
        // The search root is always the current directory.
        self.search_root = self.directory.as_ref().map(|d| PathBuf::from(&d.path));
        self.items.clear();
        self.items_sorted.clear();
        self.filter.clear();
        self.revision += 1;
    }

    #[cfg(test)]
    pub(super) fn clear_search(&mut self) {
        self.set_mode(ListingMode::Normal);
    }

    pub(super) fn is_searching(&self) -> bool {
        self.mode == ListingMode::Search
    }

    pub(super) fn search_root(&self) -> Option<&Path> {
        self.search_root.as_deref()
    }

    /// Replace the listing with the given bookmarks (one synchronous batch,
    /// unlike streamed search results). The current `directory` is left
    /// untouched so breadcrumbs/CWD restore cleanly when the view is dismissed.
    pub(super) fn set_bookmarks(&mut self, items: Vec<PathInfo>) {
        self.set_mode(ListingMode::Bookmarks);
        self.filter.clear();
        self.items = items;
        self.items_sorted.clear();
        self.revision += 1;
    }

    pub(super) fn is_showing_bookmarks(&self) -> bool {
        self.mode == ListingMode::Bookmarks
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

/// Snapshot of the visibility predicate (see `DirectoryContent::visibility`).
struct Visibility {
    show_hidden: bool,
    filter_lowercase: String,
}

impl Visibility {
    fn is_visible(&self, path: &PathInfo) -> bool {
        (self.show_hidden || !path.is_hidden()) && self.matches_filter(&path.display_name)
    }

    /// Case-insensitive substring match on the display name. The common
    /// all-ASCII case compares in place instead of allocating a lowercased
    /// copy of every entry name.
    fn matches_filter(&self, name: &str) -> bool {
        if self.filter_lowercase.is_empty() {
            return true;
        }
        if self.filter_lowercase.is_ascii() && name.is_ascii() {
            let filter = self.filter_lowercase.as_bytes();
            return name
                .as_bytes()
                .windows(filter.len())
                .any(|window| window.eq_ignore_ascii_case(filter));
        }
        name.to_lowercase().contains(&self.filter_lowercase)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::config::RuntimeEnv;

    fn ensure_config_initialized() {
        let config = Config::load(RuntimeEnv::default(), None, vec![]).unwrap();
        Config::init(config);
    }

    /// Self-cleaning unique temp directory.
    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            // A per-process counter guarantees a unique directory even when two
            // fixtures are created in the same nanosecond on parallel threads,
            // so one fixture's Drop never wipes another's directory.
            static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("filectrl_content_{}_{seq}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn dir_entry(&self, name: &str) -> PathInfo {
            let path = self.dir.join(name);
            std::fs::create_dir_all(&path).unwrap();
            PathInfo::try_from(&path).unwrap()
        }

        fn file_entry(&self, name: &str, size: usize) -> PathInfo {
            let path = self.dir.join(name);
            std::fs::write(&path, vec![b'x'; size]).unwrap();
            PathInfo::try_from(&path).unwrap()
        }

        fn directory(&self) -> PathInfo {
            PathInfo::try_from(&self.dir).unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn names(content: &DirectoryContent) -> Vec<String> {
        content
            .items_sorted()
            .iter()
            .map(|p| p.display_name.clone())
            .collect()
    }

    #[test]
    fn sort_by_name_ascending_groups_directories_first_then_case_insensitive() {
        ensure_config_initialized();
        let fx = Fixture::new();
        // Intentionally unsorted input order.
        let items = vec![
            fx.file_entry("Banana", 1),
            fx.dir_entry("Apricot"),
            fx.file_entry("apple", 1),
            fx.file_entry(".secret", 1),
            fx.dir_entry("Apple"),
        ];
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), items);
        content.sort(&SortColumn::Name, &SortDirection::Ascending);

        // Directories first (config default sort_directories_first = true),
        // then files; comparison is case-insensitive and ignores a leading dot
        // (".secret" sorts as "secret").
        assert_eq!(
            names(&content),
            vec!["Apple", "Apricot", "apple", "Banana", ".secret"]
        );
    }

    #[test]
    fn sort_by_name_descending_reverses_within_the_directory_grouping() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![
            fx.dir_entry("Apple"),
            fx.dir_entry("Apricot"),
            fx.file_entry("apple", 1),
            fx.file_entry("Banana", 1),
        ];
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), items);
        content.sort(&SortColumn::Name, &SortDirection::Descending);

        // Descending reverses the name order, but directories are still grouped
        // ahead of files (the directories-first pass runs last and is stable).
        assert_eq!(names(&content), vec!["Apricot", "Apple", "Banana", "apple"]);
    }

    #[test]
    fn sort_by_size_orders_by_byte_length() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("medium", 50),
            fx.file_entry("small", 1),
            fx.file_entry("large", 500),
        ];
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), items);

        content.sort(&SortColumn::Size, &SortDirection::Ascending);
        assert_eq!(names(&content), vec!["small", "medium", "large"]);

        content.sort(&SortColumn::Size, &SortDirection::Descending);
        assert_eq!(names(&content), vec!["large", "medium", "small"]);
    }

    #[test]
    fn filter_retains_case_insensitive_substring_matches() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("Apple", 1),
            fx.file_entry("Apricot", 1),
            fx.file_entry("Banana", 1),
        ];
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), items);
        content.set_filter("ap".to_string());
        content.sort(&SortColumn::Name, &SortDirection::Ascending);

        assert_eq!(names(&content), vec!["Apple", "Apricot"]);

        content.clear_filter();
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn toggle_show_hidden_filters_dotfiles() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![fx.file_entry("visible", 1), fx.file_entry(".hidden", 1)];
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), items);

        // Default config has show_hidden_files = true.
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(content.len(), 2);

        // First toggle flips the runtime override to false.
        content.toggle_show_hidden();
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(names(&content), vec!["visible"]);

        content.toggle_show_hidden();
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn revision_changes_when_the_listing_changes_but_not_on_reads() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut content = DirectoryContent::default();

        let r0 = content.revision();
        content.start_listing(fx.directory());
        let r1 = content.revision();
        assert_ne!(r0, r1, "start_listing must bump the revision");

        content.append(&[fx.file_entry("a", 1)]);
        let r2 = content.revision();
        assert_ne!(r1, r2, "append must bump the revision");

        content.finalize_listing(&SortColumn::Name, &SortDirection::Ascending);
        let r3 = content.revision();
        assert_ne!(r2, r3, "finalize_listing (sort) must bump the revision");

        // Pure reads must not bump it (cache stays valid while only scrolling).
        let _ = content.items_sorted();
        let _ = content.len();
        assert_eq!(r3, content.revision());
    }

    #[test]
    fn streamed_listing_matches_set_items_then_sort() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("Banana", 1),
            fx.dir_entry("Apricot"),
            fx.file_entry("apple", 1),
            fx.dir_entry("Apple"),
        ];

        // Reference: the one-shot path.
        let mut reference = DirectoryContent::default();
        reference.set_items(fx.directory(), items.clone());
        reference.sort(&SortColumn::Name, &SortDirection::Ascending);

        // Streamed in two batches, then finalized once.
        let mut streamed = DirectoryContent::default();
        streamed.start_listing(fx.directory());
        streamed.append(&items[..2]);
        streamed.append(&items[2..]);
        streamed.finalize_listing(&SortColumn::Name, &SortDirection::Ascending);

        assert_eq!(names(&streamed), names(&reference));
    }

    #[test]
    fn listing_is_visible_in_read_order_before_finalize() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("c", 1),
            fx.file_entry("a", 1),
            fx.file_entry("b", 1),
        ];
        let mut content = DirectoryContent::default();
        content.start_listing(fx.directory());
        assert!(content.is_loading());

        content.append(&items);
        // Partial results are visible in read order before the final sort.
        assert_eq!(names(&content), vec!["c", "a", "b"]);

        content.finalize_listing(&SortColumn::Name, &SortDirection::Ascending);
        assert!(!content.is_loading());
        assert_eq!(names(&content), vec!["a", "b", "c"]);
    }

    #[test]
    fn appended_batches_honor_the_active_filter_before_finalize() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("Apple", 1),
            fx.file_entry("Banana", 1),
            fx.file_entry("Apricot", 1),
        ];
        let mut content = DirectoryContent::default();
        content.set_filter("ap".to_string());
        content.start_listing(fx.directory());

        content.append(&items);
        // Non-matching entries must not flash into view mid-stream.
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);

        content.finalize_listing(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);
    }

    #[test]
    fn appended_batches_honor_show_hidden_before_finalize() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![fx.file_entry("visible", 1), fx.file_entry(".hidden", 1)];
        let mut content = DirectoryContent::default();
        // Default config has show_hidden_files = true; toggle it off.
        content.toggle_show_hidden();
        content.start_listing(fx.directory());

        content.append(&items);
        // Hidden entries must not flash into view mid-stream.
        assert_eq!(names(&content), vec!["visible"]);

        content.finalize_listing(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(names(&content), vec!["visible"]);
    }

    #[test]
    fn search_results_bypass_the_show_hidden_filter() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), vec![]);
        // Default config has show_hidden_files = true; toggle it off.
        content.toggle_show_hidden();
        content.start_search();

        // A search explicitly matched these names, so hidden results are kept.
        content.append(&[fx.file_entry(".hidden", 1), fx.file_entry("visible", 1)]);
        assert_eq!(names(&content), vec![".hidden", "visible"]);

        // Re-sorting search results must not drop hidden matches either.
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(names(&content), vec![".hidden", "visible"]);
    }

    #[test]
    fn finalize_after_a_mid_stream_filter_change_matches_a_full_sort() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let mut content = DirectoryContent::default();
        content.start_listing(fx.directory());
        content.append(&[fx.file_entry("Banana", 1), fx.file_entry("Apple", 1)]);

        // A filter arrives mid-stream; `sort` re-derives from the unfiltered
        // items, after which finalize only has to order the survivors.
        content.set_filter("ap".to_string());
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        content.append(&[fx.file_entry("Apricot", 1), fx.file_entry("Cherry", 1)]);

        content.finalize_listing(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);
    }

    #[test]
    fn filter_is_case_insensitive_for_non_ascii_names() {
        ensure_config_initialized();
        let fx = Fixture::new();
        let items = vec![fx.file_entry("Équipe", 1), fx.file_entry("autre", 1)];
        let mut content = DirectoryContent::default();
        content.set_items(fx.directory(), items);
        content.set_filter("équipe".to_string());
        content.sort(&SortColumn::Name, &SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Équipe"]);
    }
}
