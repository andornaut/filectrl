use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use super::columns::{SortColumn, SortDirection};
use crate::app::config::UiConfig;
use crate::contains_ignore_case;
use crate::file_system::path_info::{PathInfo, name_key, visible_path};
use crate::views::ListingMode;

/// Not `Default`: a derived default would contradict the shipped config.
pub(super) struct DirectoryContent {
    directory: Option<PathInfo>,
    filter: String,
    items: Vec<PathInfo>,
    items_sorted: Vec<PathInfo>,
    mode: ListingMode,
    search_root: Option<PathBuf>,
    loading: bool,
    /// Entries of a reload of the directory shown, held back until
    /// `finalize_listing` swaps them in.
    staged: Option<Vec<PathInfo>>,
    /// Bumped whenever `items_sorted` or display-affecting state changes,
    /// except by `append`. Keys the view's row-height cache.
    revision: u64,
    show_hidden: bool,
    name_order: NameOrder,
    /// The order `items_sorted` is in, while it is exactly the visible `items`
    /// sorted that way.
    sorted_by: Option<(SortColumn, SortDirection)>,
}

/// How the Name column orders entries, fixed for the listing's life.
struct NameOrder {
    directories_first: bool,
    /// Whether runs of digits compare as numbers.
    natural: bool,
}

impl DirectoryContent {
    pub(super) fn new(ui: UiConfig) -> Self {
        Self {
            directory: None,
            filter: String::new(),
            items: Vec::new(),
            items_sorted: Vec::new(),
            mode: ListingMode::default(),
            search_root: None,
            loading: false,
            staged: None,
            revision: 0,
            show_hidden: ui.show_hidden_files,
            name_order: NameOrder {
                directories_first: ui.sort_directories_first,
                natural: ui.natural_sort,
            },
            sorted_by: None,
        }
    }

    pub(super) fn get(&self, index: usize) -> Option<&PathInfo> {
        self.items_sorted.get(index)
    }

    pub(super) fn len(&self) -> usize {
        self.items_sorted.len()
    }

    /// Every entry read, before the filter and the hidden-file setting.
    pub(super) fn total_len(&self) -> usize {
        self.items.len()
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
        self.sorted_by = None;
    }

    /// Begin a streamed directory load of `directory`. `staged` keeps the
    /// listing on screen (of this same directory) until the new one completes.
    pub(super) fn start_listing(&mut self, directory: PathInfo, staged: bool) {
        self.directory = Some(directory);
        self.loading = true;
        if staged {
            self.staged = Some(Vec::new());
            return;
        }
        self.staged = None;
        self.items.clear();
        self.items_sorted.clear();
        self.sorted_by = None;
        self.revision += 1;
    }

    /// Append a streamed batch in read order, filtered; `finalize_listing`
    /// sorts it. A staged load's batches only accumulate.
    pub(super) fn append(&mut self, items: &[PathInfo]) {
        if let Some(staged) = &mut self.staged {
            staged.extend_from_slice(items);
            return;
        }
        let visibility = self.visibility();
        self.items_sorted.extend(
            items
                .iter()
                .filter(|path| visibility.is_visible(path))
                .cloned(),
        );
        self.items.extend_from_slice(items);
        self.sorted_by = None;
    }

    /// Apply a listing-mode transition (see `ListingMode::transition`).
    pub(super) fn set_mode(&mut self, mode: ListingMode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.sorted_by = None;
        if mode != ListingMode::Search {
            self.search_root = None;
        }
        if mode == ListingMode::Normal {
            // The entries are of another root; a refresh must not stage onto them.
            self.items.clear();
            self.items_sorted.clear();
        } else {
            // A load cancelled mid-stream never finalizes.
            self.loading = false;
            self.staged = None;
        }
        self.revision += 1;
    }

    pub(super) fn mode(&self) -> ListingMode {
        self.mode
    }

    /// The visibility predicate for the current mode, shared by `append` and
    /// `sort`.
    fn visibility(&self) -> Visibility {
        Visibility {
            // Search results bypass the show-hidden setting.
            show_hidden: self.show_hidden() || self.is_searching(),
            filter_lowercase: self.filter.to_lowercase(),
            is_bookmarks: self.is_showing_bookmarks(),
            search_root: self.search_root.clone(),
        }
    }

