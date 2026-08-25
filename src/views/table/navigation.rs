use super::{TableView, columns::SortColumn};
use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

/// What to select after the visible items change.
#[derive(Clone, Copy, Default)]
pub(super) enum Reselect {
    /// The selection does not carry over (navigating to another directory,
    /// filtering, or re-sorting). Restore the selected file if it still
    /// exists, otherwise select the first item.
    #[default]
    Top,
    /// The same directory was reloaded: keep the selected file, or hold the
    /// cursor at the same position if the file was deleted.
    Keep,
}

/// Selection state captured when a streamed load begins (`begin_directory`) and
/// applied once it finishes (`finish_directory`). Grouped because the fields are
/// a single cohesive unit that lives and dies together.
#[derive(Default)]
pub(super) struct PendingLoad {
    reselect: Reselect,
    prev_directory: Option<PathInfo>,
    prev_selected: Option<PathInfo>,
    prev_selected_index: Option<usize>,
    /// The marked entries, to be found again once the new listing is sorted.
    /// Only populated for a reload of the same directory; a navigation leaves
    /// it empty, because a mark on an entry of another directory means nothing.
    prev_marked: Vec<PathInfo>,
}

impl TableView {
    /// Begin a streamed directory load. Captures what to reselect once the load
    /// completes (in `finish_directory`). The command handlers reset
    /// filter/search/bookmarks beforehand; `reselect` controls how the
    /// selection is restored, and whether the load is staged.
    pub(super) fn begin_directory(&mut self, new_directory: PathInfo, reselect: Reselect) {
        // Capture the pre-load state BEFORE clearing the listing.
        self.pending_load = PendingLoad {
            reselect,
            prev_directory: self.content.directory().cloned(),
            prev_selected: self.selected_path().cloned(),
            prev_selected_index: self.table_state.selected(),
            // Reselect::Keep is exactly the reload of the same directory, so it
            // is also what says the marks describe entries the new listing will
            // hold again.
            prev_marked: match reselect {
                Reselect::Keep => self.marked_paths(),
                Reselect::Top => Vec::new(),
            },
        };

        // Reselect::Keep is the reload of a directory whose listing is still
        // on screen and still correct, so the load is staged: nothing visible
        // changes until `finish_directory` swaps the new entries in. A
        // directory written to many times a second would otherwise blank,
        // repaint in read order, and repaint again sorted, once per refresh.
        //
        // A navigation has nothing valid to show, so it empties the listing and
        // streams it back in. The indices the marks are stored as would then
        // land on whatever rows arrive first, and the selection and scroll
        // offset name rows that are gone.
        let staged = matches!(reselect, Reselect::Keep);
        if !staged {
            self.clear_marks();
            self.table_state.select(None);
            self.first_visible_item = 0;
        }
        self.content.start_listing(new_directory, staged);
    }

    /// Finish a streamed directory load: sort the accumulated entries once and
    /// restore the selection captured by `begin_directory`.
    pub(super) fn finish_directory(&mut self) -> CommandResult {
        // A staged load leaves the listing live while it runs, so the cursor
        // and the marks to carry across are whatever the user last did with
        // them, not what `begin_directory` captured before the load started.
        // Replaying that snapshot would undo a keypress made during the load,
        // twice a second for a directory being written to.
        if self.content.is_staged() {
            self.pending_load.prev_selected = self.selected_path().cloned();
            self.pending_load.prev_selected_index = self.table_state.selected();
            self.pending_load.prev_marked = self.marked_paths();
        }
        self.content
            .finalize_listing(self.columns.sort_column(), self.columns.sort_direction());
        // Marks are stored by index and the listing has just been rebuilt, so
        // they are re-derived from the entries they named. A reload is not a
        // reorder the user asked for: an unrelated file appearing must not drop
        // a selection they are part way through making. An entry renamed or
        // removed loses its mark, which is honest for one no longer there. By
        // path, not inode: two hard links share a device and inode, so inode
        // identity would spread one mark across every name the file has.
        self.clear_marks();
        let marked = std::mem::take(&mut self.pending_load.prev_marked);
        for index in self.content.find_all_by_path(&marked) {
            self.marks.insert(index);
        }
        // Ends in `select`, whose snapshot then carries the restored count.
        self.restore_selection()
    }

