use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{MAIN_SEPARATOR, Path, PathBuf};

use super::columns::{SortColumn, SortDirection};
use crate::file_system::path_info::{PathInfo, name_comparator};
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
    /// Entries of a staged load, held back until `finalize_listing` swaps them
    /// in. `Some` for a reload of the directory already shown, whose listing
    /// stays valid until the new one is complete; `None` for a load that has
    /// nothing valid to show and so clears the listing up front.
    staged: Option<Vec<PathInfo>>,
    /// Bumped whenever `items_sorted` or display-affecting state (search root,
    /// bookmarks mode) changes. Lets the view cache per-item row heights and
    /// invalidate them with a cheap equality check.
    revision: u64,
    /// Whether hidden (dotfile) entries are listed. Seeded from
    /// `ui.show_hidden_files` and toggled at runtime.
    show_hidden: bool,
    /// Whether directories are grouped ahead of files under a name sort.
    /// Seeded from `ui.sort_directories_first`.
    sort_directories_first: bool,
}

impl DirectoryContent {
    /// The listing settings come from the config once, at construction, rather
    /// than being read on every sort: a listing has to behave the same way for
    /// its whole life, and a test has to be able to state the settings it is
    /// about.
    pub(super) fn new(show_hidden: bool, sort_directories_first: bool) -> Self {
        Self {
            show_hidden,
            sort_directories_first,
            ..Self::default()
        }
    }

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