    /// Finish a streamed load: sort the entries `append` accumulated, or swap
    /// in a staged load's entries and filter them.
    pub(super) fn finalize_listing(
        &mut self,
        sort_column: SortColumn,
        sort_direction: SortDirection,
    ) {
        self.loading = false;
        if let Some(staged) = self.staged.take() {
            self.items = staged;
            self.sort(sort_column, sort_direction);
            return;
        }
        self.sort_in_place(sort_column, sort_direction);
        self.sorted_by = Some((sort_column, sort_direction));
        self.revision += 1;
    }

    pub(super) fn is_loading(&self) -> bool {
        self.loading
    }

    /// Whether the load in flight is staged.
    pub(super) fn is_staged(&self) -> bool {
        self.staged.is_some()
    }

    #[cfg(test)]
    pub(super) fn set_filter(&mut self, filter: String) {
        self.filter = filter;
        self.sorted_by = None;
    }

    /// Filter the listing by `filter`, sorted by `sort_column`. A filter that
    /// extends the one applied narrows the sorted listing in place: the filter
    /// is a substring test, so a name holding the longer text holds the shorter.
    pub(super) fn apply_filter(
        &mut self,
        filter: String,
        sort_column: SortColumn,
        sort_direction: SortDirection,
    ) {
        let narrows = self.sorted_by == Some((sort_column, sort_direction))
            && filter.to_lowercase().contains(&self.filter.to_lowercase());
        self.filter = filter;
        if !narrows {
            self.sort(sort_column, sort_direction);
            return;
        }
        let visibility = self.visibility();
        self.items_sorted
            .retain(|path| visibility.matches_filter(path));
        self.revision += 1;
    }

    pub(super) fn clear_filter(&mut self) {
        self.filter.clear();
        self.sorted_by = None;
    }

    fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub(super) fn toggle_show_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.sorted_by = None;
    }

    /// Sort and filter the unfiltered `items` into `items_sorted`.
    pub(super) fn sort(&mut self, sort_column: SortColumn, sort_direction: SortDirection) {
        let visibility = self.visibility();
        self.items_sorted = self
            .items
            .iter()
            .filter(|path| visibility.is_visible(path))
            .cloned()
            .collect();
        self.sort_in_place(sort_column, sort_direction);
        self.sorted_by = Some((sort_column, sort_direction));
        self.revision += 1;
    }

    /// Sort `items_sorted` without re-deriving visibility.
    fn sort_in_place(&mut self, sort_column: SortColumn, sort_direction: SortDirection) {
        // Order by the displayed name (the relative path while searching).
        let search_root = self.search_root.clone();
        let natural = self.name_order.natural;
        let name_key =
            |item: &PathInfo| name_key(&displayed_name_stem(item, search_root.as_deref()), natural);
        // Keys are built once per entry because the name key allocates. Ties
        // break by ascending name. Every sort is stable, as the
        // directories-first pass requires.
        let descending = sort_direction == SortDirection::Descending;
        match (sort_column, descending) {
            (SortColumn::Name, true) => self
                .items_sorted
                .sort_by_cached_key(|item| Reverse(name_key(item))),
            (SortColumn::Name, false) => self.items_sorted.sort_by_cached_key(name_key),
            (SortColumn::Modified, true) => self
                .items_sorted
                .sort_by_cached_key(|item| (Reverse(item.modified_comparator()), name_key(item))),
            (SortColumn::Modified, false) => self
                .items_sorted
                .sort_by_cached_key(|item| (item.modified_comparator(), name_key(item))),
            (SortColumn::Size, true) => self
                .items_sorted
                .sort_by_cached_key(|item| (Reverse(item.size), name_key(item))),
            (SortColumn::Size, false) => self
                .items_sorted
                .sort_by_cached_key(|item| (item.size, name_key(item))),
        }

        if sort_column == SortColumn::Name && self.name_order.directories_first {
            self.items_sorted.sort_by_key(|path| !path.is_directory());
        }
    }

    pub(super) fn start_search(&mut self) {
        self.set_mode(ListingMode::Search);
        self.search_root = self.directory.as_ref().map(|d| PathBuf::from(&d.path));
        self.items.clear();
        self.items_sorted.clear();
        self.filter.clear();
        self.sorted_by = None;
        self.revision += 1;
    }

    /// Replaces the search results; the visible listing is left for `sort`.
    pub(super) fn replace_search_results(&mut self, items: Vec<PathInfo>) {
        self.items = items;
        self.sorted_by = None;
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

    /// Replace the listing with the given bookmarks, keeping `directory`. The
    /// visible listing is left for `sort`.
    pub(super) fn set_bookmarks(&mut self, items: Vec<PathInfo>) {
        self.set_mode(ListingMode::Bookmarks);
        self.filter.clear();
        self.items = items;
        self.sorted_by = None;
        self.revision += 1;
    }

    pub(super) fn is_showing_bookmarks(&self) -> bool {
        self.mode == ListingMode::Bookmarks
    }

    pub(super) fn find_by_inode(&self, path: &PathInfo) -> Option<usize> {
        self.items_sorted.iter().position(|p| p.is_same_inode(path))
    }

    /// The indices `paths` now occupy, for carrying marks across a reorder. By
    /// path, not inode: hard links share an inode.
    pub(super) fn find_all_by_path(&self, paths: &[PathInfo]) -> Vec<usize> {
        let wanted: HashSet<&Path> = paths.iter().map(PathInfo::as_path).collect();
        self.items_sorted
            .iter()
            .enumerate()
            .filter(|(_, item)| wanted.contains(item.as_path()))
            .map(|(index, _)| index)
            .collect()
    }

    pub(super) fn find_by_path(&self, target: &Path) -> Option<usize> {
        self.items_sorted
            .iter()
            .position(|item| item.as_path() == target)
    }
}

