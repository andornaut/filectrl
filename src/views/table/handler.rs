use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{TableView, columns::SortColumn, navigation::Reselect};
use crate::{
    app::config::{
        Config,
        keybindings::{Action, hardcoded_action},
    },
    command::{Command, handler::CommandHandler, result::CommandResult},
};

impl CommandHandler for TableView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        match command {
            Command::Copy { .. } | Command::Move { .. } => {
                self.clear_marks();
                Command::ClearClipboard.into()
            }
            Command::Chmod { .. } => {
                self.clear_marks();
                CommandResult::NotHandled
            }
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
            Command::Delete(_) => {
                self.clear_marks();
                CommandResult::NotHandled
            }
            Command::ClearClipboard => {
                self.clipboard_entry = None;
                CommandResult::NotHandled
            }

            Command::SetClipboardEntry(entry) => {
                self.clipboard_entry = Some(entry.clone());
                CommandResult::NotHandled
            }
            Command::NavigatedDirectory {
                directory,
                children,
            } => {
                // Different directory: nothing from the old listing carries over.
                self.content.clear_search();
                self.content.clear_bookmarks();
                self.content.clear_filter();
                self.clear_marks();
                self.table_state.select(None);
                self.set_directory(directory.clone(), children.to_vec(), Reselect::Top)
            }
            Command::RefreshedDirectory {
                directory,
                children,
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
                if self.content.is_showing_bookmarks() {
                    return Command::GetBookmarks.into();
                }
                // Same directory reloaded: keep filter and selection.
                self.set_directory(directory.clone(), children.to_vec(), Reselect::Keep)
            }
            Command::ResetView => {
                self.clipboard_entry = None;
                self.clear_marks();
                let had_filter = !self.content.filter().is_empty();
                self.content.clear_filter();
                if self.content.is_searching() {
                    self.content.clear_search();
                    self.table_state.select(None); // search-result index is meaningless in the directory
                    return Command::RefreshDirectory.into();
                }
                if self.content.is_showing_bookmarks() {
                    self.content.clear_bookmarks();
                    self.table_state.select(None); // bookmarks-list index is meaningless in the directory
                    return Command::RefreshDirectory.into();
                }
                if had_filter {
                    return self.sort(Reselect::Top);
                }
                CommandResult::Handled
            }
            Command::StartSearch(query) => {
                if query.is_empty() {
                    return CommandResult::Handled;
                }
                self.clear_marks();
                self.content.start_search();
                self.table_state.select(None);
                CommandResult::Handled
            }
            Command::SearchResult(path_info) => {
                if !self.content.is_searching() {
                    return CommandResult::Handled;
                }
                let is_first = self.content.len() == 0;
                self.content.append_search_result(path_info.clone());
                if is_first {
                    self.table_state.select(Some(0));
                    Command::SelectionChanged(Some(path_info.clone())).into()
                } else {
                    CommandResult::Handled
                }
            }
            Command::ExitedSearch => CommandResult::Handled,
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
        let action = hardcoded_action(code, modifiers)
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
            || self.table_area.contains(Position {
                x: event.column,
                y: event.row,
            })
            || self.scrollbar_view.is_clicked(event.column, event.row)
    }
}