    /// Restore the selection captured by `begin_directory`: prefer the child we
    /// came from when navigating to an ancestor, then the previously selected
    /// file by inode, then (on a refresh) the held cursor position, else the
    /// first item.
    fn restore_selection(&mut self) -> CommandResult {
        let pending = std::mem::take(&mut self.pending_load);

        // If we navigated to an ancestor directory, select the child we came from.
        if let Some(prev_directory) = pending.prev_directory
            && let Some(new_directory) = self.content.directory()
        {
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

        // Otherwise restore the previously selected file by inode, or (on a
        // refresh) hold the cursor position if it was deleted.
        if let Some(selected_path) = pending.prev_selected {
            if let Some(new_index) = self.content.find_by_inode(&selected_path) {
                return self.select(new_index);
            }
            if let Reselect::Keep = pending.reselect
                && let Some(idx) = pending.prev_selected_index
            {
                return self.select(idx.min(self.content.len().saturating_sub(1)));
            }
        }

        // Fallback: select the first item.
        self.select(0)
    }

    pub(super) fn set_filter(&mut self, filter: String) -> CommandResult {
        // Avoid an extra sort/SelectionChanged when there is no filter change
        if self.content.filter().is_empty() && filter.is_empty() {
            return CommandResult::Handled;
        }
        self.content.set_filter(filter);
        self.sort(Reselect::Top)
    }

    pub(super) fn sort(&mut self, reselect: Reselect) -> CommandResult {
        // Marks are stored by index, so any change to the visible items invalidates them.
        self.clear_marks();

        // Remember the selection across the reorder: the entry may move or drop
        // out of the listing entirely (filtering, show-hidden).
        //
        // By path, because this reorders entries already loaded rather than
        // reloading them, so nothing here can rename one and a path names
        // exactly one entry. Inode cannot: two hard links share one, and the
        // cursor would land on whichever name sorted first. A reload restores
        // the selection in `restore_selection`, where inode is right precisely
        // because a rename is possible.
        let selected = self.selected_path().cloned();
        let selected_index = self.table_state.selected();

        self.content
            .sort(self.columns.sort_column(), self.columns.sort_direction());

        if let Some(selected_path) = selected {
            if let Some(new_index) = self.content.find_by_path(selected_path.as_path()) {
                // The selected file still exists after sort/filter
                return self.select(new_index);
            }

            // The selected file is gone. On a refresh (Reselect::Keep) it was
            // likely deleted, so hold the cursor at the same position;
            // otherwise fall through to the top.
            if let Reselect::Keep = reselect
                && let Some(idx) = selected_index
            {
                return self.select(idx.min(self.content.len().saturating_sub(1)));
            }
        }

        // Fallback: Select the first item
        self.select(0)
    }

    /// Reorder the visible items, carrying the marks across.
    ///
    /// Every other reorder here is one the user asked for (a sort column, a
    /// filter), and dropping index-based marks answers those honestly. A search
    /// ending is not one: results stream so they can be marked before the walk
    /// is done. The marks are re-derived from the entries they named, so one the
    /// reorder dropped loses its mark. Range mode ends either way, since its
    /// anchor names a position and the positions have just changed.
    pub(super) fn sort_keeping_marks(&mut self, reselect: Reselect) -> CommandResult {
        // Captured before the sort clears them: an index says nothing about an
        // entry once the order has changed.
        let marked = self.marked_paths();
        let result = self.sort(reselect);
        if marked.is_empty() {
            return result;
        }
        for index in self.content.find_all_by_path(&marked) {
            self.marks.insert(index);
        }
        // `sort` already reported the selection, but with a mark count of zero,
        // which was only true for the moment between there and here.
        self.selection_snapshot()
    }

    pub(super) fn sort_by(&mut self, column: SortColumn) -> CommandResult {
        self.columns.sort_by(column);
        self.sort(Reselect::Top)
    }

    pub(super) fn toggle_show_hidden(&mut self) -> CommandResult {
        // Search results always show hidden entries, so the setting has no
        // visible effect during a search; toggling then would only cause
        // invisible state changes (the persisted flag, the sort order, and
        // the marks).
        if self.content.is_searching() {
            return CommandResult::Handled;
        }
        self.content.toggle_show_hidden();
        self.sort(Reselect::Top)
    }
}

/// Synchronous convenience for tests: runs the streamed begin/append/finish
/// cycle in one call, mirroring how a directory loads at runtime.
#[cfg(test)]
impl TableView {
    fn set_directory(
        &mut self,
        directory: PathInfo,
        children: &[PathInfo],
        reselect: Reselect,
    ) -> CommandResult {
        self.begin_directory(directory, reselect);
        self.content.append(children);
        self.finish_directory()
    }
}

#[cfg(test)]
mod tests {