/// The name column's text: the entry's name, the path relative to the search
/// root while searching, or the bookmark name. Directories outside the
/// bookmarks view carry a trailing separator. The filter matches it too.
pub(super) fn displayed_name<'a>(
    item: &'a PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> Cow<'a, str> {
    let stem = displayed_name_stem(item, search_root);
    if displays_trailing_separator(item, is_bookmarks) {
        Cow::Owned(format!("{stem}{MAIN_SEPARATOR}"))
    } else {
        stem
    }
}

/// `displayed_name` without the trailing separator.
fn displayed_name_stem<'a>(item: &'a PathInfo, search_root: Option<&Path>) -> Cow<'a, str> {
    match search_root {
        Some(root) => Cow::Owned(visible_path(
            item.path.strip_prefix(root).unwrap_or(&item.path),
        )),
        _ => Cow::Borrowed(&item.display_name),
    }
}

/// Whether `displayed_name` appends a separator to the stem.
fn displays_trailing_separator(item: &PathInfo, is_bookmarks: bool) -> bool {
    !is_bookmarks && item.is_directory()
}

/// Snapshot of the visibility predicate.
struct Visibility {
    show_hidden: bool,
    filter_lowercase: String,
    /// Owned: `DirectoryContent` is mutated while the predicate is live.
    is_bookmarks: bool,
    search_root: Option<PathBuf>,
}

impl Visibility {
    fn is_visible(&self, path: &PathInfo) -> bool {
        (self.show_hidden || !path.is_hidden()) && self.matches_filter(path)
    }

    /// Case-insensitive substring match on the displayed name, matched against
    /// the stem plus a rule for the trailing separator to avoid allocating.
    fn matches_filter(&self, path: &PathInfo) -> bool {
        if self.filter_lowercase.is_empty() {
            return true;
        }
        let stem = displayed_name_stem(path, self.search_root.as_deref());
        if contains_ignore_case(&stem, &self.filter_lowercase) {
            return true;
        }
        let Some(prefix) = self.filter_lowercase.strip_suffix(MAIN_SEPARATOR) else {
            return false;
        };
        displays_trailing_separator(path, self.is_bookmarks) && ends_with_ignore_case(&stem, prefix)
    }
}

