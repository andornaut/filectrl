use super::{TableView, columns::SortColumn, content::DirectoryContent};
use crate::{command::result::CommandResult, file_system::path_info::PathInfo};

/// What to select after the visible items change.
#[derive(Clone, Copy, Default)]
pub(super) enum Reselect {
    /// The selection does not carry over (navigating to another directory).
    /// Restore the selected file if it still exists, otherwise select the
    /// first item.
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
    /// Populated by `finish_directory` from the marks live when the load ends:
    /// a reload's, or those made on the new listing while it streamed in.
    prev_marked: Vec<PathInfo>,
    /// Whether the cursor was moved while a navigation streamed in, so that
    /// where it was left wins over reselecting the child we came from.
    cursor_moved: bool,
}

impl TableView {
    /// Begin a streamed directory load. Captures what to reselect once the load
    /// completes (in `finish_directory`). The command handlers reset
    /// filter/search/bookmarks beforehand; `reselect` controls how the
    /// selection is restored, and whether the load is staged.
    pub(super) fn begin_directory(&mut self, new_directory: PathInfo, reselect: Reselect) {
        // Capture the pre-load state BEFORE clearing the listing.
        // The cursor index and the marks are left for `finish_directory`: only
        // a reload (Reselect::Keep) uses them, and a reload is staged, so what
        // carries across is their state when it finishes.
        self.pending_load = PendingLoad {
            reselect,
            prev_directory: self.content.directory().cloned(),
            prev_selected: self.selected_path().cloned(),
            ..PendingLoad::default()
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

    /// Records that input moved the cursor while a navigation or a search
    /// streams in, so that `finish_directory` or `exited_search` keeps it
    /// there. Read from the selection before and after the input rather than
    /// from where the cursor ends up, since moving away and back to the top
    /// row is still a choice the user made. During a search, marking the row
    /// the first result put the cursor on chooses that row too.
    pub(super) fn note_cursor_move(&mut self, before: Option<usize>, marks_before: usize) {
        let moved = self.table_state.selected() != before;
        if moved && self.content.is_loading() && !self.content.is_staged() {
            self.pending_load.cursor_moved = true;
        }
        if self.content.is_searching() && (moved || self.marks.len() != marks_before) {
            self.search_cursor_chosen = true;
        }
    }

    /// Finish a streamed directory load: sort the accumulated entries once and
    /// restore the selection captured by `begin_directory`.
    pub(super) fn finish_directory(&mut self) -> CommandResult {
        // A staged load leaves the listing live while it runs, so the cursor
        // and the marks to carry across are whatever the user last did with
        // them, not what `begin_directory` captured before the load started.
        // Replaying that snapshot would undo a keypress made during the load,
        // twice a second for a directory being written to.
        // Range mode started while either kind of load ran is the user's too.
        let range = self.range_by_path();
        if self.content.is_staged() {
            self.pending_load.prev_selected = self.selected_path().cloned();
            self.pending_load.prev_selected_index = self.table_state.selected();
            self.pending_load.prev_marked = self.marked_paths();
        } else {
            // A navigation streams onto a live listing: the first batch puts
            // the cursor on the top row, and anything done to the listing
            // since is the user's, made on entries of the new directory. The
            // sort below would otherwise move the rows out from under it.
            self.pending_load.prev_marked = self.marked_paths();
            // A range anchored during the load extends from the cursor, so the
            // cursor stays with it even if it never moved: sent to the top of
            // the sorted listing, the next move would mark every row between.
            if range.is_some() {
                self.pending_load.cursor_moved = true;
            }
            if self.pending_load.cursor_moved {
                self.pending_load.prev_selected = self.selected_path().cloned();
            }
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
        let _ = self.restore_selection(range.is_some());
        // Range mode is carried across a reload the same way, and resumes
        // only after the cursor is placed: `select` would otherwise mark every
        // entry that appeared between the anchor and the cursor, unseen. The
        // snapshot is taken after both, so it carries the restored count and
        // range.
        self.restore_range_by_path(range);
        self.selection_snapshot()
    }

    /// Restore the selection captured by `begin_directory`: prefer the child we
    /// came from when navigating to an ancestor, then the previously selected
    /// file by inode, then (on a refresh) the held cursor position, else the
    /// first item.
    ///
    /// With a range carried across, the cursor is found by path, as the range's
    /// anchor is: two hard links share an inode, so the cursor could land on the
    /// other name and the next move would mark every row between the two.
    fn restore_selection(&mut self, by_path: bool) -> CommandResult {
        let pending = std::mem::take(&mut self.pending_load);

        // If we navigated to an ancestor directory, select the child we came
        // from, unless the cursor was already moved while the listing loaded.
        if !pending.cursor_moved
            && let Some(prev_directory) = pending.prev_directory
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
            let found = if by_path {
                self.content.find_by_path(&selected_path.path)
            } else {
                self.content.find_by_inode(&selected_path)
            };
            if let Some(new_index) = found {
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
        // An unchanged filter reorders nothing, so it must not cost the marks
        // or a sort: dismissing the filter prompt, or submitting it unedited,
        // sends the filter already applied.
        if self.content.filter() == filter {
            return CommandResult::Handled;
        }
        let (column, direction) = (self.columns.sort_column(), self.columns.sort_direction());
        self.reorder(|content| content.apply_filter(filter, column, direction))
    }

    /// Reorder the visible items, following the selection to where it moved.
    ///
    /// A reorder cannot rename an entry, so an entry that is no longer in the
    /// listing was filtered out rather than replaced, and the cursor goes to
    /// the top. Holding the cursor's position instead is a reload's job, where
    /// the entry under it may have been deleted (`restore_selection`).
    pub(super) fn sort(&mut self) -> CommandResult {
        let (column, direction) = (self.columns.sort_column(), self.columns.sort_direction());
        self.reorder(|content| content.sort(column, direction))
    }

    /// Change the visible items with `apply`, following the selection as
    /// `sort` describes.
    fn reorder(&mut self, apply: impl FnOnce(&mut DirectoryContent)) -> CommandResult {
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

        apply(&mut self.content);

        if let Some(selected_path) = selected
            && let Some(new_index) = self.content.find_by_path(selected_path.as_path())
        {
            // The selected file still exists after sort/filter
            return self.select(new_index);
        }

        // Fallback: Select the first item
        self.select(0)
    }

    /// Reorder the visible items, carrying the marks across.
    ///
    /// Every other reorder here is one the user asked for (a sort column, a
    /// filter), and dropping index-based marks answers those honestly. A search
    /// ending is not one: results stream so they can be marked before the walk
    /// is done. Nor is a bookmarks reload, which follows a change made
    /// elsewhere. The marks are re-derived from the entries they named, so one the
    /// reorder dropped loses its mark. Range mode ends either way, since its
    /// anchor names a position and the positions have just changed.
    pub(super) fn sort_keeping_marks(&mut self) -> CommandResult {
        // Captured before the sort clears them: an index says nothing about an
        // entry once the order has changed.
        let marked = self.marked_paths();
        let result = self.sort();
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
        self.sort()
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
        self.sort()
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

    use ratatui::crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    use super::{Reselect, SortColumn, TableView};
    use crate::{
        app::config::Config,
        command::{Command, handler::CommandHandler, result::CommandResult},
        file_system::path_info::PathInfo,
        test_support::TempDir,
    };

    /// A table listing `children` of `fx`, with the cursor on the first.
    fn listed(fx: &TempDir, children: &[PathInfo]) -> TableView {
        Config::init_test();
        let mut table = TableView::default();
        table.set_directory(fx.directory(), children, Reselect::Top);
        table
    }

    fn visible_names(table: &TableView) -> Vec<String> {
        table
            .content
            .items_sorted()
            .iter()
            .map(|item| item.display_name.clone())
            .collect()
    }

    /// A keypress through the table's own handler, as the user makes it.
    fn press(table: &mut TableView, key: char) {
        table.handle_key(KeyCode::Char(key), KeyModifiers::NONE);
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
        let fx = TempDir::new("nav");
        let table = listed(&fx, &[fx.file("b", 1), fx.file("a", 1), fx.file("c", 1)]);

        assert_eq!(table.table_state.selected(), Some(0));
        assert_eq!(selected_basename(&table).as_deref(), Some("a"));
    }

    #[test]
    fn sort_keeps_the_selected_file_when_it_moves_position() {
        let fx = TempDir::new("nav");
        // Name-ascending order: a, b, c
        let mut table = listed(&fx, &[fx.file("a", 3), fx.file("b", 1), fx.file("c", 2)]);

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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)]);
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
    fn reselect_keep_clamps_a_held_cursor_to_a_shorter_listing() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)]);
        table.select(2); // "c", the last item

