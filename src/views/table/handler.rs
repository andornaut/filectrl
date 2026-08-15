use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{TableView, columns::SortColumn, navigation::Reselect};
use crate::{
    app::config::{
        Config,
        keybindings::{Action, hardcoded_normal_action},
    },
    command::{Command, handler::CommandHandler, result::CommandResult},
    views::ListingMode,
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
            Command::Copy { .. } | Command::Move { .. } => {
                // The operation consumes the marks; the FileSystem handler clears
                // the clipboard for these same commands. Reset the mark-count
                // notice here so it doesn't reappear once the clipboard is gone.
                self.clear_marks_notifying()
            }
            Command::Chmod { .. } => self.clear_marks_notifying(),
            Command::CancelPrompt => {
                self.pending_delete.clear();
                CommandResult::NotHandled
            }
            Command::ConfirmDelete => {
                let paths = std::mem::take(&mut self.pending_delete);
                if paths.is_empty() {
                    CommandResult::Handled
                } else {
                    Command::Delete(paths).into()
                }
            }
            Command::Delete(_) => self.clear_marks_notifying(),
            Command::SetClipboardEntry(entry) => {
                self.clipboard_entry = entry.clone();
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
            } => {
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
                // The marks are left for the Bookmarks handler to clear against
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
                self.stream_generation = *generation;
                self.begin_directory(directory.clone(), Reselect::Keep);
                CommandResult::Handled
            }
            Command::ListingBatch { items, generation } => {
                // Only an in-flight load or search accepts batches; stale
                // generations (superseded streams) are ignored.
                if *generation != self.stream_generation
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
            Command::ResetView => {
                self.clipboard_entry = None;
                self.clear_marks();
                let had_filter = !self.content.filter().is_empty();
                self.content.clear_filter();
                match previous_mode {
                    // The search/bookmarks index is meaningless in the directory.
                    ListingMode::Search | ListingMode::Bookmarks => {
                        self.table_state.select(None);
                        Command::RefreshDirectory.into()
                    }
                    ListingMode::Normal if had_filter => self.sort(Reselect::Top),
                    ListingMode::Normal => CommandResult::Handled,
                }
            }
            Command::StartSearch(query) => {
                if query.is_empty() {
                    return CommandResult::Handled;
                }
                self.content.start_search();
                self.table_state.select(None);
                self.clear_marks_notifying()
            }
            Command::SearchStarted { generation } => {
                // The empty-query backstop emits SearchStarted without the
                // table entering search mode; an in-flight load's generation
                // must not be clobbered then.
                if self.content.is_searching() {
                    self.stream_generation = *generation;
                }
                CommandResult::Handled
            }
            Command::ExitedSearch { generation } => {
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
                if *generation != self.stream_generation || !self.content.is_searching() {
                    return CommandResult::Handled;
                }
                // Marks carry across: results stream so they can be marked
                // while the walk is still running, and the walk finishing is
                // not a reorder the user asked for.
                self.sort_keeping_marks(Reselect::Top)
            }
            // FileSystem resolves GetBookmarks into Bookmarks.
            Command::GetBookmarks => CommandResult::NotHandled,
            Command::Bookmarks { bookmarks } => {
                self.clear_marks();
                self.content.set_bookmarks(bookmarks.clone());
                self.table_state.select(None);
                // Must keep terminating in `sort`/`select`: its snapshot is the
                // only `SelectionChanged { mark_count: 0 }` that resets the
                // mark-count notice for a bookmarks reload, because the
                // `RefreshedDirectory` branch above defers clearing to here.
                self.sort(Reselect::Top)
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
            Command::FilterChanged(filter) => self.set_filter(filter.clone()),

            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        // Hardcoded bindings take precedence, then config bindings.
        let action = hardcoded_normal_action(code, modifiers)
            .or_else(|| Config::global().keybindings.normal_action(code, modifiers));

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
            Some(Action::GoHome) => self.navigate_to_home_directory(),
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
            // File operations
            Some(Action::AddBookmark) => self.open_add_bookmark_prompt(),
            Some(Action::GetBookmarks) => self.get_bookmarks(),
            Some(Action::Chmod) => self.open_chmod_prompt(),
            Some(Action::CreateDirectory) => self.open_create_directory_prompt(),
            Some(Action::Delete) => self.delete(),
            Some(Action::Rename) => self.open_rename_prompt(),
            Some(Action::Filter) => self.open_filter_prompt(),
            Some(Action::Search) => self.open_search_prompt(),
            // Sort
            Some(Action::SortByName) => self.sort_by(SortColumn::Name),
            Some(Action::SortByModified) => self.sort_by(SortColumn::Modified),
            Some(Action::SortBySize) => self.sort_by(SortColumn::Size),
            Some(Action::ToggleShowHidden) => self.toggle_show_hidden(),
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        let x = event.column.saturating_sub(self.table_area.x);
        let y = event.row.saturating_sub(self.table_area.y);

        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Check for scrollbar click first
                if self.scrollbar_view.is_clicked(event.column, event.row) {
                    return self.handle_scroll(event);
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

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        let is_scroll = matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
        is_scroll
            // While dragging, Drag/Up events outside the table must still be
            // routed here so the drag tracks and its state is released.
            || self.scrollbar_view.is_dragging()
            || self.table_area.contains(Position {
                x: event.column,
                y: event.row,
            })
            || self.scrollbar_view.is_clicked(event.column, event.row)
    }
}