/// Case-insensitive `str::ends_with`; `suffix_lowercase` must be lowercase.
fn ends_with_ignore_case(haystack: &str, suffix_lowercase: &str) -> bool {
    if suffix_lowercase.is_ascii() && haystack.is_ascii() {
        let suffix = suffix_lowercase.as_bytes();
        let bytes = haystack.as_bytes();
        return bytes.len() >= suffix.len()
            && bytes[bytes.len() - suffix.len()..].eq_ignore_ascii_case(suffix);
    }
    haystack.to_lowercase().ends_with(suffix_lowercase)
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;
    use crate::{app::config::Config, test_support::TempDir};

    /// A listing built with the shipped settings.
    fn content() -> DirectoryContent {
        Config::init_test();
        DirectoryContent::new(Config::global().ui)
    }

    /// The shipped settings with the two listing settings a test is about.
    fn ui(show_hidden_files: bool, sort_directories_first: bool) -> UiConfig {
        UiConfig {
            show_hidden_files,
            sort_directories_first,
            ..Config::global().ui
        }
    }

    fn names(content: &DirectoryContent) -> Vec<String> {
        content
            .items_sorted()
            .iter()
            .map(|p| p.display_name.clone())
            .collect()
    }

    // Linux only: needs "Apple" and "apple" as distinct entries.
    #[cfg(target_os = "linux")]
    #[test]
    fn sort_by_name_ascending_groups_directories_first_then_case_insensitive() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("Banana", 1),
            fx.subdirectory("Apricot"),
            fx.file("apple", 1),
            fx.file(".secret", 1),
            fx.subdirectory("Apple"),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);
        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(
            names(&content),
            vec!["Apple", "Apricot", "apple", "Banana", ".secret"]
        );
    }

    // Linux only: needs "Apple" and "apple" as distinct entries.
    #[cfg(target_os = "linux")]
    #[test]
    fn sort_by_name_descending_reverses_within_the_directory_grouping() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.subdirectory("Apple"),
            fx.subdirectory("Apricot"),
            fx.file("apple", 1),
            fx.file("Banana", 1),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);
        content.sort(SortColumn::Name, SortDirection::Descending);

        assert_eq!(names(&content), vec!["Apricot", "Apple", "Banana", "apple"]);
    }

    #[test]
    fn sort_by_size_orders_by_byte_length() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("medium", 50),
            fx.file("small", 1),
            fx.file("large", 500),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Size, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["small", "medium", "large"]);

        content.sort(SortColumn::Size, SortDirection::Descending);
        assert_eq!(names(&content), vec!["large", "medium", "small"]);
    }

    #[test_case("ap", &["Apple", "Apricot"] ; "lowercase")]
    #[test_case("AP", &["Apple", "Apricot"] ; "uppercase")]
    #[test_case("ÉQ", &["Équipe"] ; "uppercase outside ascii")]
    fn filter_retains_case_insensitive_substring_matches(filter: &str, expected: &[&str]) {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("Apple", 1),
            fx.file("Apricot", 1),
            fx.file("Banana", 1),
            fx.file("Équipe", 1),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);
        content.set_filter(filter.to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(names(&content), expected);

        content.clear_filter();
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(content.len(), 4);
    }

    #[test]
    fn sort_by_modified_orders_by_age() {
        Config::init_test();
        let fx = TempDir::new("content");
        let now = chrono::Local::now();
        let aged = |name: &str, hours: i64| {
            let mut entry = fx.file(name, 1);
            entry.modified = Some(now - chrono::Duration::hours(hours));
            entry
        };
        let items = vec![aged("hour", 1), aged("day", 24), aged("now", 0)];
        let mut content = content();
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Modified, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["day", "hour", "now"]);

        content.sort(SortColumn::Modified, SortDirection::Descending);
        assert_eq!(names(&content), vec!["now", "hour", "day"]);
    }

    #[test]
    fn directories_are_grouped_first_only_under_a_name_sort() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("small", 1),
            fx.subdirectory("dir"),
            fx.file("large", 1_000_000),
        ];
        let mut content = DirectoryContent::new(ui(true, true));
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Size, SortDirection::Descending);

        // A directory's own size varies by filesystem.
        assert_eq!("large", names(&content)[0]);
    }

    #[test]
    fn toggle_show_hidden_filters_dotfiles() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![fx.file("visible", 1), fx.file(".hidden", 1)];
        let mut content = content();
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(content.len(), 2);

        content.toggle_show_hidden();
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["visible"]);

        content.toggle_show_hidden();
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn revision_changes_when_the_listing_changes_but_not_on_reads() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();

        let r0 = content.revision();
        content.start_listing(fx.directory(), false);
        let r1 = content.revision();
        assert_ne!(r0, r1, "start_listing must bump the revision");

        content.append(&[fx.file("a", 1)]);
        let r2 = content.revision();
        assert_eq!(r1, r2, "append must leave the revision alone");

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        let r3 = content.revision();
        assert_ne!(r2, r3, "finalize_listing (sort) must bump the revision");

        let _ = content.items_sorted();
        let _ = content.len();
        assert_eq!(r3, content.revision());
    }

    // Linux only: needs "Apple" and "apple" as distinct entries.
    #[cfg(target_os = "linux")]
    #[test]
    fn streamed_listing_matches_set_items_then_sort() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("Banana", 1),
            fx.subdirectory("Apricot"),
            fx.file("apple", 1),
            fx.subdirectory("Apple"),
        ];

        let mut reference = content();
        reference.set_items(fx.directory(), items.clone());
        reference.sort(SortColumn::Name, SortDirection::Ascending);

        let mut streamed = content();
        streamed.start_listing(fx.directory(), false);
        streamed.append(&items[..2]);
        streamed.append(&items[2..]);
        streamed.finalize_listing(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(names(&streamed), names(&reference));
    }

    #[test]
    fn listing_is_visible_in_read_order_before_finalize() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![fx.file("c", 1), fx.file("a", 1), fx.file("b", 1)];
        let mut content = content();
        content.start_listing(fx.directory(), false);
        assert!(content.is_loading());

        content.append(&items);
        assert_eq!(names(&content), vec!["c", "a", "b"]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert!(!content.is_loading());
        assert_eq!(names(&content), vec!["a", "b", "c"]);
    }

    #[test]
    fn a_staged_listing_replaces_the_visible_one_only_at_finalize() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file("a", 1), fx.file("b", 1)]);
        content.sort(SortColumn::Name, SortDirection::Ascending);
        let revision = content.revision();

        content.start_listing(fx.directory(), true);
        content.append(&[fx.file("c", 1), fx.file("b", 1)]);

        assert_eq!(names(&content), vec!["a", "b"]);
        assert_eq!(revision, content.revision());

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["b", "c"]);
        assert_ne!(revision, content.revision());
    }

    #[test]
    fn a_staged_listing_is_filtered_when_it_is_swapped_in() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file("Apple", 1)]);
        content.set_filter("ap".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);

        content.start_listing(fx.directory(), true);
        content.append(&[fx.file("Apricot", 1), fx.file("Banana", 1)]);
        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(names(&content), vec!["Apricot"]);
    }

    // The directory sorts last by name, so only grouping puts it first.
    #[test_case(true, &["zdir", "afile", ".hidden"]   ; "directories are grouped first")]
    #[test_case(false, &["afile", ".hidden", "zdir"]  ; "one flat name order")]
    fn the_listing_obeys_the_settings_it_was_built_with(
        directories_first: bool,
        expected: &[&str],
    ) {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = DirectoryContent::new(ui(true, directories_first));
        content.set_items(
            fx.directory(),
            vec![
                fx.file("afile", 1),
                fx.subdirectory("zdir"),
                fx.file(".hidden", 1),
            ],
        );

        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(expected, names(&content));
    }

    #[test_case(true, &["file1", "file2", "file10"] ; "natural reads the numbers")]
    #[test_case(false, &["file1", "file10", "file2"] ; "plain compares characters")]
    fn the_name_order_follows_the_natural_sort_setting(natural_sort: bool, expected: &[&str]) {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = DirectoryContent::new(UiConfig {
            natural_sort,
            ..Config::global().ui
        });
        content.set_items(
            fx.directory(),
            ["file10", "file2", "file1"]
                .map(|name| fx.file(name, 1))
                .to_vec(),
        );

        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(expected, names(&content));
    }

    /// Arrival order `b`, `a`, `c` would survive a sort that left ties alone.
    #[test_case(SortDirection::Ascending, &["b", "a", "c", "big"], &["a", "b", "c", "big"] ; "ascending")]
    #[test_case(SortDirection::Descending, &["b", "a", "c", "big"], &["big", "a", "b", "c"] ; "descending")]
    fn a_size_tie_is_broken_by_name(direction: SortDirection, arrival: &[&str], expected: &[&str]) {
        let fx = TempDir::new("content");
        let mut content = content();
        let items = arrival
            .iter()
            .map(|&name| fx.file(name, if name == "big" { 100 } else { 1 }))
            .collect();
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Size, direction);

        assert_eq!(expected, names(&content));
    }

    /// Arrival order `b`, `a`, `c` would survive a sort that left ties alone.
    #[test_case(SortDirection::Ascending, &["a", "b", "c", "new"] ; "ascending")]
    #[test_case(SortDirection::Descending, &["new", "a", "b", "c"] ; "descending")]
    fn a_modified_tie_is_broken_by_name(direction: SortDirection, expected: &[&str]) {
        use std::time::{Duration, SystemTime};

        let fx = TempDir::new("content");
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let items = ["b", "a", "c", "new"]
            .iter()
            .map(|&name| {
                let path = fx.join(name);
                let file = std::fs::File::create(&path).unwrap();
                let modified = if name == "new" {
                    old + Duration::from_mins(1)
                } else {
                    old
                };
                file.set_modified(modified).unwrap();
                PathInfo::try_from(path.as_path()).unwrap()
            })
            .collect();
        let mut content = content();
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Modified, direction);

        assert_eq!(expected, names(&content));
    }

    #[test]
    fn a_listing_built_without_hidden_files_never_lists_them() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = DirectoryContent::new(ui(false, true));
        content.set_items(
            fx.directory(),
            vec![fx.file("file", 1), fx.file(".hidden", 1)],
        );

        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(vec!["file"], names(&content));
    }

    #[test]
    fn returning_to_the_plain_listing_drops_the_entries_of_the_mode_it_left() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file("a", 1)]);
        content.sort(SortColumn::Name, SortDirection::Ascending);
        content.start_search();
        content.append(&[fx.nested("sub", "hit")]);
        assert_eq!(names(&content), vec!["hit"]);

        content.set_mode(ListingMode::Normal);
        assert!(names(&content).is_empty());
    }

    #[test_case(ListingMode::Normal ; "for the plain listing")]
    #[test_case(ListingMode::Bookmarks ; "for the bookmarks")]
    fn leaving_a_search_drops_its_root(mode: ListingMode) {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![]);
        content.start_search();
        assert!(content.search_root().is_some());

        content.set_mode(mode);

        assert_eq!(None, content.search_root());
    }

    #[test]
    fn starting_a_search_drops_the_filter() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![]);
        content.set_filter("zzz".to_string());

        content.start_search();
        content.append(&[fx.file("hit", 1)]);

        assert_eq!(names(&content), vec!["hit"]);
    }

    #[test]
    fn showing_the_bookmarks_drops_the_filter() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_filter("zzz".to_string());

        content.set_bookmarks(vec![fx.subdirectory("mark")]);
        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(names(&content), vec!["mark"]);
    }

    #[test]
    fn a_staged_listing_abandoned_by_a_search_is_dropped() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file("a", 1)]);
        content.sort(SortColumn::Name, SortDirection::Ascending);

        content.start_listing(fx.directory(), true);
        content.append(&[fx.file("b", 1)]);
        content.start_search();
        assert!(!content.is_loading());
        content.append(&[fx.file("hit", 1)]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["hit"]);
    }

    #[test]
    fn appended_batches_honor_the_active_filter_before_finalize() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("Apple", 1),
            fx.file("Banana", 1),
            fx.file("Apricot", 1),
        ];
        let mut content = content();
        content.set_filter("ap".to_string());
        content.start_listing(fx.directory(), false);

        content.append(&items);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);
    }

    #[test]
    fn appended_batches_honor_show_hidden_before_finalize() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![fx.file("visible", 1), fx.file(".hidden", 1)];
        let mut content = content();
        // Default config has show_hidden_files = true; toggle it off.
        content.toggle_show_hidden();
        content.start_listing(fx.directory(), false);

        content.append(&items);
        assert_eq!(names(&content), vec!["visible"]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["visible"]);
    }

    #[test]
    fn search_results_bypass_the_show_hidden_filter() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(fx.directory(), vec![]);
        // Default config has show_hidden_files = true; toggle it off.
        content.toggle_show_hidden();
        content.start_search();

        content.append(&[fx.file(".hidden", 1), fx.file("visible", 1)]);
        assert_eq!(names(&content), vec![".hidden", "visible"]);

        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec![".hidden", "visible"]);
    }

    #[test]
    fn finalize_after_a_mid_stream_filter_change_matches_a_full_sort() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.start_listing(fx.directory(), false);
        content.append(&[fx.file("Banana", 1), fx.file("Apple", 1)]);

        content.set_filter("ap".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        content.append(&[fx.file("Apricot", 1), fx.file("Cherry", 1)]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);
    }

    /// Every narrowing step equals filtering and sorting afresh.
    #[test]
    fn a_narrowed_filter_matches_a_fresh_filter_and_sort() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.file("report10.txt", 3),
            fx.subdirectory("Reports"),
            fx.file("report2.txt", 1),
            fx.file("Équipe report", 2),
            fx.file(".report", 5),
            fx.file("other", 4),
            fx.file("REPORT1.TXT", 3),
        ];
        let steps = ["", "r", "rep", "REPO", "report", "report1", "rt"];
        let orders = [
            (SortColumn::Name, SortDirection::Ascending),
            (SortColumn::Name, SortDirection::Descending),
            (SortColumn::Size, SortDirection::Descending),
            (SortColumn::Modified, SortDirection::Ascending),
        ];
        for (column, direction) in orders {
            let mut typed = content();
            typed.set_items(fx.directory(), items.clone());
            typed.sort(column, direction);
            for filter in steps {
                typed.apply_filter(filter.to_string(), column, direction);

                let mut fresh = content();
                fresh.set_items(fx.directory(), items.clone());
                fresh.set_filter(filter.to_string());
                fresh.sort(column, direction);
                assert_eq!(names(&fresh), names(&typed), "{filter:?} by {column:?}");
            }
        }
    }

    /// A listing reordered behind the content's back: narrowing keeps that
    /// order, a widened or replaced filter sorts again.
    #[test]
    fn only_a_filter_holding_the_last_one_narrows_the_shown_listing() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_items(
            fx.directory(),
            vec![fx.file("ab", 1), fx.file("abc", 1), fx.file("b", 1)],
        );
        let (column, direction) = (SortColumn::Name, SortDirection::Ascending);
        content.apply_filter("a".to_string(), column, direction);
        content.items_sorted.reverse();

        content.apply_filter("ab".to_string(), column, direction);
        assert_eq!(vec!["abc", "ab"], names(&content));

        content.apply_filter("b".to_string(), column, direction);
        assert_eq!(vec!["ab", "abc", "b"], names(&content));

        content.items_sorted.reverse();
        content.apply_filter("b".to_string(), column, SortDirection::Descending);
        assert_eq!(vec!["b", "abc", "ab"], names(&content));
    }

    #[test]
    fn a_listing_narrows_in_place_only_while_it_is_sorted() {
        Config::init_test();
        let fx = TempDir::new("content");
        let (column, direction) = (SortColumn::Name, SortDirection::Ascending);
        let mut content = content();
        content.start_listing(fx.directory(), false);
        content.append(&[fx.file("ab", 1), fx.file("abc", 1)]);
        content.finalize_listing(column, direction);
        content.items_sorted.reverse();

        content.apply_filter("a".to_string(), column, direction);
        assert_eq!(vec!["abc", "ab"], names(&content));

        content.append(&[fx.file("aa", 1)]);
        content.apply_filter("ab".to_string(), column, direction);
        assert_eq!(vec!["ab", "abc"], names(&content));
    }

    #[test]
    fn filter_agrees_with_a_substring_search_of_the_displayed_name() {
        Config::init_test();
        let fx = TempDir::new("content");
        let entries = [
            fx.subdirectory("reports"),
            fx.subdirectory("Équipe"),
            fx.file("report.txt", 1),
            fx.file("Équipe.txt", 1),
            fx.file("a", 1),
            fx.nested("reports", "inner.txt"),
        ];
        let filters = [
            "",
            "/",
            "//",
            "s/",
            "report",
            "reports",
            "reports/",
            "report/",
            "reports/x",
            "reports/inner.txt",
            "orts/inn",
            "inner",
            "a/b",
            "a//",
            "REPORTS/",
            "équipe",
            "équipe/",
            "ÉQUIPE/",
            "z",
        ];
        let modes = [
            (false, None),
            (false, Some(fx.path().to_path_buf())),
            (true, None),
        ];

        for (is_bookmarks, search_root) in modes {
            for filter in filters {
                let filter_lowercase = filter.to_lowercase();
                let visibility = Visibility {
                    show_hidden: true,
                    filter_lowercase: filter_lowercase.clone(),
                    is_bookmarks,
                    search_root: search_root.clone(),
                };
                for entry in &entries {
                    let displayed = displayed_name(entry, is_bookmarks, search_root.as_deref());
                    let expected = filter_lowercase.is_empty()
                        || displayed.to_lowercase().contains(&filter_lowercase);
                    assert_eq!(
                        expected,
                        visibility.matches_filter(entry),
                        "filter {filter:?} against {displayed:?} \
                         (is_bookmarks={is_bookmarks}, search_root={search_root:?})"
                    );
                }
            }
        }
    }

    #[test]
    fn displayed_name_per_listing_mode() {
        Config::init_test();
        let fx = TempDir::new("content");
        let dir = fx.subdirectory("reports");
        let nested = fx.nested("reports", "inner.txt");

        // Plain listing: the entry's own name, directories separator-suffixed.
        assert_eq!("reports/", displayed_name(&dir, false, None));
        assert_eq!("inner.txt", displayed_name(&nested, false, None));

        // Searching: the path relative to the search root.
        let root = Some(fx.path());
        assert_eq!("reports/", displayed_name(&dir, false, root));
        assert_eq!("reports/inner.txt", displayed_name(&nested, false, root));

        // Bookmarks: the bare name, with no separator appended.
        assert_eq!("reports", displayed_name(&dir, true, None));
        assert_eq!("inner.txt", displayed_name(&nested, true, None));
    }

    #[test]
    fn filter_matches_the_relative_path_of_search_results() {
        Config::init_test();
        let fx = TempDir::new("content");
        let items = vec![
            fx.subdirectory("reports"),
            fx.nested("reports", "inner.txt"),
            fx.file("other.txt", 1),
        ];
        let mut content = content();
        content.set_items(fx.directory(), vec![]);
        content.start_search();
        content.append(&items);

        content.set_filter("reports/".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["reports", "inner.txt"]);

        // A fragment spanning the separator matches only the nested file.
        content.set_filter("orts/inn".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["inner.txt"]);
    }

    #[test]
    fn a_search_result_spells_out_a_disguising_name() {
        let fx = TempDir::new("content");
        let item = fx.nested("sub", "a\u{202e}b");

        let name = displayed_name(&item, false, Some(fx.path()));

        assert_eq!("sub/a\\u{202e}b", name);
    }

    #[test]
    fn filter_finds_no_separator_in_bookmark_rows() {
        Config::init_test();
        let fx = TempDir::new("content");
        let mut content = content();
        content.set_bookmarks(vec![fx.subdirectory("reports"), fx.file("report.txt", 1)]);

        content.set_filter("/".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert!(names(&content).is_empty());

        content.set_filter("report".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["reports", "report.txt"]);
    }
}