        // Reloaded with the tail gone, so the position the cursor held no
        // longer exists. Holding it verbatim would leave the cursor past the
        // end of the listing, selecting nothing at all.
        table.set_directory(fx.directory(), &[fx.file("a", 1)], Reselect::Keep);

        assert_eq!(Some(0), table.table_state.selected());
        assert_eq!(Some("a"), selected_basename(&table).as_deref());
    }

    #[test]
    fn reselect_top_falls_back_to_first_when_the_selected_file_is_gone() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)]);
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
    fn table_with_two_marks(fx: &TempDir) -> TableView {
        let mut table = listed(fx, &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)]);
        table.select(0);
        table.toggle_mark();
        table.select(1);
        table.toggle_mark();
        assert_eq!(table.marks.len(), 2);
        table
    }

    fn copy_op(srcs: Vec<PathInfo>, dest: PathInfo) -> Command {
        Command::Copy { srcs, dest }
    }

    fn move_op(srcs: Vec<PathInfo>, dest: PathInfo) -> Command {
        Command::Move { srcs, dest }
    }

    fn chmod_op(paths: Vec<PathInfo>, _: PathInfo) -> Command {
        Command::Chmod {
            paths,
            mode: "644".into(),
        }
    }

    fn delete_op(paths: Vec<PathInfo>, _: PathInfo) -> Command {
        Command::Delete(paths)
    }

    #[test_case(copy_op ; "a copy")]
    #[test_case(move_op ; "a move")]
    #[test_case(chmod_op ; "a chmod")]
    #[test_case(delete_op ; "a delete")]
    fn an_operation_consumes_the_marks_and_resets_the_mark_count_notice(
        operation: fn(Vec<PathInfo>, PathInfo) -> Command,
    ) {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);

        let result = table.handle_command(&operation(table.marked_paths(), fx.directory()));

        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    #[test]
    fn delete_without_marks_does_not_emit_a_mark_count_command() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1)]);

        let result = table.handle_command(&Command::Delete(vec![]));

        assert_eq!(result, CommandResult::Handled);
    }

    /// Drives the reload a watcher event starts, through to its completion.
    fn reload(table: &mut TableView, fx: &TempDir, children: Vec<PathInfo>) -> CommandResult {
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file("b", 1)]);
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
    fn navigating_to_the_parent_selects_the_directory_it_came_from() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let child = fx.nested("sub", "inside");
        let parent = fx.directory();
        let mut table = TableView::default();
        // Start inside `sub`, as though the user had just descended into it.
        table.set_directory(
            PathInfo::try_from(child.as_path().parent().unwrap()).unwrap(),
            std::slice::from_ref(&child),
            Reselect::Top,
        );

        table.handle_command(&Command::NavigatedDirectory {
            directory: parent.clone(),
            generation: 1,
        });
        // `aaa` is a directory that sorts above `sub`, so it is what the
        // fallback would select. A listing whose first row is `sub` cannot
        // tell the rule being pinned from the fallback.
        fx.nested("aaa", "inside");
        table.handle_command(&Command::ListingBatch {
            items: vec![
                PathInfo::try_from(fx.directory().as_path().join("aaa").as_path()).unwrap(),
                fx.file("z", 1),
                PathInfo::try_from(fx.directory().as_path().join("sub").as_path()).unwrap(),
            ],
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        // Going up puts the cursor on the directory just left, not on the top
        // of the parent listing, so a second press of the same key walks back
        // out along the path the user came in on.
        assert_eq!(Some("sub".to_string()), selected_basename(&table));
    }

    /// Arrival order `c`, `a`, `b` sorts to `a`, `b`, `c` on completion, so a
    /// cursor or mark held by position would land on another entry, and the
    /// fallback would select `a`.
    #[test]
    fn the_cursor_and_marks_set_while_a_listing_loads_survive_its_completion() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let mut table = TableView::default();

        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("c", 1), fx.file("a", 1), fx.file("b", 1)],
            generation: 1,
        });
        press(&mut table, 'j');
        press(&mut table, 'v');
        press(&mut table, 'j');
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert_eq!(Some("b".to_string()), selected_basename(&table));
        assert_eq!(
            vec!["a"],
            super::super::display_names(&table.marked_paths())
        );
    }

    /// Range mode started while a navigation streams in keeps extending once
    /// the listing is sorted, from the entry it was anchored on.
    #[test]
    fn range_mode_started_while_a_listing_loads_survives_its_completion() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let mut table = TableView::default();

        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        table.handle_command(&Command::ListingBatch {
            items: ["d", "a", "b", "c"].map(|name| fx.file(name, 1)).to_vec(),
            generation: 1,
        });
        press(&mut table, 'j');
        press(&mut table, 'V');
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });
        press(&mut table, 'j');

        assert!(table.marks.in_range_mode());
        assert_eq!(vec!["a", "b"], marked_names(&table));
    }

    /// `V` on the row the first batch put the cursor on, which is not where the
    /// sort puts that entry.
    #[test]
    fn range_mode_started_without_moving_during_a_load_keeps_the_cursor_on_its_anchor() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let mut table = TableView::default();

        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        table.handle_command(&Command::ListingBatch {
            items: ["d", "a", "b", "c"].map(|name| fx.file(name, 1)).to_vec(),
            generation: 1,
        });
        press(&mut table, 'V');
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });
        press(&mut table, 'k');

        assert_eq!(vec!["c", "d"], marked_names(&table));
    }

    #[test]
    fn a_cursor_moved_while_the_parent_loads_is_not_sent_back_to_the_child() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let child = fx.nested("sub", "inside");
        let mut table = TableView::default();
        table.set_directory(
            PathInfo::try_from(child.as_path().parent().unwrap()).unwrap(),
            std::slice::from_ref(&child),
            Reselect::Top,
        );

        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        fx.nested("aaa", "inside");
        table.handle_command(&Command::ListingBatch {
            items: vec![
                PathInfo::try_from(fx.directory().as_path().join("aaa").as_path()).unwrap(),
                fx.file("z", 1),
                PathInfo::try_from(fx.directory().as_path().join("sub").as_path()).unwrap(),
            ],
            generation: 1,
        });
        press(&mut table, 'j');
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert_eq!(Some("z".to_string()), selected_basename(&table));
    }

    /// Moving away and back to the top row is still a choice: the cursor stays
    /// on `aaa` rather than returning to `sub`, the child it came from.
    #[test]
    fn a_cursor_moved_back_to_the_top_while_the_parent_loads_stays_there() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let child = fx.nested("sub", "inside");
        let mut table = TableView::default();
        table.set_directory(
            PathInfo::try_from(child.as_path().parent().unwrap()).unwrap(),
            std::slice::from_ref(&child),
            Reselect::Top,
        );

        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        fx.nested("aaa", "inside");
        table.handle_command(&Command::ListingBatch {
            items: vec![
                PathInfo::try_from(fx.directory().as_path().join("aaa").as_path()).unwrap(),
                fx.file("z", 1),
                PathInfo::try_from(fx.directory().as_path().join("sub").as_path()).unwrap(),
            ],
            generation: 1,
        });
        press(&mut table, 'j');
        press(&mut table, 'k');
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert_eq!(Some("aaa".to_string()), selected_basename(&table));
    }

    #[test]
    fn jumping_to_an_ancestor_selects_the_child_on_the_path_it_came_from() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let inside = fx.nested("sub/deeper", "inside");
        let mut table = TableView::default();
        table.set_directory(
            PathInfo::try_from(inside.as_path().parent().unwrap()).unwrap(),
            std::slice::from_ref(&inside),
            Reselect::Top,
        );

        // Two levels up in one step, as the goto prompt or a breadcrumb does.
        // The directory left, `sub/deeper`, is not in this listing; `sub`, the
        // entry the path went through, is. `aaa` sorts first, so the fallback
        // would select it.
        fx.nested("aaa", "inside");
        table.set_directory(
            fx.directory(),
            &[
                PathInfo::try_from(fx.join("aaa").as_path()).unwrap(),
                PathInfo::try_from(fx.join("sub").as_path()).unwrap(),
            ],
            Reselect::Top,
        );

        assert_eq!(Some("sub".to_string()), selected_basename(&table));
    }

    #[test]
    fn a_reload_keeps_a_cursor_move_and_a_mark_made_while_it_runs() {
        let fx = TempDir::new("nav");
        let children = [fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)];
        let mut table = listed(&fx, &children);
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1)]);
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
        let fx = TempDir::new("nav");
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)]);
        table.select(1); // "b"

        // Renamed on disk rather than recreated, so the entry keeps the device
        // and inode it was selected under.
        std::fs::rename(fx.join("b"), fx.join("z_renamed")).unwrap();
        let renamed = PathInfo::try_from(&fx.join("z_renamed")).unwrap();
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
        let fx = TempDir::new("nav");
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
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);

        // A mark names an entry of the directory being left, so it means
        // nothing in the one being entered, even if a name happens to repeat.
        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        // Marks are held by index, so any kept while the new listing streams
        // in would land on whichever entries arrive first.
        assert!(!table.has_marks());
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1), fx.file("b", 1)],
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert!(!table.has_marks());
    }

    #[test]
    fn navigating_away_drops_the_filter() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1)]);
        table.handle_command(&Command::FilterChanged("a".to_string()));

        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 1,
        });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1), fx.file("b", 1)],
            generation: 1,
        });
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        // A filter typed for one directory would hide entries of the next.
        assert_eq!(vec!["a", "b"], visible_names(&table));
    }

    #[test]
    fn late_listing_completion_does_not_clobber_the_bookmarks_listing() {
        Config::init_test();
        let fx = TempDir::new("nav");
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
    fn a_refresh_while_searching_keeps_the_results_streaming_and_the_marks() {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);
        // start_search is called directly, so the two marks carry into the
        // search listing exactly as they would after marking results.
        table.content.start_search();
        table.handle_command(&Command::SearchStarted { generation: 5 });
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
        // Nor does the refresh take over the stream: the search's next batch
        // is still shown, rather than held for a directory load.
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.nested("sub", "hit")],
            generation: 5,
        });
        assert_eq!(vec!["hit"], visible_names(&table));
    }

    #[test]
    fn bookmarks_clears_the_marks_and_emits_the_mark_reset_snapshot() {
        let fx = TempDir::new("nav");
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
        let fx = TempDir::new("nav");
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

        // The marks belong to the listing still on screen and are found again
        // by the Bookmarks handler once the new one arrives. Clearing them here
        // would drop valid marks when the read fails and no Bookmarks command
        // follows.
        assert_eq!(table.marks.len(), 2);
        assert_eq!(result, Command::GetBookmarks.into());
    }

    #[test]
    fn sort_by_clears_marks_and_resets_the_mark_count_notice() {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);

        let result = table.sort_by(SortColumn::Size);

        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    #[test]
    fn a_filter_change_clears_the_marks() {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);

        // Filtering is a reorder the user asked for, and a mark is held by
        // index, so the rows those indices address are no longer the ones the
        // user chose.
        let result = table.handle_command(&Command::FilterChanged("a".to_string()));

        assert!(!table.has_marks());
        assert_mark_reset_snapshot(&result);
    }

    /// A filter the same as the one applied reorders nothing, so it must not
    /// cost the user their marks. Dismissing the filter prompt, or submitting
    /// it unedited, sends this.
    #[test_case("" ; "no filter")]
    #[test_case("a" ; "a filter")]
    fn a_filter_change_that_reorders_nothing_keeps_the_marks(filter: &str) {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a1", 1), fx.file("a2", 1), fx.file("b", 1)]);
        table.handle_command(&Command::FilterChanged(filter.to_string()));
        table.select(0);
        table.toggle_mark();
        table.select(1);
        table.toggle_mark();

        let result = table.handle_command(&Command::FilterChanged(filter.to_string()));

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(2, table.marks.len());
    }

    #[test]
    fn a_filter_being_typed_narrows_the_listing() {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);

        table.handle_command(&Command::FilterEdited("a".to_string()));

        assert_eq!(vec!["a"], visible_names(&table));
    }

    #[test]
    fn clearing_the_filter_lists_every_entry_again() {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);
        table.handle_command(&Command::FilterChanged("a".to_string()));
        assert_eq!(vec!["a"], visible_names(&table));

        table.handle_command(&Command::FilterChanged(String::new()));

        assert_eq!(vec!["a", "b", "c"], visible_names(&table));
    }

    /// Only the rows on screen are marked: a hidden entry and one the filter
    /// leaves out stay unmarked, and the range that was open is replaced.
    #[test]
    fn select_all_marks_every_shown_row_and_ends_range_mode() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[".ah", "a", "ab", "b"].map(|name| fx.file(name, 1)));
        table.toggle_show_hidden();
        table.handle_command(&Command::FilterChanged("a".to_string()));
        table.enter_range_mode();

        table.handle_key(KeyCode::Char('a'), KeyModifiers::CONTROL);

        assert_eq!(vec!["a", "ab"], marked_names(&table));
        assert!(!table.marks.in_range_mode());
    }

    #[test]
    fn a_reorder_ends_range_mode() {
        let fx = TempDir::new("nav");
        let mut table = table_with_two_marks(&fx);
        table.enter_range_mode();
        assert!(table.marks.in_range_mode());

        table.sort_by(SortColumn::Size);

        // The anchor names a position, and the positions have just changed, so
        // the next cursor move would sweep a range from somewhere else.
        assert!(!table.marks.in_range_mode());
    }

    fn marked_names(table: &TableView) -> Vec<String> {
        table
            .marked_paths()
            .into_iter()
            .map(|item| item.display_name)
            .collect()
    }

    /// Items `a` to `e`, with `e` marked and a range from `a` to `b`, so the
    /// range has earlier marks under it that are not next to it.
    fn table_in_range_mode(fx: &TempDir) -> TableView {
        let mut table = listed(fx, &["a", "b", "c", "d", "e"].map(|name| fx.file(name, 1)));
        table.select(4);
        table.toggle_mark();
        table.select(0);
        table.enter_range_mode();
        table.select(1);
        assert_eq!(vec!["a", "b", "e"], marked_names(&table));
        table
    }

    /// The snapshot is what the notices bar reads to show range mode.
    #[test]
    fn the_selection_snapshot_reports_range_mode() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &["a", "b"].map(|name| fx.file(name, 1)));
        table.select(0);
        let range_of = |result: CommandResult| match result.into_commands().as_slice() {
            [Command::SelectionChanged { range, .. }] => *range,
            other => panic!("expected a selection snapshot, got {other:?}"),
        };

        assert!(range_of(table.enter_range_mode()));
        assert!(range_of(table.select(1)));
        assert!(!range_of(table.enter_range_mode()));
    }

    /// `a` and `d` are two names of one file, so finding the cursor by inode
    /// would put it on `a` and extend the range from `c` back over `b`.
    #[test]
    fn a_reload_in_range_mode_keeps_the_cursor_on_its_own_hard_link() {
        let fx = TempDir::new("nav");
        let a = fx.file("a", 1);
        std::fs::hard_link(&a.path, fx.join("d")).unwrap();
        let d = PathInfo::try_from(fx.join("d").as_path()).unwrap();
        let items = [a, fx.file("b", 1), fx.file("c", 1), d];
        let mut table = listed(&fx, &items);
        table.select(2);
        table.enter_range_mode();
        table.select(3);

        table.set_directory(fx.directory(), &items, Reselect::Keep);

        assert_eq!(Some("d".to_string()), selected_basename(&table));
        assert_eq!(vec!["c", "d"], marked_names(&table));
    }

    #[test]
    fn a_reload_keeps_range_mode_by_the_entries_it_names() {
        let fx = TempDir::new("nav");
        let mut table = table_in_range_mode(&fx);

        // A watcher reload with an entry sorting first, so every position
        // shifts by one.
        let items = ["0", "a", "b", "c", "d", "e"].map(|name| fx.file(name, 1));
        let result = table.set_directory(fx.directory(), &items, Reselect::Keep);
        assert!(table.marks.in_range_mode());
        // The notices bar reads range mode from this snapshot.
        assert!(
            matches!(
                result.into_commands().as_slice(),
                [.., Command::SelectionChanged { range: true, .. }]
            ),
            "the reload's snapshot must report range mode"
        );
        press(&mut table, 'j');

        // By position, the range would run from "0" and the earlier mark would
        // have moved onto "d".
        assert_eq!(Some("c".to_string()), selected_basename(&table));
        assert_eq!(vec!["a", "b", "c", "e"], marked_names(&table));
    }

    /// The range covers what it covered before the reload until the cursor
    /// moves, so an entry that appears inside it is not marked unseen.
    #[test]
    fn a_reload_does_not_mark_an_entry_that_appears_inside_the_range() {
        let fx = TempDir::new("nav");
        let mut table = table_in_range_mode(&fx);

        let items = ["a", "ab", "b", "c", "d", "e"].map(|name| fx.file(name, 1));
        table.set_directory(fx.directory(), &items, Reselect::Keep);

        assert!(table.marks.in_range_mode());
        assert_eq!(vec!["a", "b", "e"], marked_names(&table));
    }

    #[test]
    fn a_reload_that_removes_the_range_anchor_ends_range_mode() {
        let fx = TempDir::new("nav");
        let mut table = table_in_range_mode(&fx);

        let items = ["b", "c", "d", "e"].map(|name| fx.file(name, 1));
        table.set_directory(fx.directory(), &items, Reselect::Keep);

        // Nothing is left to measure the range from, so the marks it made that
        // survive stay as ordinary marks.
        assert!(!table.marks.in_range_mode());
        assert_eq!(vec!["b", "e"], marked_names(&table));
    }

    #[test]
    fn a_bookmarks_reload_keeps_the_cursor_and_the_marks() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("x", 1)]);
        let bookmarks = vec![fx.file("a", 1), fx.file("b", 1), fx.file("c", 1)];
        table.handle_command(&Command::Bookmarks {
            bookmarks: bookmarks.clone(),
        });
        table.select(0);
        table.toggle_mark();
        table.select(2);

        // A watcher refresh while the bookmarks are shown reloads them, here
        // with an entry sorting first so every position shifts by one.
        let result = table.handle_command(&Command::RefreshedDirectory {
            directory: fx.directory(),
            generation: 9,
        });
        assert_eq!(result, Command::GetBookmarks.into());
        let result = table.handle_command(&Command::Bookmarks {
            bookmarks: [vec![fx.file("0", 1)], bookmarks].concat(),
        });

        assert_eq!(Some("c".to_string()), selected_basename(&table));
        assert_eq!(vec!["a"], marked_names(&table));
        assert_eq!(
            result,
            Command::SelectionChanged {
                selected: table.selected_path().cloned(),
                mark_count: 1,
                range: false,
            }
            .into()
        );
    }

    #[test]
    fn a_search_ending_keeps_the_marks_but_still_ends_range_mode() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[]);
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
        let fx = TempDir::new("nav");
        // Sorted (a leading dot is ignored when comparing names): a, .b
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file(".b", 1)]);
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
                range: false,
            }
            .into()
        );
    }

    #[test]
    fn toggle_show_hidden_is_a_noop_during_a_search() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1), fx.file(".b", 1)]);
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
        let fx = TempDir::new("nav");
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

    /// A search that ended with three results in walk order: `cab`, `bat`,
    /// `art`. The first batch put the cursor on `cab`.
    fn streamed_search(fx: &TempDir) -> TableView {
        let mut table = listed(fx, &[]);
        table.handle_command(&Command::StartSearch("a".into()));
        table.handle_command(&Command::SearchStarted { generation: 1 });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("cab", 1), fx.file("bat", 1), fx.file("art", 1)],
            generation: 1,
        });
        table
    }

    fn selected_name(table: &TableView) -> Option<&str> {
        table.selected_path().map(|item| item.display_name.as_str())
    }

    #[test]
    fn a_finished_search_puts_a_cursor_the_user_never_moved_on_the_top_row() {
        let fx = TempDir::new("nav");
        let mut table = streamed_search(&fx);
        assert_eq!(Some("cab"), selected_name(&table));

        table.handle_command(&Command::ExitedSearch { generation: 1 });

        // Following `cab` through the sort would leave the cursor on the last
        // row, a place the user never put it.
        assert_eq!(Some(0), table.table_state.selected());
        assert_eq!(Some("art"), selected_name(&table));
    }

    #[test]
    fn a_finished_search_keeps_a_cursor_the_user_moved() {
        let fx = TempDir::new("nav");
        let mut table = streamed_search(&fx);
        table.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(Some("bat"), selected_name(&table));

        table.handle_command(&Command::ExitedSearch { generation: 1 });

        assert_eq!(Some("bat"), selected_name(&table));
    }

    /// The flag belongs to one search: a move during the last one says
    /// nothing about where the user wants the cursor in this one.
    #[test]
    fn a_new_search_forgets_that_the_last_one_moved_the_cursor() {
        let fx = TempDir::new("nav");
        let mut table = streamed_search(&fx);
        table.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        table.handle_command(&Command::ExitedSearch { generation: 1 });

        table.handle_command(&Command::StartSearch("a".into()));
        table.handle_command(&Command::SearchStarted { generation: 2 });
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("cab", 1), fx.file("art", 1)],
            generation: 2,
        });
        table.handle_command(&Command::ExitedSearch { generation: 2 });

        assert_eq!(Some("art"), selected_name(&table));
    }

    #[test]
    fn search_results_are_sorted_by_the_name_the_column_shows() {
        let fx = TempDir::new("nav");
        // `start_search` takes the search root from the current directory, and
        // the root is what the column renders names relative to.
        let mut table = listed(&fx, &[]);
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[]);
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[]);
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[]);
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
        // Marked with the key, as the user would, which also chooses the row
        // the first result put the cursor on.
        table.handle_key(KeyCode::Char('v'), KeyModifiers::NONE);

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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[]);
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
        let fx = TempDir::new("nav");
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
        let fx = TempDir::new("nav");
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
    fn a_stale_completion_does_not_end_the_current_load() {
        Config::init_test();
        let fx = TempDir::new("nav");
        let mut table = TableView::default();
        table.handle_command(&Command::NavigatedDirectory {
            directory: fx.directory(),
            generation: 2,
        });

        // The load this one superseded finishes late.
        table.handle_command(&Command::DirectoryListingComplete { generation: 1 });

        assert!(table.content.is_loading());
        table.handle_command(&Command::ListingBatch {
            items: vec![fx.file("a", 1)],
            generation: 2,
        });
        assert_eq!(table.content.len(), 1);
    }

    #[test]
    fn late_search_batches_are_dropped_in_bookmarks_mode() {
        Config::init_test();
        let fx = TempDir::new("nav");
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
    fn showing_bookmarks_clears_an_active_search() {
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1)]);
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
        let fx = TempDir::new("nav");
        let mut table = listed(&fx, &[fx.file("a", 1)]);
        table.handle_command(&Command::Bookmarks { bookmarks: vec![] });
        assert!(table.content.is_showing_bookmarks());

        table.handle_command(&Command::StartSearch("a".into()));
        assert!(!table.content.is_showing_bookmarks());
        assert!(table.content.is_searching());
    }
}
