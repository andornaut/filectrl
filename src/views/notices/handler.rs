use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{NoticesView, notice::Notice};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, PromptAction, handler::CommandHandler, result::CommandResult},
};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        let result = match command {
            Command::CancelPrompt | Command::ConfirmDelete => {
                self.hide_marked = false;
                CommandResult::NotHandled
            }
            Command::OpenPrompt(PromptAction::Delete(_)) => {
                self.hide_marked = true;
                CommandResult::NotHandled
            }
            Command::ClearClipboard => {
                self.clipboard_entry = None;
                CommandResult::NotHandled
            }
            Command::NavigatedDirectory { .. } => {
                self.filter.clear();
                self.mark_count = 0;
                self.search_query = None;
                self.search_cancelled = false;
                CommandResult::Handled
            }
            Command::StartSearch(query) => {
                self.search_query = Some(query.clone());
                self.search_tick = 0;
                self.search_cancelled = false;
                CommandResult::NotHandled
            }
            Command::CancelSearch => {
                // Keep the search notice visible; relabel it to "Cancelled: ...".
                self.search_cancelled = true;
                self.search_tick = 0;
                CommandResult::Handled
            }
            Command::ExitedSearch => {
                if self.search_cancelled {
                    // Search was cancelled via cancel_task: keep the notice.
                    self.search_tick = 0;
                    CommandResult::Handled
                } else {
                    self.search_query = None;
                    self.search_tick = 0;
                    CommandResult::Handled
                }
            }
            Command::SearchTick => {
                if self.search_query.is_some() && !self.search_cancelled {
                    self.search_tick = self.search_tick.wrapping_add(1);
                }
                CommandResult::Handled
            }
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::ResetView => {
                self.clipboard_entry = None;
                self.filter.clear();
                self.mark_count = 0;
                self.search_query = None;
                self.search_cancelled = false;
                CommandResult::Handled
            }
            Command::SetClipboardEntry(entry) => {
                self.clipboard_entry = Some(entry.clone());
                CommandResult::NotHandled
            }
            // Filtering and showing bookmarks both re-list, clearing the table's
            // marks. Their table handlers must emit SelectionChanged, so they
            // can't also emit MarkCountChanged; reset the count from this side.
            Command::FilterChanged(filter) => {
                self.filter.clone_from(filter);
                self.mark_count = 0;
                CommandResult::NotHandled
            }
            Command::Bookmarks { .. } => {
                self.mark_count = 0;
                CommandResult::NotHandled
            }
            Command::MarkCountChanged(count) => {
                self.mark_count = *count;
                // Marks and clipboard are mutually exclusive
                if *count > 0 && self.clipboard_entry.is_some() {
                    Command::ClearClipboard.into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => CommandResult::NotHandled,
        };
        // Keep the cached notice list in sync with the state just mutated, so
        // `constraint`/`render` can read it without rebuilding every frame.
        // SearchTick only advances the spinner counter (read live in render) and
        // never changes the notice list, so skip the rebuild on that hot path.
        if !matches!(command, Command::SearchTick) {
            self.rebuild_notices();
        }
        result
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::ClearProgress) => {
                // The only key this view handles, and the only one that mutates
                // notice state, so rebuild the cache here rather than on every
                // unrelated keystroke.
                let result = self.clear_progress();
                self.rebuild_notices();
                result
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: &MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let y = event.row.saturating_sub(self.area.y) as usize;
                match self.notices.get(y) {
                    Some(Notice::Clipboard(_))
                    | Some(Notice::Filter(_))
                    | Some(Notice::Marked(_))
                    | Some(Notice::Search(_))
                    | Some(Notice::SearchCancelled(_))
                    | Some(Notice::SearchLoading) => Command::ResetView.into(),
                    _ => CommandResult::Handled,
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: &MouseEvent) -> bool {
        self.area.contains(Position {
            x: event.column,
            y: event.row,
        })
    }
}