    use std::path::PathBuf;

    use super::{Reselect, SortColumn, TableView};
    use crate::{
        app::config::Config,
        command::{Command, handler::CommandHandler, result::CommandResult},
        file_system::path_info::PathInfo,
        test_support::TempDir,
    };

    struct Fixture {
        dir: TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = TempDir::new("nav");
            Self { dir }
        }

        fn file(&self, name: &str, size: usize) -> PathInfo {
            let path = self.dir.join(name);
            std::fs::write(&path, vec![b'x'; size]).unwrap();
            PathInfo::try_from(&path).unwrap()
        }

        /// A file one level down, so a search result's displayed name (the
        /// path relative to the search root) differs from its basename.
        fn nested(&self, dir: &str, name: &str) -> PathInfo {
            let dir = self.dir.join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, b"x").unwrap();
            PathInfo::try_from(&path).unwrap()
        }

        fn directory(&self) -> PathInfo {
            PathInfo::try_from(self.dir.path()).unwrap()
        }
    }

    fn visible_names(table: &TableView) -> Vec<String> {
        table
            .content
            .items_sorted()
            .iter()
            .map(|item| item.display_name.clone())
            .collect()
    }

    fn selected_basename(table: &TableView) -> Option<String> {
        table.selected_path().map(|p| p.display_name.clone())
    }

    /// Asserts that `result` is a single `SelectionChanged` snapshot with a
    /// mark count of zero (the marks were cleared).
    fn assert_mark_reset_snapshot(result: &CommandResult) {
        match result {
            CommandResult::HandledWith(command) => {
                assert!(
                    matches!(**command, Command::SelectionChanged { mark_count: 0, .. }),
                    "expected a mark-reset snapshot, got {command:?}"
                );
            }
            other => panic!("expected a SelectionChanged snapshot, got {other:?}"),
        }
    }

    #[test]
    fn set_directory_top_selects_the_first_item() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        let children = vec![fx.file("b", 1), fx.file("a", 1), fx.file("c", 1)];
        table.set_directory(fx.directory(), &children, Reselect::Top);

        assert_eq!(table.table_state.selected(), Some(0));
        assert_eq!(selected_basename(&table).as_deref(), Some("a"));
    }

    #[test]
    fn sort_keeps_the_selected_file_when_it_moves_position() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        // Name-ascending order: a, b, c
        let children = vec![fx.file("a", 3), fx.file("b", 1), fx.file("c", 2)];
        table.set_directory(fx.directory(), &children, Reselect::Top);

        // Select "b" (index 1 by name).
        table.select(1);
        assert_eq!(selected_basename(&table).as_deref(), Some("b"));

        // Re-sort by size, largest first (a=3, c=2, b=1): "b" moves to the
        // last index but stays selected.
        table.sort_by(SortColumn::Size);
        assert_eq!(table.table_state.selected(), Some(2));
        assert_eq!(selected_basename(&table).as_deref(), Some("b"));
    }

    #[test]
    fn reselect_keep_holds_the_cursor_position_when_the_selected_file_is_deleted() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)],
            Reselect::Top,
        );
        table.select(1); // "b"

        // Same directory reloaded with "b" removed; cursor holds at index 1.
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("c", 1)],
            Reselect::Keep,
        );
        assert_eq!(table.table_state.selected(), Some(1));
        assert_eq!(selected_basename(&table).as_deref(), Some("c"));
    }

    #[test]
    fn reselect_top_falls_back_to_first_when_the_selected_file_is_gone() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)],
            Reselect::Top,
        );
        table.select(2); // "c"

        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("b", 1)],
            Reselect::Top,
        );
        assert_eq!(table.table_state.selected(), Some(0));
        assert_eq!(selected_basename(&table).as_deref(), Some("a"));
    }

    /// Build a table with three items and mark the first two.
    fn table_with_two_marks(fx: &Fixture) -> TableView {
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)],
            Reselect::Top,
        );
        table.select(0);
        table.toggle_mark();
        table.select(1);
        table.toggle_mark();
        assert_eq!(table.marks.len(), 2);
        table
    }

    #[test]
    fn delete_clears_marks_and_resets_the_mark_count_notice() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        let result = table.handle_command(&Command::Delete(table.marked_paths()));

        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    #[test]
    fn delete_without_marks_does_not_emit_a_mark_count_command() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[fx.file("a", 1)], Reselect::Top);

        let result = table.handle_command(&Command::Delete(vec![]));

        assert_eq!(result, CommandResult::Handled);
    }

    /// Drives the reload a watcher event starts, through to its completion.
    fn reload(table: &mut TableView, fx: &Fixture, children: Vec<PathInfo>) -> CommandResult {
        let result = table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        // Nothing is announced up front: the marks are still the user's, and
        // saying otherwise would blank the notice for the length of the reload.
        assert_eq!(result, CommandResult::Handled);
        table.handle_command(&Command::ListingBatch {
            items: children,
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 })
    }

    #[test]
    fn a_reload_keeps_the_listing_and_the_cursor_until_it_completes() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("b", 1)],
            Reselect::Top,
        );
        table.select(1);

        table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });

        // A directory being written to reloads at the watcher's rate. Emptying
        // the listing and the cursor here, then repainting them as the entries
        // arrive, is what makes that look like the screen flashing.
        assert_eq!(vec!["a", "b"], visible_names(&table));
        assert_eq!(Some("b".to_string()), selected_basename(&table));

        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("c", 1), fx.file("b", 1)],
            generation: 1,
        });

        // Batches arrive in read order, which is not the order the header
        // advertises; showing them would reorder the rows mid-reload.
        assert_eq!(vec!["a", "b"], visible_names(&table));

        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });
        assert_eq!(vec!["b", "c"], visible_names(&table));
        assert_eq!(Some("b".to_string()), selected_basename(&table));
    }

    #[test]
    fn a_reload_keeps_a_cursor_move_and_a_mark_made_while_it_runs() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        let children = [fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)];
        table.set_directory(fx.directory(), &children, Reselect::Top);
        table.select(0);

        table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });

        // The listing stays live for the length of the reload, so it still
        // takes input. Restoring the cursor and marks the reload began with
        // would undo that input, twice a second under a watcher refresh.
        table.select_next();
        table.select_next();
        table.toggle_mark();

        table.handle_command(&Command::ListingBatch {
            items: children.to_vec(),
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert_eq!(Some("c".to_string()), selected_basename(&table));
        assert_eq!(
            vec!["c".to_string()],
            table
                .marked_paths()
                .iter()
                .map(|item| item.display_name.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn leaving_a_search_does_not_leave_its_results_in_the_directory_listing() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[fx.file("a", 1)], Reselect::Top);
        table.content.start_search();
        table.content.append(&[fx.nested("sub", "hit")]);
        assert_eq!(vec!["hit"], visible_names(&table));

        // Esc, then the refresh it asks for. The results belong to another
        // root, so a reload staging onto them would show them as this
        // directory's entries until it completes.
        table.handle_command(&Command::ResetView);
        table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        assert!(visible_names(&table).is_empty());

        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1)],
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });
        assert_eq!(vec!["a"], visible_names(&table));
    }

    #[test]
    fn a_reload_carries_the_marks_across() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        // A file appearing in the directory is not a request to drop what the
        // user has selected, so the marks come back on the same two entries.
        reload(
            &mut table,
            &fx,
            vec![
                fx.file("a", 1),
                fx.file("b", 1),
                fx.file("c", 1),
                fx.file("d", 1),
            ],
        );

        assert_eq!(table.marks.len(), 2);
        let marked: Vec<String> = table
            .marked_paths()
            .iter()
            .map(|info| info.display_name.clone())
            .collect();
        assert_eq!(vec!["a".to_string(), "b".to_string()], marked);
    }

    /// Why the cursor is restored by inode rather than by path: a reload can
    /// rename, and the entry the user was on is the same entry under its new
    /// name. Restoring by path would drop to the fallback and move the cursor.
    #[test]
    fn a_reload_follows_a_renamed_entry_to_its_new_name() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)],
            Reselect::Top,
        );
        table.select(1); // "b"

        // Renamed on disk rather than recreated, so the entry keeps the device
        // and inode it was selected under.
        std::fs::rename(fx.dir.join("b"), fx.dir.join("z_renamed")).unwrap();
        let renamed = PathInfo::try_from(&fx.dir.join("z_renamed")).unwrap();
        reload(
            &mut table,
            &fx,
            vec![fx.file("a", 1), fx.file("c", 1), renamed],
        );

        // Sorted last, so a cursor left on index 1 would land on "c".
        assert_eq!(Some(2), table.table_state.selected());
        assert_eq!(selected_basename(&table).as_deref(), Some("z_renamed"));
    }

    #[test]
    fn a_reload_drops_a_mark_on_an_entry_that_is_gone() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        // "a" was removed while the listing reloaded. Nothing names it any
        // more, so its mark goes with it and "b" keeps its own.
        reload(&mut table, &fx, vec![fx.file("b", 1), fx.file("c", 1)]);

        assert_eq!(table.marks.len(), 1);
        assert_eq!(
            vec!["b".to_string()],
            table
                .marked_paths()
                .iter()
                .map(|info| info.display_name.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn navigating_away_does_not_carry_the_marks() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        // A mark names an entry of the directory being left, so it means
        // nothing in the one being entered, even if a name happens to repeat.
        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1), fx.file("b", 1)],
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert!(!table.has_marks());
    }

    #[test]
    fn late_listing_completion_does_not_clobber_the_bookmarks_listing() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        // A load is in flight for the CWD when the bookmarks key is pressed.
        table.begin_directory(fx.directory(), Reselect::Top);
        table.stream_generation = 7;
        table.handle_command(&Command::Bookmarks {
            bookmarks: vec![fx.file("mark-a", 1), fx.file("mark-b", 1)],
        });
        table.select(1);
        assert!(table.content.is_showing_bookmarks());

        // The cancelled loader had already drained the directory, so it still
        // sends its completion with the generation the table last recorded.
        // Finalizing here would re-sort the bookmarks and move the cursor.
        let result = table.handle_command(&Command::DirectoryListingComplete { generation: 7 });

        assert_eq!(result, CommandResult::Handled);
        assert!(table.content.is_showing_bookmarks());
        assert_eq!(table.table_state.selected(), Some(1));
        assert_eq!(selected_basename(&table).as_deref(), Some("mark-b"));
    }

    #[test]
    fn refreshed_directory_while_searching_keeps_the_marks_and_the_notice() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);
        // start_search is called directly, so the two marks carry into the
        // search listing exactly as they would after marking results.
        table.content.start_search();
        assert_eq!(table.marks.len(), 2);

        // A watcher event fires while search results are displayed. The listing
        // belongs to a different root, so the refresh is ignored and the marks
        // survive. A mark-reset snapshot would blank the notice while the marks
        // are still live and still operated on by a later delete/copy/chmod.
        let result = table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });

        assert_eq!(table.marks.len(), 2);
        assert_eq!(result, CommandResult::Handled);
    }

    #[test]
    fn bookmarks_clears_the_marks_and_emits_the_mark_reset_snapshot() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        let result = table.handle_command(&Command::Bookmarks {
            bookmarks: vec![fx.file("mark-a", 1)],
        });

        // Pins the invariant documented in the `Command::Bookmarks` arm.
        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    #[test]
    fn refreshed_directory_while_showing_bookmarks_reloads_them_and_keeps_the_marks() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);
        table
            .content
            .set_bookmarks(vec![fx.file("mark-a", 1), fx.file("mark-b", 1)]);
        assert_eq!(table.marks.len(), 2);

        // Renaming a bookmark refreshes the CWD; the bookmarks list is
        // reloaded instead.
        let result = table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });

        // The marks belong to the listing still on screen and are cleared by
        // the Bookmarks handler once the new one arrives. Clearing them here
        // would drop valid marks when the read fails and no Bookmarks command
        // follows.
        assert_eq!(table.marks.len(), 2);
        assert_eq!(result, Command::GetBookmarks.into());
    }

    #[test]
    fn sort_by_clears_marks_and_resets_the_mark_count_notice() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        let result = table.sort_by(SortColumn::Size);

        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    #[test]
    fn a_filter_change_clears_the_marks() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        // Filtering is a reorder the user asked for, and a mark is held by
        // index, so the rows those indices address are no longer the ones the
        // user chose.
        let result = table.handle_command(&Command::FilterChanged("a".to_string()));

        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    #[test]
    fn a_filter_change_that_reorders_nothing_keeps_the_marks() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);

        // Clearing a filter that was never set reorders nothing, so it must not
        // cost the user their marks. Dismissing the filter prompt sends this.
        let result = table.handle_command(&Command::FilterChanged(String::new()));

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(2, table.marks.len());
    }

    #[test]
    fn a_reorder_ends_range_mode() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = table_with_two_marks(&fx);
        table.enter_range_mode();
        assert!(table.marks.in_range_mode());

        table.sort_by(SortColumn::Size);

        // The anchor names a position, and the positions have just changed, so
        // the next cursor move would sweep a range from somewhere else.
        assert!(!table.marks.in_range_mode());
    }

    #[test]
    fn a_search_ending_keeps_the_marks_but_still_ends_range_mode() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[], Reselect::Top);
        let apple = fx.nested("z", "apple.txt");
        let zebra = fx.nested("a", "zebra.txt");

        table.handle_command(&Command::StartSearch("txt".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![apple.clone(), zebra],
            generation: 1,
        });
        table.select(0);
        table.enter_range_mode();

        table.handle_command(&Command::ExitedSearch { generation: 1 });

        // The reorder that ends a search is the one case that carries marks
        // across, and the anchor is what it must still drop: it is an index
        // into the order the sort has just replaced.
        assert_eq!(
            vec![apple.path],
            table
                .marked_paths()
                .into_iter()
                .map(|item| item.path)
                .collect::<Vec<_>>()
        );
        assert!(!table.marks.in_range_mode());
    }

    #[test]
    fn toggle_show_hidden_snapshot_carries_the_new_selection_and_cleared_marks() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        // Sorted (a leading dot is ignored when comparing names): a, .b
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file(".b", 1)],
            Reselect::Top,
        );
        table.select(1); // ".b"
        table.toggle_mark();

        // Hiding dotfiles removes the selected file; one snapshot must carry
        // both the fallback selection and the cleared mark count.
        let result = table.toggle_show_hidden();

        assert!(!table.has_marks());
        assert_eq!(selected_basename(&table).as_deref(), Some("a"));
        assert_eq!(
            result,
            Command::SelectionChanged {
                selected: table.selected_path().cloned(),
                mark_count: 0,
            }
            .into()
        );
    }

    #[test]
    fn toggle_show_hidden_is_a_noop_during_a_search() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file(".b", 1)],
            Reselect::Top,
        );
        assert_eq!(table.content.len(), 2);

        table.handle_command(&Command::StartSearch("a".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1)],
            generation: 1,
        });
        table.select(0);
        table.toggle_mark();
        assert!(table.has_marks());

        let result = table.toggle_show_hidden();

        assert_eq!(result, CommandResult::Handled);
        assert!(table.has_marks());

        // The setting must not have flipped: reloading the directory after
        // the search still shows the hidden file.
        table.content.clear_search();
        table.set_directory(
            fx.directory(),
            &[fx.file("a", 1), fx.file(".b", 1)],
            Reselect::Top,
        );
        assert_eq!(table.content.len(), 2);
    }

    #[test]
    fn search_results_are_sorted_once_the_walk_is_done() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();

        table.handle_command(&Command::StartSearch("a".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        // Batches arrive in walk order, which is whatever readdir gave the
        // walk, not the order the header advertises.
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("cab", 1), fx.file("bat", 1)],
            generation: 1,
        });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("art", 1)],
            generation: 1,
        });
        assert_eq!(
            vec!["cab", "bat", "art"],
            table
                .content
                .items_sorted()
                .iter()
                .map(|item| item.display_name.as_str())
                .collect::<Vec<_>>()
        );

        table.handle_command(&Command::ExitedSearch { generation: 1 });

        assert_eq!(
            vec!["art", "bat", "cab"],
            table
                .content
                .items_sorted()
                .iter()
                .map(|item| item.display_name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn search_results_are_sorted_by_the_name_the_column_shows() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        // `start_search` takes the search root from the current directory, and
        // the root is what the column renders names relative to.
        table.set_directory(fx.directory(), &[], Reselect::Top);
        let apple = fx.nested("z", "apple.txt");
        let zebra = fx.nested("a", "zebra.txt");

        table.handle_command(&Command::StartSearch("txt".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![apple, zebra],
            generation: 1,
        });
        table.handle_command(&Command::ExitedSearch { generation: 1 });

        // Ordering by the basename would put `z/apple.txt` first, which reads
        // as unsorted in a column that shows the path relative to the root.
        let search_root = table.content.search_root().map(PathBuf::from);
        let displayed: Vec<String> = table
            .content
            .items_sorted()
            .iter()
            .map(|item| {
                super::super::content::displayed_name(item, false, search_root.as_deref())
                    .into_owned()
            })
            .collect();
        assert_eq!(vec!["a/zebra.txt", "z/apple.txt"], displayed);
    }

    #[test]
    fn a_mark_made_while_a_search_streamed_follows_its_entry_through_the_sort() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[], Reselect::Top);
        let apple = fx.nested("z", "apple.txt");
        let zebra = fx.nested("a", "zebra.txt");

        table.handle_command(&Command::StartSearch("txt".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![apple.clone(), zebra],
            generation: 1,
        });
        // Marked while the walk is still running, which is what streaming the
        // results is for.
        table.select(0);
        table.toggle_mark();

        let result = table.handle_command(&Command::ExitedSearch { generation: 1 });

        // The sort moves `z/apple.txt` to the end, so a mark carried by index
        // would land on `a/zebra.txt` instead.
        assert_eq!(
            vec![apple.path.clone()],
            table
                .marked_paths()
                .into_iter()
                .map(|item| item.path)
                .collect::<Vec<_>>()
        );
        // The notice has to agree, or it would still read the zero the sort
        // reported on its way through.
        match &result {
            CommandResult::HandledWith(command) => assert!(
                matches!(**command, Command::SelectionChanged { mark_count: 1, .. }),
                "expected a snapshot counting the carried mark, got {command:?}"
            ),
            other => panic!("expected a SelectionChanged snapshot, got {other:?}"),
        }
    }

    #[test]
    fn a_dot_file_below_the_search_root_sorts_next_to_its_neighbours() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[], Reselect::Top);
        let hidden = fx.nested("projects", ".zzz.txt");
        let plain = fx.nested("projects", "bbb.txt");

        table.handle_command(&Command::StartSearch("txt".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![hidden.clone(), plain.clone()],
            generation: 1,
        });
        table.handle_command(&Command::ExitedSearch { generation: 1 });

        // What `ls -a` does under a UTF-8 locale: the dot is ignored wherever
        // it sits, so `.zzz.txt` sorts after `bbb.txt` rather than ahead of
        // every visible entry in its own subtree.
        assert_eq!(
            vec![plain.path, hidden.path],
            table
                .content
                .items_sorted()
                .iter()
                .map(|item| item.path.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn carrying_marks_does_not_spread_them_across_hard_links() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[], Reselect::Top);
        let first = fx.nested("a", "obj");
        // A second name for the same file. A recursive search finds both, and
        // they share a device and inode, so identity by inode cannot tell them
        // apart.
        let link_dir = fx.directory().path.join("b");
        std::fs::create_dir_all(&link_dir).unwrap();
        let link_path = link_dir.join("obj");
        std::fs::hard_link(&first.path, &link_path).unwrap();
        let link = PathInfo::try_from(&link_path).unwrap();

        table.handle_command(&Command::StartSearch("obj".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![link.clone(), first],
            generation: 1,
        });
        table.select(0);
        table.toggle_mark();

        table.handle_command(&Command::ExitedSearch { generation: 1 });

        // Only the entry the user marked. Marking the other one too would put
        // it in the next delete or cut without it ever having been chosen.
        assert_eq!(
            vec![link.path.clone()],
            table
                .marked_paths()
                .into_iter()
                .map(|item| item.path)
                .collect::<Vec<_>>()
        );
        // And the cursor stays on it. Restoring the selection by inode would
        // move it to whichever name sorted first, so the next delete, rename,
        // or open would act on a path the user never selected.
        assert_eq!(
            Some(link.path),
            table.selected_path().map(|item| item.path.clone())
        );
    }

    #[test]
    fn a_cancelled_search_sorts_the_results_it_did_find() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[], Reselect::Top);
        let apple = fx.nested("z", "apple.txt");
        let zebra = fx.nested("a", "zebra.txt");

        table.handle_command(&Command::StartSearch("txt".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![apple.clone(), zebra.clone()],
            generation: 1,
        });
        // Stopped part way through. The results found so far are kept, and the
        // listing stays in search mode, so the exit still arrives.
        table.handle_command(&Command::CancelSearch);
        table.handle_command(&Command::ExitedSearch { generation: 1 });

        // Nothing more is coming, so walk order would be the final order and
        // the header would be describing something the listing never becomes.
        assert_eq!(
            vec![zebra.path, apple.path],
            table
                .content
                .items_sorted()
                .iter()
                .map(|item| item.path.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_superseded_search_exiting_does_not_reorder_its_replacement() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();

        table.handle_command(&Command::StartSearch("a".into()));
        table.handle_command(&Command::SearchStarted { generation: 2 });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("cab", 1), fx.file("bat", 1)],
            generation: 2,
        });

        // The search this one replaced exits with its own generation, while
        // the replacement is still streaming.
        table.handle_command(&Command::ExitedSearch { generation: 1 });

        assert_eq!(
            vec!["cab", "bat"],
            table
                .content
                .items_sorted()
                .iter()
                .map(|item| item.display_name.as_str())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn stale_listing_batches_are_ignored() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 2,
        });

        // A batch from a superseded stream must not be appended.
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("stale", 1)],
            generation: 1,
        });
        assert_eq!(table.content.len(), 0);

        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("fresh", 1)],
            generation: 2,
        });
        assert_eq!(table.content.len(), 1);
    }

    #[test]
    fn late_search_batches_are_dropped_in_bookmarks_mode() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 2,
        });
        assert!(table.content.is_loading());

        // The load is cancelled by the starting search and never finalizes,
        // so only the mode transition can clear the loading flag.
        table.handle_command(&Command::StartSearch("a".into()));
        table.handle_command(&Command::SearchStarted { generation: 3 });
        assert!(!table.content.is_loading());

        table.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        // The cancelled search's final flush still carries the current
        // generation; bookmarks mode accepts no batches.
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("late", 1)],
            generation: 3,
        });
        assert!(table.content.is_showing_bookmarks());
        assert_eq!(table.content.len(), 0);
    }

    #[test]
    fn search_started_outside_search_mode_keeps_the_load_generation() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 2,
        });

        // The empty-query backstop emits SearchStarted while the table never
        // entered search mode; the in-flight load must keep streaming.
        table.handle_command(&Command::SearchStarted { generation: 9 });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1)],
            generation: 2,
        });
        assert_eq!(table.content.len(), 1);
    }

    #[test]
    fn showing_bookmarks_clears_an_active_search() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[fx.file("a", 1)], Reselect::Top);
        table.handle_command(&Command::StartSearch("a".into()));
        assert!(table.content.is_searching());

        table.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert!(!table.content.is_searching());
        assert!(table.content.is_showing_bookmarks());

        // With the search cleared, a directory refresh reloads the bookmarks
        // list instead of being swallowed by the search guard.
        let result = table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        assert_eq!(result, Command::GetBookmarks.into());
    }

    #[test]
    fn starting_a_search_clears_the_bookmarks_view() {
        Config::init_test();
        let fx = Fixture::new();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), &[fx.file("a", 1)], Reselect::Top);
        table.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert!(table.content.is_showing_bookmarks());

        table.handle_command(&Command::StartSearch("a".into()));
        assert!(!table.content.is_showing_bookmarks());
        assert!(table.content.is_searching());
    }
}