    /// Begin a streamed directory load: switch to `directory`, and either stage
    /// the incoming entries or clear the listing for them. Entries arrive via
    /// `append` and are applied by `finalize_listing`. Filter/search/bookmarks
    /// state is left untouched (the caller decides what carries over for a
    /// navigate vs. a refresh).
    ///
    /// `staged` says the listing on screen is of this same directory and stays
    /// valid until the new one is complete, so a directory being written to
    /// does not blank and repaint once per watcher refresh. Nothing visible
    /// changes then, so the revision is left alone.
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
        self.revision += 1;
    }

    /// Append a streamed batch (directory entries or search results) in read
    /// order so partial results are visible immediately. The visibility
    /// predicate is applied per batch so mid-stream rendering honors it; the
    /// final ordering is applied once by `finalize_listing`. A staged load
    /// shows nothing until it completes, so its batches only accumulate.
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
        if mode == ListingMode::Normal {
            // Entering the plain listing from a search or the bookmarks view,
            // whose entries are of another root and describe nothing in this
            // directory. The refresh that follows would otherwise stage onto
            // them, leaving them on screen as this directory's own, and
            // rendered as bare names now that the search root is gone.
            self.items.clear();
            self.items_sorted.clear();
        } else {
            // A load cancelled mid-stream never finalizes, so leaving the
            // plain-listing flow must clear the loading flag; a stale value
            // would let late batches through the table's accept guard. Its
            // staged entries go with it: nothing will ever swap them in, and
            // the next load must not inherit them.
            self.loading = false;
            self.staged = None;
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
            is_bookmarks: self.is_showing_bookmarks(),
            search_root: self.search_root.clone(),
        }
    }

    /// Finish a streamed load: sort the entries accumulated (and already
    /// filtered) by `append` once, in place. Visibility changes mid-stream go
    /// through `sort`, which re-derives from the unfiltered items.
    ///
    /// A staged load replaces the listing here instead, in one step: its
    /// entries skipped `append`'s per-batch filter, so `sort` re-derives
    /// visibility from them.
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
        self.revision += 1;
    }

    pub(super) fn is_loading(&self) -> bool {
        self.loading
    }

    /// Whether the load in flight is staged, and so has left the listing that
    /// is on screen live for its whole duration.
    pub(super) fn is_staged(&self) -> bool {
        self.staged.is_some()
    }

    pub(super) fn set_filter(&mut self, filter: String) {
        self.filter = filter;
    }

    pub(super) fn clear_filter(&mut self) {
        self.filter.clear();
    }

    fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    pub(super) fn toggle_show_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
    }

    /// Sort and filter items into `items_sorted`. Visibility is re-derived
    /// from the unfiltered `items`, so a toggled show-hidden setting or filter
    /// change takes effect on the next sort.
    pub(super) fn sort(&mut self, sort_column: SortColumn, sort_direction: SortDirection) {
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
    fn sort_in_place(&mut self, sort_column: SortColumn, sort_direction: SortDirection) {
        // The Name column shows the path relative to the search root while
        // searching, not the entry's own name. Order by that same string, or the
        // listing looks unsorted (`z/apple.txt` above `a/zebra.txt`). The filter
        // matches the displayed name for the same reason. Read before the sort
        // borrows `items_sorted` mutably.
        let is_bookmarks = self.is_showing_bookmarks();
        let search_root = self.search_root.clone();
        let name_key = |item: &PathInfo| {
            name_comparator(&displayed_name_stem(
                item,
                is_bookmarks,
                search_root.as_deref(),
            ))
        };
        // Sorted by key rather than by comparator: the name key allocates twice
        // to build, and a comparator builds one per side of every comparison,
        // which a listing of any size pays n log n times over. `Reverse` rather
        // than reversing the sorted listing, so that entries sharing a key keep
        // the order they arrived in whichever way the column points. Every sort
        // here is stable, as the directories-first pass below requires.
        let descending = sort_direction == SortDirection::Descending;
        match (sort_column, descending) {
            (SortColumn::Name, false) => self.items_sorted.sort_by_cached_key(name_key),
            (SortColumn::Name, true) => self
                .items_sorted
                .sort_by_cached_key(|item| Reverse(name_key(item))),
            (SortColumn::Modified, false) => {
                self.items_sorted.sort_by_key(PathInfo::modified_comparator);
            }
            (SortColumn::Modified, true) => self
                .items_sorted
                .sort_by_key(|item| Reverse(item.modified_comparator())),
            (SortColumn::Size, false) => self.items_sorted.sort_by_key(|item| item.size),
            (SortColumn::Size, true) => self.items_sorted.sort_by_key(|item| Reverse(item.size)),
        }

        if sort_column == SortColumn::Name && self.sort_directories_first {
            self.items_sorted.sort_by_key(|path| !path.is_directory());
        }
    }

    pub(super) fn start_search(&mut self) {
        self.set_mode(ListingMode::Search);
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

    /// The indices `paths` now occupy, for carrying marks across a reorder.
    /// One pass over the listing rather than a scan per path, so marking every
    /// result of a large search stays linear. An entry the reorder dropped
    /// (filtered out, or gone from the listing) simply has no index.
    ///
    /// By path, not inode: two hard links share a device and inode, so inode
    /// identity would spread one mark onto every name the file has. A path
    /// appears at most once, which is the identity a mark needs.
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

/// The name the table shows in its name column: the entry's own name in a plain
/// listing, the path relative to the search root while searching, the bookmark
/// name in the bookmarks view. Directories outside that view carry a trailing
/// separator.
///
/// Shared by the name column and the filter so the two cannot disagree about
/// what a row is called. Borrowed where possible: both run over every item.
pub(super) fn displayed_name<'a>(
    item: &'a PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> Cow<'a, str> {
    let stem = displayed_name_stem(item, is_bookmarks, search_root);
    if displays_trailing_separator(item, is_bookmarks, &stem) {
        Cow::Owned(format!("{stem}{MAIN_SEPARATOR}"))
    } else {
        stem
    }
}

/// `displayed_name` without the trailing separator, which the filter matches
/// by rule instead of by building the joined string for every directory entry.
fn displayed_name_stem<'a>(
    item: &'a PathInfo,
    is_bookmarks: bool,
    search_root: Option<&Path>,
) -> Cow<'a, str> {
    match search_root {
        // Bookmarks win: that listing is the bookmarks directory, so a search
        // root does not describe its entries.
        Some(root) if !is_bookmarks => item
            .path
            .strip_prefix(root)
            .unwrap_or(&item.path)
            .to_string_lossy(),
        _ => Cow::Borrowed(&item.display_name),
    }
}

/// Whether `displayed_name` appends a separator to `stem`.
fn displays_trailing_separator(item: &PathInfo, is_bookmarks: bool, stem: &str) -> bool {
    !is_bookmarks && item.is_directory() && !stem.ends_with(MAIN_SEPARATOR)
}

