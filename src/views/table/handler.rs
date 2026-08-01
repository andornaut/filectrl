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
                // While showing bookmarks the listing is the bookmarks dir, not
                // this directory. A rename of a bookmark triggers a CWD refresh;
                // reload the bookmarks list instead of showing the CWD.
                //
                // Leave the marks for the Bookmarks handler to clear against
                // the new listing. Clearing them here would drop marks that are
                // still valid whenever the reload does not arrive: a failed
                // bookmarks read broadcasts an alert and no Bookmarks command,
                // leaving the current listing on screen.
                if self.content.is_showing_bookmarks() {
                    return Command::GetBookmarks.into();
                }
                // Same directory reloaded: keep the filter, and let
                // begin_directory restore the selection once the stream
                // completes.
                self.stream_generation = *generation;
                let had_marks = self.has_marks();
                // Captured before begin_directory drops the selection.
                let selected = self.selected_path().cloned();
                self.begin_directory(directory.clone(), Reselect::Keep);
                // The reload invalidates the index-based marks, so the notice
                // must be reset in this same broadcast: the post-load snapshot
                // never arrives if the load is cancelled first (by a search or
                // the bookmarks view). The mark count is the only field that
                // changes; `selection_snapshot` cannot be used here because it
                // would read the selection `begin_directory` just dropped and
                // clear StatusView's panel for the length of the reload.
                if had_marks {
                    Command::SelectionChanged {
                        selected,
                        mark_count: 0,
                    }
                    .into()
                } else {
                    CommandResult::Handled
                }
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
                // A cancelled load that had already drained the directory
                // still sends its completion, and the bookmarks view does not
                // bump the generation, so match the ListingBatch guard and
                // require an in-flight load. Without this, a late completion
                // re-sorts the bookmarks listing and moves the cursor off the
                // bookmark the user selected.
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
            Command::ExitedSearch { .. } => CommandResult::Handled,
            // FileSystem resolves GetBookmarks into Bookmarks.
            Command::GetBookmarks => CommandResult::NotHandled,
            Command::Bookmarks { bookmarks } => {
                self.clear_marks();
                self.content.set_bookmarks(bookmarks.clone());
                self.table_state.select(None);
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
            // Global
            Some(Action::ResetView) => Command::ResetView.into(),
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
