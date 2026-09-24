use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{TableView, columns::SortColumn, navigation::Reselect, style::ClipboardHighlight};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, ForegroundProgram, handler::CommandHandler, result::CommandResult},
    file_system::path_info::PathInfo,
    views::{ListingMode, contains},
};

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        // Mode membership comes from the shared transition; the arms below
        // handle per-mode data and side effects. `previous_mode` preserves
        // the pre-transition mode for arms that dispatch on it.
        let previous_mode = self.content.mode();
        if let Some(mode) = ListingMode::transition(command) {
            self.content.set_mode(mode);
        }
        match command {
            Command::Copy { .. }
            | Command::Move { .. }
            | Command::Chmod { .. }
            | Command::Delete(_) => {
                // The operation consumes the marks; the FileSystem handler clears
                // the clipboard for these same commands. Reset the mark-count
                // notice here so it doesn't reappear once the clipboard is gone.
                self.clear_marks_notifying()
            }
            Command::CancelPrompt => {
                self.pending_delete.clear();
                CommandResult::NotHandled
            }
            Command::ConfirmDelete => {
                let paths = self.pending_delete.take();
                if paths.is_empty() {
                    CommandResult::Handled
                } else {
                    Command::Delete(paths).into()
                }
            }
            Command::SetClipboardEntry(entry) => {
                self.clipboard = entry.as_ref().map(ClipboardHighlight::from);
                CommandResult::NotHandled
            }
            Command::NavigatedDirectory {
                directory,
                generation,
            } => {
                // Different directory: nothing from the old listing carries over.
                self.content.clear_filter();
                self.stream_generation = *generation;
                self.begin_directory(directory.clone(), Reselect::Top);
                CommandResult::Handled
            }
            Command::RefreshedDirectory {
                directory,
                generation,
            } => self.refreshed_directory(directory, *generation),
            Command::ListingBatch { items, generation } => self.listing_batch(items, *generation),
            Command::DirectoryListingComplete { generation } => {
                // A cancelled load that had already drained the directory still
                // reports completion, and the bookmarks view does not bump the
                // generation, so require an in-flight load as the ListingBatch
                // guard does. Without it a late completion re-sorts the
                // bookmarks and moves the cursor off the selected one.
                if *generation != self.stream_generation || !self.content.is_loading() {
                    return CommandResult::Handled;
                }
                self.finish_directory()
            }
            Command::ResetView => self.reset_view(previous_mode),
            Command::StartSearch(_) => {
                self.content.start_search();
                self.table_state.select(None);
                self.search_cursor_chosen = false;
                self.clear_marks_notifying()
            }
            Command::SearchStarted { generation } => {
                self.stream_generation = *generation;
                CommandResult::Handled
            }
            Command::ExitedSearch { generation } => self.exited_search(*generation),
            Command::Bookmarks { bookmarks } => {
                // Entering the view: nothing marked or selected in the previous
                // listing names a bookmark. A reload (after a refresh or a
                // finished task) keeps both, found again by path.
                if previous_mode != ListingMode::Bookmarks {
                    self.clear_marks();
                    self.table_state.select(None);
                }
                self.content.set_bookmarks(bookmarks.clone());
                // Must keep terminating in `sort`/`select`: its snapshot is the
                // only one that reports the mark count after entering the view
                // (zero) or a reload (the marks still found).
                self.sort_keeping_marks()
            }
            // A bookmark delete runs as an async task; reload the list once it
            // finishes so the deleted entry disappears.
            Command::Progress(task) => {
                if self.content.is_showing_bookmarks() && task.is_terminal() {
                    Command::GetBookmarks.into()
                } else {
                    CommandResult::NotHandled
                }
            }
            // self.handle_key() and PromptView may emit FilterChanged()
            Command::FilterChanged(filter) | Command::FilterEdited(filter) => {
                self.set_filter(filter.clone())
            }

            // GetBookmarks included: FileSystem resolves it into Bookmarks.
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let (before, marks_before) = (self.table_state.selected(), self.marks.len());
        let result = self.dispatch_key(code, modifiers);
        self.note_cursor_move(before, marks_before);
        result
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> CommandResult {
        let (before, marks_before) = (self.table_state.selected(), self.marks.len());
        let result = self.dispatch_mouse(event);
        self.note_cursor_move(before, marks_before);
        result
    }

    fn should_handle_mouse(&self, event: MouseEvent) -> bool {
        let is_scroll = matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
        is_scroll
            // While dragging, Drag/Up events outside the table must still be
            // routed here so the drag tracks and its state is released.
            || self.scrollbar_view.is_dragging()
            || contains(self.table_area, event)
            || self.scrollbar_view.is_clicked(event)
    }
}