/// Snapshot of the visibility predicate (see `DirectoryContent::visibility`).
struct Visibility {
    show_hidden: bool,
    filter_lowercase: String,
    /// The name column's inputs, so the filter matches the displayed name.
    /// Owned rather than borrowed from `DirectoryContent`, which is mutated
    /// while the predicate is live.
    is_bookmarks: bool,
    search_root: Option<PathBuf>,
}

impl Visibility {
    fn is_visible(&self, path: &PathInfo) -> bool {
        (self.show_hidden || !path.is_hidden()) && self.matches_filter(path)
    }

    /// Case-insensitive substring match on the displayed name, so the filter
    /// acts on what the row says (see `displayed_name`).
    ///
    /// Matched against the stem plus a rule for the trailing separator rather
    /// than the joined name, which would allocate for every directory entry on
    /// every keystroke. A match reaching the separator has to end there, so the
    /// separator is the filter's last character and the rest is a suffix of the
    /// stem.
    fn matches_filter(&self, path: &PathInfo) -> bool {
        if self.filter_lowercase.is_empty() {
            return true;
        }
        let stem = displayed_name_stem(path, self.is_bookmarks, self.search_root.as_deref());
        if contains_ignore_case(&stem, &self.filter_lowercase) {
            return true;
        }
        let Some(prefix) = self.filter_lowercase.strip_suffix(MAIN_SEPARATOR) else {
            return false;
        };
        displays_trailing_separator(path, self.is_bookmarks, &stem)
            && ends_with_ignore_case(&stem, prefix)
    }
}

/// Case-insensitive `str::contains`. The common all-ASCII case compares in
/// place instead of allocating a lowercased copy of every entry name.
/// `needle_lowercase` must already be lowercased and non-empty.
fn contains_ignore_case(haystack: &str, needle_lowercase: &str) -> bool {
    if needle_lowercase.is_ascii() && haystack.is_ascii() {
        let needle = needle_lowercase.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle));
    }
    haystack.to_lowercase().contains(needle_lowercase)
}

