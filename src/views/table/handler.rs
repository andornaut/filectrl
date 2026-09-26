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
        let previous_mode = self.content.mode();
        if let Some(mode) = ListingMode::transition(command) {
            self.content.set_mode(mode);
        }
        match command {
            Command::Copy { .. }
            | Command::Move { .. }
            | Command::Chmod { .. }
            | Command::Delete(_) => {
                // The operation consumes the marks.
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
            Command::SearchResultsRefreshed { items, generation } => {
                self.search_results_refreshed(items, *generation)
            }
            Command::DirectoryListingComplete { generation } => {
                // A cancelled load still reports completion, and the bookmarks
                // view does not bump the generation, so require an in-flight load.
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
                self.clear_marks();
                self.selection_snapshot()
            }
            Command::SearchStarted { generation } => {
                self.stream_generation = *generation;
                CommandResult::Handled
            }
            Command::ExitedSearch { generation } => self.exited_search(*generation),
            Command::Bookmarks { bookmarks } => {
                // Entering the view clears marks and the cursor; a reload keeps
                // both, found again by path.
                if previous_mode != ListingMode::Bookmarks {
                    self.clear_marks();
                    self.table_state.select(None);
                }
                self.content.set_bookmarks(bookmarks.clone());
                // Its snapshot is the only report of the mark count here.
                self.sort_keeping_marks()
            }
            // The bookmarks view has no watcher; reload once a task finishes.
            Command::Progress(task) => {
                if self.content.is_showing_bookmarks() && task.is_terminal() {
                    Command::GetBookmarks.into()
                } else {
                    CommandResult::NotHandled
                }
            }
            Command::FilterChanged(filter) | Command::FilterEdited(filter) => {
                self.set_filter(filter.clone())
            }

            _ => CommandResult::NotHandled,
        }
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let (before, marks_before) = (self.table_state.selected(), self.marks.len());
        let result = self.dispatch_key(code, modifiers);
        if !matches!(result, CommandResult::NotHandled) {
            self.wheel_scrolled = false;
        }
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
        if self.ignores_mouse {
            return false;
        }
        let is_scroll = matches!(
            event.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        );
        is_scroll
            // Drag/Up events outside the table still reach an active drag.
            || self.scrollbar_view.is_dragging()
            || contains(self.table_area, event)
            || self.scrollbar_view.is_clicked(event)
    }
}

// The bodies of the longest `handle_command` arms.
impl TableView {
    fn dispatch_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        let action = Config::global().keybindings.normal_action(code, modifiers);

        match action {
            Some(Action::Copy) => self.copy_to_clipboard(),
            Some(Action::Cut) => self.cut_to_clipboard(),
            Some(Action::Paste) => self.paste_from_clipboard(),
            Some(Action::PageUp) => self.previous_page(),
            Some(Action::PageDown) => self.next_page(),
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
            Some(Action::SelectNext) => self.select_next(),
            Some(Action::SelectPrevious) => self.select_previous(),
            Some(Action::SelectFirst) => self.select_first(),
            Some(Action::SelectLast) => self.select_last(),
            Some(Action::SelectMiddle) => self.select_middle_item(),
            Some(Action::SelectFirstVisible) => self.select_first_visible_item(),
            Some(Action::SelectMiddleVisible) => self.select_middle_visible_item(),
            Some(Action::SelectLastVisible) => self.select_last_visible_item(),
            Some(Action::ToggleMark) => self.toggle_mark(),
            Some(Action::RangeMark) => self.enter_range_mode(),
            Some(Action::SelectAll) => self.mark_all(),
            Some(Action::AddBookmark) => self.open_add_bookmark_prompt(),
            Some(Action::GetBookmarks) => Self::get_bookmarks(),
            Some(Action::Chmod) => self.open_chmod_prompt(),
            Some(Action::CreateDirectory) => self.open_create_directory_prompt(),
            Some(Action::Delete) => self.delete(),
            Some(Action::Rename) => self.open_rename_prompt(),
            Some(Action::Filter) => self.open_filter_prompt(),
            Some(Action::Search) => Self::open_search_prompt(),
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
                if self.scrollbar_view.is_clicked(event) {
                    return self.handle_scroll(event);
                }
                // A drag still recorded means its release went to another view.
                // End it, and leave a press outside the table to that view.
                if self.scrollbar_view.is_dragging() {
                    self.scrollbar_view.end_drag();
                    self.drag_line = None;
                    if !contains(self.table_area, event) {
                        return CommandResult::Handled;
                    }
                }

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
            MouseEventKind::ScrollUp => self.scroll_window_up(),
            MouseEventKind::ScrollDown => self.scroll_window_down(),
            _ => CommandResult::Handled,
        }
    }

    fn refreshed_directory(&mut self, directory: &PathInfo, generation: u64) -> CommandResult {
        // The listing holds search results, not this directory.
        if self.content.is_searching() {
            return CommandResult::Handled;
        }
        // Reload the bookmarks instead; the Bookmarks handler re-finds marks.
        if self.content.is_showing_bookmarks() {
            return Command::GetBookmarks.into();
        }
        // Same directory reloaded: keep the filter, marks and selection.
        self.stream_generation = generation;
        self.begin_directory(directory.clone(), Reselect::Keep);
        CommandResult::Handled
    }

    fn listing_batch(&mut self, items: &[PathInfo], generation: u64) -> CommandResult {
        if generation != self.stream_generation
            || !(self.content.is_loading() || self.content.is_searching())
            || items.is_empty()
        {
            return CommandResult::Handled;
        }
        let was_empty = self.content.len() == 0;
        self.content.append(items);
        if was_empty && self.content.len() > 0 {
            self.select(0)
        } else {
            CommandResult::Handled
        }
    }

    /// Replaces the ended search's results with the same ones read again,
    /// keeping the marks and the cursor found by path.
    fn search_results_refreshed(&mut self, items: &[PathInfo], generation: u64) -> CommandResult {
        if generation != self.stream_generation || !self.content.is_searching() {
            return CommandResult::Handled;
        }
        self.content.replace_search_results(items.to_vec());
        self.sort_keeping_marks()
    }

    fn exited_search(&mut self, generation: u64) -> CommandResult {
        // Results arrive in walk order; sort once the walk ends or is cancelled.
        if generation != self.stream_generation || !self.content.is_searching() {
            return CommandResult::Handled;
        }
        let result = self.sort_keeping_marks();
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
            ListingMode::Search | ListingMode::Bookmarks => {
                self.table_state.select(None);
                vec![self.selection_changed(), Command::RefreshDirectory].into()
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

        assert!(!table.has_marks());
        assert!(table.clipboard.is_none());
    }

    #[test]
    fn resetting_an_unfiltered_listing_reorders_nothing() {
        let (_dir, mut table) = marked_table();

        let result = table.handle_command(&Command::ResetView);

        assert_eq!(CommandResult::Handled, result);
        assert_eq!(Some(2), table.table_state.selected());
    }

    /// A non-terminal update and the terminal one for the same task.
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

        assert!(!running.is_terminal());
        assert_eq!(
            CommandResult::NotHandled,
            table.handle_command(&Command::Progress(running))
        );
    }
}