// The bodies of the longest `handle_command` arms. They live here rather than
// inline so the match stays a dispatch table that can be read in one screen.
impl TableView {
    fn dispatch_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let action = Config::global().keybindings.normal_action(code, modifiers);

        match action {
            // Clipboard
            Some(Action::Copy) => self.copy_to_clipboard(),
            Some(Action::Cut) => self.cut_to_clipboard(),
            Some(Action::Paste) => self.paste_from_clipboard(),
            // Navigation (page)
            Some(Action::PageUp) => self.previous_page(),
            Some(Action::PageDown) => self.next_page(),
            // Navigation (filesystem)
            Some(Action::Refresh) => Command::RefreshDirectory.into(),
            Some(Action::GoToParentDirectory) => Command::GoToParentDirectory.into(),
            Some(Action::GoToPreviousDirectory) => Command::GoToPreviousDirectory.into(),
            Some(Action::Open) => self.open_selected(),
            Some(Action::OpenCurrentDirectory) => Command::OpenCurrentDirectory.into(),
            Some(Action::OpenNewWindow) => Command::OpenNewWindow.into(),
            Some(Action::OpenWith) => self.open_with(),
            Some(Action::Edit) => self.run_in_foreground(ForegroundProgram::Editor),
            Some(Action::Page) => self.run_in_foreground(ForegroundProgram::Pager),
            Some(Action::GoHome) => Self::navigate_to_home_directory(),
            Some(Action::Goto) => self.open_goto_prompt(),
            // Selection
            Some(Action::SelectNext) => self.select_next(),
            Some(Action::SelectPrevious) => self.select_previous(),
            Some(Action::SelectFirst) => self.select_first(),
            Some(Action::SelectLast) => self.select_last(),
            Some(Action::SelectMiddle) => self.select_middle_item(),
            Some(Action::SelectFirstVisible) => self.select_first_visible_item(),
            Some(Action::SelectMiddleVisible) => self.select_middle_visible_item(),
            Some(Action::SelectLastVisible) => self.select_last_visible_item(),
            // Marks
            Some(Action::ToggleMark) => self.toggle_mark(),
            Some(Action::RangeMark) => self.enter_range_mode(),
            Some(Action::SelectAll) => self.mark_all(),
            // File operations
            Some(Action::AddBookmark) => self.open_add_bookmark_prompt(),
            Some(Action::GetBookmarks) => Self::get_bookmarks(),
            Some(Action::Chmod) => self.open_chmod_prompt(),
            Some(Action::CreateDirectory) => self.open_create_directory_prompt(),
            Some(Action::Delete) => self.delete(),
            Some(Action::Rename) => self.open_rename_prompt(),
            Some(Action::Filter) => self.open_filter_prompt(),
            Some(Action::Search) => Self::open_search_prompt(),
            // Sort
            Some(Action::SortByName) => self.sort_by(SortColumn::Name),
            Some(Action::SortByModified) => self.sort_by(SortColumn::Modified),
            Some(Action::SortBySize) => self.sort_by(SortColumn::Size),
            Some(Action::ToggleShowHidden) => self.toggle_show_hidden(),
            _ => CommandResult::NotHandled,
        }
    }

    fn dispatch_mouse(&mut self, event: MouseEvent) -> CommandResult {
        let x = event.column.saturating_sub(self.table_area.x);
        let y = event.row.saturating_sub(self.table_area.y);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check for scrollbar click first
                if self.scrollbar_view.is_clicked(event) {
                    return self.handle_scroll(event);
                }
                // A press while a drag is still recorded means its release
                // went to a view that took it (help, a prompt). A drag routes
                // every mouse event here, so end it first, and leave a press
                // outside the table to the view it landed on: it would
                // otherwise saturate to the header row and sort.
                if self.scrollbar_view.is_dragging() {
                    self.scrollbar_view.end_drag();
                    self.drag_line = None;
                    if !contains(self.table_area, event) {
                        return CommandResult::Handled;
                    }
                }

                // Then handle table clicks
                if y == 0 {
                    return self.click_header(x);
                }
                self.click_table(y)
            }
            MouseEventKind::Up(MouseButton::Left) => self.handle_scroll(event),
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.scrollbar_view.is_dragging() {
                    return self.handle_scroll(event);
                }
                CommandResult::Handled
            }
            MouseEventKind::ScrollUp => self.select_previous(),
            MouseEventKind::ScrollDown => self.select_next(),
            _ => CommandResult::Handled,
        }
    }

    fn refreshed_directory(&mut self, directory: &PathInfo, generation: u64) -> CommandResult {
        // While searching, the listing holds results from a different
        // root, not this directory. Ignore watcher/refresh events so a
        // background file change doesn't clobber the search results.
        if self.content.is_searching() {
            return CommandResult::Handled;
        }
        // The listing is the bookmarks dir, not this directory, and
        // renaming a bookmark triggers a CWD refresh; reload the
        // bookmarks instead of showing the CWD.
        //
        // The marks are left for the Bookmarks handler to find again in
        // the new listing. Clearing here would drop still valid marks
        // whenever the reload never arrives: a failed bookmarks read
        // broadcasts an alert and no Bookmarks command, leaving the
        // current listing on screen.
        if self.content.is_showing_bookmarks() {
            return Command::GetBookmarks.into();
        }
        // Same directory reloaded: keep the filter, marks and
        // selection, and let begin_directory/finish_directory restore
        // the last two when the stream completes. Nothing is announced
        // here, since the count does not change and the post-load
        // snapshot carries whatever the reload could not find again.
        // Only a search or the bookmarks view cancels a load before
        // then, and each announces its own clearing.
        self.stream_generation = generation;
        self.begin_directory(directory.clone(), Reselect::Keep);
        CommandResult::Handled
    }

    fn listing_batch(&mut self, items: &[PathInfo], generation: u64) -> CommandResult {
        // Only an in-flight load or search accepts batches; stale
        // generations (superseded streams) are ignored.
        if generation != self.stream_generation
            || !(self.content.is_loading() || self.content.is_searching())
            || items.is_empty()
        {
            return CommandResult::Handled;
        }
        let was_empty = self.content.len() == 0;
        self.content.append(items);
        // The batch may filter down to nothing; select only once an
        // item survives the filter.
        if was_empty && self.content.len() > 0 {
            self.select(0)
        } else {
            CommandResult::Handled
        }
    }

    fn exited_search(&mut self, generation: u64) -> CommandResult {
        // Results append in walk order so partial ones show up at once,
        // but the header advertises a sort column throughout, so apply
        // it when the walk ends. `DirectoryListingComplete` does this
        // for a listing; a search never reaches it, because `set_mode`
        // clears the loading flag its guard requires.
        //
        // A cancelled search arrives here too, since `run_search`
        // announces its exit either way and cancelling keeps the partial
        // results in search mode. Sorting matters more for those:
        // nothing further is coming, so walk order would leave the
        // header describing an order the listing never takes.
        //
        // A superseded search exits with its own generation, which no
        // longer matches, so it cannot reorder its replacement.
        if generation != self.stream_generation || !self.content.is_searching() {
            return CommandResult::Handled;
        }
        // Marks carry across: results stream so they can be marked
        // while the walk is still running, and the walk finishing is
        // not a reorder the user asked for.
        let result = self.sort_keeping_marks();
        // The cursor went to the first result in walk order, which the sort
        // moves anywhere. Unless the user chose that row, it goes to the top.
        if self.search_cursor_chosen {
            result
        } else {
            self.select(0)
        }
    }

    fn reset_view(&mut self, previous_mode: ListingMode) -> CommandResult {
        self.clipboard = None;
        self.clear_marks();
        let had_filter = !self.content.filter().is_empty();
        self.content.clear_filter();
        match previous_mode {
            // The search/bookmarks index is meaningless in the directory.
            ListingMode::Search | ListingMode::Bookmarks => {
                self.table_state.select(None);
                Command::RefreshDirectory.into()
            }
            ListingMode::Normal if had_filter => self.sort(),
            ListingMode::Normal => CommandResult::Handled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{display_names, marked_table};
    use super::*;
    use crate::{
        app::clipboard::ClipboardEntry,
        command::progress::{ActiveTask, Task, TaskKind},
    };

    /// The delete prompt is a confirmation: `delete` stashes what it resolved,
    /// and only the answer decides whether that stash is acted on. The stash
    /// outlives the prompt, so what clears it is a safety property rather than
    /// bookkeeping.
    #[test]
    fn confirming_a_delete_acts_on_what_the_prompt_asked_about() {
        let (_dir, mut table) = marked_table();
        table.delete();

        let result = table.handle_command(&Command::ConfirmDelete);

        let Ok(Command::Delete(paths)) = Command::try_from(result) else {
            panic!("expected a Delete");
        };
        assert_eq!(vec!["a", "b"], display_names(&paths));
    }

    #[test]
    fn dismissing_the_prompt_drops_what_the_delete_had_resolved() {
        let (_dir, mut table) = marked_table();
        table.delete();

        table.handle_command(&Command::CancelPrompt);

        // Every prompt broadcasts CancelPrompt on Esc, and any later
        // ConfirmDelete would otherwise delete entries the user had already
        // declined to.
        assert_eq!(
            CommandResult::Handled,
            table.handle_command(&Command::ConfirmDelete)
        );
    }

    #[test]
    fn a_confirmation_is_spent_once() {
        let (_dir, mut table) = marked_table();
        table.delete();
        table.handle_command(&Command::ConfirmDelete);

        // The stash is taken, not copied, so a second confirmation cannot
        // re-run the delete against paths that are already gone.
        assert_eq!(
            CommandResult::Handled,
            table.handle_command(&Command::ConfirmDelete)
        );
    }

    #[test]
    fn resetting_a_filtered_listing_clears_the_filter_and_re_sorts() {
        let (_dir, mut table) = marked_table();
        table.handle_command(&Command::FilterChanged("a".to_string()));
        assert_eq!(1, table.content.len());

        let result = table.handle_command(&Command::ResetView);

        assert!(table.content.filter().is_empty());
        assert_eq!(3, table.content.len());
        // The listing changed, so the selection has to be reported again.
        assert!(
            matches!(result, CommandResult::HandledWith(ref command)
                if matches!(**command, Command::SelectionChanged { .. })),
            "expected a selection snapshot, got {result:?}"
        );
    }

    // Linux only: APFS refuses a name that is not valid UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_name_that_is_not_utf8_is_filtered_by_the_text_its_row_shows() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        use crate::{app::config::Config, test_support::TempDir};

        Config::init_test();
        let dir = TempDir::new("table_filter_not_utf8");
        let items: Vec<PathInfo> = [OsStr::from_bytes(b"caf\xe9.txt"), OsStr::new("cafe.txt")]
            .iter()
            .map(|name| {
                let path = dir.join(name);
                std::fs::write(&path, b"").unwrap();
                PathInfo::try_from(path.as_path()).unwrap()
            })
            .collect();
        let mut table = TableView::default();
        table.begin_directory(
            PathInfo::try_from(dir.path()).unwrap(),
            super::super::navigation::Reselect::Top,
        );
        table.content.append(&items);
        table.finish_directory();

        // The text the row shows, spelled in mixed case, as a search matches it.
        table.handle_command(&Command::FilterChanged("CAF\\XE9".to_string()));

        assert_eq!(1, table.content.len());
        assert_eq!("caf\\xe9.txt", table.content.get(0).unwrap().display_name);
    }

    #[test]
    fn resetting_drops_the_marks_and_the_clipboard_highlight() {
        let (_dir, mut table) = marked_table();
        table.handle_command(&Command::SetClipboardEntry(Some(ClipboardEntry::Copy(
            table.marked_paths(),
        ))));
        assert!(table.clipboard.is_some());

        table.handle_command(&Command::ResetView);

        // Esc empties the clipboard, so a highlight left behind would mark
        // rows as copied that a paste no longer touches.
        assert!(!table.has_marks());
        assert!(table.clipboard.is_none());
    }

    #[test]
    fn resetting_an_unfiltered_listing_reorders_nothing() {
        let (_dir, mut table) = marked_table();

        // Esc with nothing to undo: re-sorting would move the cursor to the
        // top of a listing the user never filtered.
        let result = table.handle_command(&Command::ResetView);

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(Some(2), table.table_state.selected());
    }

    /// A non-terminal update and the terminal one for the same task, taken
    /// from the channel an `ActiveTask` reports on rather than built by hand.
    fn task_updates() -> (Task, Task) {
        let (tx, rx) = std::sync::mpsc::channel();
        let (active, running, _token) = ActiveTask::new(
            tx,
            TaskKind::Delete {
                path: String::new(),
            },
            1,
        );
        active.done();
        let Ok(Command::Progress(finished)) = rx.recv() else {
            panic!("the task should have reported")
        };
        (running, finished)
    }

    #[test]
    fn a_finished_task_reloads_only_the_bookmarks_listing() {
        let (_dir, mut table) = marked_table();
        let (_running, finished) = task_updates();

        // A directory listing has a watcher to refresh it; the bookmarks view
        // has none, so deleting a bookmark is only reflected by re-reading it.
        assert_eq!(
            CommandResult::NotHandled,
            table.handle_command(&Command::Progress(finished.clone()))
        );

        table.handle_command(&Command::Bookmarks {
            bookmarks: Vec::new(),
        });
        assert_eq!(
            CommandResult::from(Command::GetBookmarks),
            table.handle_command(&Command::Progress(finished))
        );
    }

    #[test]
    fn a_running_task_does_not_reload_the_bookmarks() {
        let (_dir, mut table) = marked_table();
        table.handle_command(&Command::Bookmarks {
            bookmarks: Vec::new(),
        });
        let (running, _finished) = task_updates();

        // Reloading on every progress update would re-read the directory
        // repeatedly for the length of the operation.
        assert!(!running.is_terminal());
        assert_eq!(
            CommandResult::NotHandled,
            table.handle_command(&Command::Progress(running))
        );
    }
}