/// Case-insensitive `str::ends_with`. `suffix_lowercase` must already be
/// lowercased.
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

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new("content");
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

        /// A file one level down, so a search rooted at the fixture renders it
        /// with a separator in the middle of its name.
        fn nested_file_entry(&self, dir: &str, name: &str) -> PathInfo {
            let path = self.dir.join(dir).join(name);
            std::fs::create_dir_all(self.dir.join(dir)).unwrap();
            std::fs::write(&path, b"x").unwrap();
            PathInfo::try_from(&path).unwrap()
        }

        fn directory(&self) -> PathInfo {
            PathInfo::try_from(self.dir.path()).unwrap()
        }
    }

    /// A listing built with the shipped settings, so a test states only the
    /// setting it is about. The app builds one from the config it loaded.
    fn content() -> DirectoryContent {
        Config::init_test();
        let ui = Config::global().ui;
        DirectoryContent::new(ui.show_hidden_files, ui.sort_directories_first)
    }

    fn names(content: &DirectoryContent) -> Vec<String> {
        content
            .items_sorted()
            .iter()
            .map(|p| p.display_name.clone())
            .collect()
    }

    // Linux only: the fixture needs a directory "Apple" and a file "apple" as
    // distinct entries, which a case-insensitive filesystem cannot represent.
    // The comparison under test is pure and platform-independent.
    #[cfg(target_os = "linux")]
    #[test]
    fn sort_by_name_ascending_groups_directories_first_then_case_insensitive() {
        Config::init_test();
        let fx = Fixture::new();
        // Intentionally unsorted input order.
        let items = vec![
            fx.file_entry("Banana", 1),
            fx.dir_entry("Apricot"),
            fx.file_entry("apple", 1),
            fx.file_entry(".secret", 1),
            fx.dir_entry("Apple"),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);
        content.sort(SortColumn::Name, SortDirection::Ascending);

        // Directories first (config default sort_directories_first = true),
        // then files; comparison is case-insensitive and ignores a leading dot
        // (".secret" sorts as "secret").
        assert_eq!(
            names(&content),
            vec!["Apple", "Apricot", "apple", "Banana", ".secret"]
        );
    }

    // Linux only, for the same case-only fixture pair as the ascending case.
    #[cfg(target_os = "linux")]
    #[test]
    fn sort_by_name_descending_reverses_within_the_directory_grouping() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![
            fx.dir_entry("Apple"),
            fx.dir_entry("Apricot"),
            fx.file_entry("apple", 1),
            fx.file_entry("Banana", 1),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);
        content.sort(SortColumn::Name, SortDirection::Descending);

        // Descending reverses the name order, but directories are still grouped
        // ahead of files (the directories-first pass runs last and is stable).
        assert_eq!(names(&content), vec!["Apricot", "Apple", "Banana", "apple"]);
    }

    #[test]
    fn sort_by_size_orders_by_byte_length() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("medium", 50),
            fx.file_entry("small", 1),
            fx.file_entry("large", 500),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);

        content.sort(SortColumn::Size, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["small", "medium", "large"]);

        content.sort(SortColumn::Size, SortDirection::Descending);
        assert_eq!(names(&content), vec!["large", "medium", "small"]);
    }

    #[test]
    fn filter_retains_case_insensitive_substring_matches() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("Apple", 1),
            fx.file_entry("Apricot", 1),
            fx.file_entry("Banana", 1),
        ];
        let mut content = content();
        content.set_items(fx.directory(), items);
        content.set_filter("ap".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(names(&content), vec!["Apple", "Apricot"]);

        content.clear_filter();
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn toggle_show_hidden_filters_dotfiles() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![fx.file_entry("visible", 1), fx.file_entry(".hidden", 1)];
        let mut content = content();
        content.set_items(fx.directory(), items);

        // Default config has show_hidden_files = true.
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(content.len(), 2);

        // First toggle flips the runtime override to false.
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
        let fx = Fixture::new();
        let mut content = content();

        let r0 = content.revision();
        content.start_listing(fx.directory(), false);
        let r1 = content.revision();
        assert_ne!(r0, r1, "start_listing must bump the revision");

        content.append(&[fx.file_entry("a", 1)]);
        let r2 = content.revision();
        assert_ne!(r1, r2, "append must bump the revision");

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        let r3 = content.revision();
        assert_ne!(r2, r3, "finalize_listing (sort) must bump the revision");

        // Pure reads must not bump it (cache stays valid while only scrolling).
        let _ = content.items_sorted();
        let _ = content.len();
        assert_eq!(r3, content.revision());
    }

    // Linux only, for the same case-only fixture pair as the sort cases.
    #[cfg(target_os = "linux")]
    #[test]
    fn streamed_listing_matches_set_items_then_sort() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("Banana", 1),
            fx.dir_entry("Apricot"),
            fx.file_entry("apple", 1),
            fx.dir_entry("Apple"),
        ];

        // Reference: the one-shot path.
        let mut reference = content();
        reference.set_items(fx.directory(), items.clone());
        reference.sort(SortColumn::Name, SortDirection::Ascending);

        // Streamed in two batches, then finalized once.
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
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("c", 1),
            fx.file_entry("a", 1),
            fx.file_entry("b", 1),
        ];
        let mut content = content();
        content.start_listing(fx.directory(), false);
        assert!(content.is_loading());

        content.append(&items);
        // Partial results are visible in read order before the final sort.
        assert_eq!(names(&content), vec!["c", "a", "b"]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert!(!content.is_loading());
        assert_eq!(names(&content), vec!["a", "b", "c"]);
    }

    #[test]
    fn a_staged_listing_replaces_the_visible_one_only_at_finalize() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.set_items(
            fx.directory(),
            vec![fx.file_entry("a", 1), fx.file_entry("b", 1)],
        );
        content.sort(SortColumn::Name, SortDirection::Ascending);
        let revision = content.revision();

        content.start_listing(fx.directory(), true);
        content.append(&[fx.file_entry("c", 1), fx.file_entry("b", 1)]);

        // The entries on screen are of this same directory and are still
        // correct, so nothing changes until the load completes: neither the
        // listing nor the revision the row-height cache keys on, which is what
        // makes the frame identical and the terminal write nothing.
        assert_eq!(names(&content), vec!["a", "b"]);
        assert_eq!(revision, content.revision());

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["b", "c"]);
        assert_ne!(revision, content.revision());
    }

    #[test]
    fn a_staged_listing_is_filtered_when_it_is_swapped_in() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file_entry("Apple", 1)]);
        content.set_filter("ap".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);

        // Staged entries never reach `append`'s per-batch filter, so the swap
        // is what has to apply it.
        content.start_listing(fx.directory(), true);
        content.append(&[fx.file_entry("Apricot", 1), fx.file_entry("Banana", 1)]);
        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(names(&content), vec!["Apricot"]);
    }

    // The directory sorts last by name, so grouping it first is the only thing
    // that can put it at the top: a directory named ahead of the files would
    // lead either way. The dot is trimmed per segment, so `.hidden` sorts as
    // `hidden`.
    #[test_case(true, &["zdir", "afile", ".hidden"]   ; "directories are grouped first")]
    #[test_case(false, &["afile", ".hidden", "zdir"]  ; "one flat name order")]
    fn the_listing_obeys_the_settings_it_was_built_with(
        directories_first: bool,
        expected: &[&str],
    ) {
        Config::init_test();
        let fx = Fixture::new();
        // Built with the settings rather than reading them from a global, so
        // the same listing can be exercised both ways in one process.
        let mut content = DirectoryContent::new(true, directories_first);
        content.set_items(
            fx.directory(),
            vec![
                fx.file_entry("afile", 1),
                fx.dir_entry("zdir"),
                fx.file_entry(".hidden", 1),
            ],
        );

        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(expected, names(&content));
    }

    #[test]
    fn a_listing_built_without_hidden_files_never_lists_them() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = DirectoryContent::new(false, true);
        content.set_items(
            fx.directory(),
            vec![fx.file_entry("file", 1), fx.file_entry(".hidden", 1)],
        );

        content.sort(SortColumn::Name, SortDirection::Ascending);

        assert_eq!(vec!["file"], names(&content));
    }

    #[test]
    fn returning_to_the_plain_listing_drops_the_entries_of_the_mode_it_left() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file_entry("a", 1)]);
        content.sort(SortColumn::Name, SortDirection::Ascending);
        content.start_search();
        content.append(&[fx.nested_file_entry("sub", "hit")]);
        assert_eq!(names(&content), vec!["hit"]);

        // Esc leaves the search. Its results are of another root and describe
        // nothing in this directory, and the refresh that follows stages onto
        // whatever is here, so leaving them would show them as this
        // directory's own entries, under bare names now that the search root
        // is gone.
        content.set_mode(ListingMode::Normal);
        assert!(names(&content).is_empty());
    }

    #[test]
    fn a_staged_listing_abandoned_by_a_search_is_dropped() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.set_items(fx.directory(), vec![fx.file_entry("a", 1)]);
        content.sort(SortColumn::Name, SortDirection::Ascending);

        content.start_listing(fx.directory(), true);
        content.append(&[fx.file_entry("b", 1)]);
        content.start_search();
        assert!(!content.is_loading());
        content.append(&[fx.file_entry("hit", 1)]);

        // A late completion of the abandoned load must not swap its directory
        // entries into the search results.
        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["hit"]);
    }

    #[test]
    fn appended_batches_honor_the_active_filter_before_finalize() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![
            fx.file_entry("Apple", 1),
            fx.file_entry("Banana", 1),
            fx.file_entry("Apricot", 1),
        ];
        let mut content = content();
        content.set_filter("ap".to_string());
        content.start_listing(fx.directory(), false);

        content.append(&items);
        // Non-matching entries must not flash into view mid-stream.
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);
    }

    #[test]
    fn appended_batches_honor_show_hidden_before_finalize() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![fx.file_entry("visible", 1), fx.file_entry(".hidden", 1)];
        let mut content = content();
        // Default config has show_hidden_files = true; toggle it off.
        content.toggle_show_hidden();
        content.start_listing(fx.directory(), false);

        content.append(&items);
        // Hidden entries must not flash into view mid-stream.
        assert_eq!(names(&content), vec!["visible"]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["visible"]);
    }

    #[test]
    fn search_results_bypass_the_show_hidden_filter() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.set_items(fx.directory(), vec![]);
        // Default config has show_hidden_files = true; toggle it off.
        content.toggle_show_hidden();
        content.start_search();

        // A search explicitly matched these names, so hidden results are kept.
        content.append(&[fx.file_entry(".hidden", 1), fx.file_entry("visible", 1)]);
        assert_eq!(names(&content), vec![".hidden", "visible"]);

        // Re-sorting search results must not drop hidden matches either.
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec![".hidden", "visible"]);
    }

    #[test]
    fn finalize_after_a_mid_stream_filter_change_matches_a_full_sort() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.start_listing(fx.directory(), false);
        content.append(&[fx.file_entry("Banana", 1), fx.file_entry("Apple", 1)]);

        // A filter arrives mid-stream; `sort` re-derives from the unfiltered
        // items, after which finalize only has to order the survivors.
        content.set_filter("ap".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        content.append(&[fx.file_entry("Apricot", 1), fx.file_entry("Cherry", 1)]);

        content.finalize_listing(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["Apple", "Apricot"]);
    }

    /// `matches_filter` avoids building the displayed name by special-casing
    /// the trailing separator, so it must agree exactly with a plain substring
    /// search of that name, in every listing mode.
    #[test]
    fn filter_agrees_with_a_substring_search_of_the_displayed_name() {
        Config::init_test();
        let fx = Fixture::new();
        let entries = [
            fx.dir_entry("reports"),
            fx.dir_entry("Équipe"),
            fx.file_entry("report.txt", 1),
            fx.file_entry("Équipe.txt", 1),
            fx.file_entry("a", 1),
            fx.nested_file_entry("reports", "inner.txt"),
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
        // Normal, searching from the fixture root, and bookmarks.
        let modes = [
            (false, None),
            (false, Some(fx.dir.path().to_path_buf())),
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

    /// The name column and the filter both read `displayed_name`, so the
    /// property test above cannot catch a wrong string on its own: pin the
    /// three modes here.
    #[test]
    fn displayed_name_per_listing_mode() {
        Config::init_test();
        let fx = Fixture::new();
        let dir = fx.dir_entry("reports");
        let nested = fx.nested_file_entry("reports", "inner.txt");

        // Plain listing: the entry's own name, directories separator-suffixed.
        assert_eq!("reports/", displayed_name(&dir, false, None));
        assert_eq!("inner.txt", displayed_name(&nested, false, None));

        // Searching: the path relative to the search root.
        let root = Some(fx.dir.path());
        assert_eq!("reports/", displayed_name(&dir, false, root));
        assert_eq!("reports/inner.txt", displayed_name(&nested, false, root));

        // Bookmarks: the bare name, with no separator appended.
        assert_eq!("reports", displayed_name(&dir, true, None));
        assert_eq!("inner.txt", displayed_name(&nested, true, None));
    }

    /// Search rows render the path relative to the search root, so the filter
    /// has to reach the directory part, not just the basename. The property test
    /// above builds its `Visibility` by hand, so this and
    /// `filter_finds_no_separator_in_bookmark_rows` are what pin `visibility()`
    /// threading the mode through from the content's own state.
    #[test]
    fn filter_matches_the_relative_path_of_search_results() {
        Config::init_test();
        let fx = Fixture::new();
        let items = vec![
            fx.dir_entry("reports"),
            fx.nested_file_entry("reports", "inner.txt"),
            fx.file_entry("other.txt", 1),
        ];
        let mut content = content();
        content.set_items(fx.directory(), vec![]);
        content.start_search();
        content.append(&items);

        // "reports/" reaches the directory itself through its trailing
        // separator, and the nested file through its rendered path.
        content.set_filter("reports/".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["reports", "inner.txt"]);

        // A fragment spanning the separator matches only the nested file.
        content.set_filter("orts/inn".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["inner.txt"]);
    }

    /// Bookmark rows render bare names, so there is no trailing separator for
    /// a filter to match.
    #[test]
    fn filter_finds_no_separator_in_bookmark_rows() {
        Config::init_test();
        let fx = Fixture::new();
        let mut content = content();
        content.set_bookmarks(vec![
            fx.dir_entry("reports"),
            fx.file_entry("report.txt", 1),
        ]);

        content.set_filter("/".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert!(names(&content).is_empty());

        content.set_filter("report".to_string());
        content.sort(SortColumn::Name, SortDirection::Ascending);
        assert_eq!(names(&content), vec!["reports", "report.txt"]);
    }
}
