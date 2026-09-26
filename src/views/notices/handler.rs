use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{NoticesView, SearchState, notice::Notice};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, PromptAction, handler::CommandHandler, result::CommandResult},
    views::{ListingMode, contains},
};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        // Every listing-mode transition command has an arm below, so the rebuild still runs.
        if let Some(mode) = ListingMode::transition(command)
            && mode != ListingMode::Search
        {
            self.clear_search_notice();
        }
        let result = match command {
            Command::CancelPrompt | Command::ConfirmDelete => {
                self.hide_marked = false;
                CommandResult::NotHandled
            }
            Command::OpenPrompt(PromptAction::Delete(_)) => {
                self.hide_marked = true;
                CommandResult::NotHandled
            }
            Command::NavigatedDirectory { .. } => {
                self.filter.clear();
                self.mark_count = 0;
                CommandResult::Handled
            }
            Command::StartSearch(query) => {
                self.search_query = Some(query.clone());
                self.search_started_at = Some(Instant::now());
                self.search_state = SearchState::Running;
                // Search results are unfiltered.
                self.filter.clear();
                CommandResult::NotHandled
            }
            Command::CancelSearch => {
                // Keep the search notice, relabelled "[Search cancelled]".
                self.search_state = SearchState::Cancelled;
                self.search_started_at = None;
                CommandResult::Handled
            }
            Command::SearchStarted { generation } => {
                self.search_generation = *generation;
                CommandResult::Handled
            }
            Command::ExitedSearch { generation } => {
                // Exits from superseded searches are ignored. The notice stays until the listing
                // changes.
                if *generation == self.search_generation
                    && self.search_state == SearchState::Running
                {
                    self.search_state = SearchState::Finished;
                    self.search_started_at = None;
                }
                CommandResult::Handled
            }
            // The indicator position derives from elapsed time; a tick only wakes the event loop.
            Command::SearchTick => return CommandResult::Handled,
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::ResetView => {
                self.clipboard_entry = None;
                self.filter.clear();
                self.mark_count = 0;
                CommandResult::Handled
            }
            Command::SetClipboardEntry(entry) => {
                self.clipboard_entry.clone_from(entry);
                CommandResult::NotHandled
            }
            Command::FilterChanged(filter) | Command::FilterEdited(filter) => {
                self.filter.clone_from(filter);
                CommandResult::NotHandled
            }
            // The transition hook above already cleared the search notice.
            Command::Bookmarks { .. } => {
                // The bookmarks listing is unfiltered.
                self.filter.clear();
                CommandResult::NotHandled
            }
            Command::SelectionChanged {
                mark_count, range, ..
            } => {
                // Most cursor moves carry an unchanged mark count.
                if *mark_count == self.mark_count && *range == self.range {
                    return CommandResult::Handled;
                }
                let marked_more = *mark_count > self.mark_count;
                self.mark_count = *mark_count;
                self.range = *range;
                // Marking more clears the clipboard. Only a growing count counts: copying marked
                // files keeps the marks.
                if marked_more && self.clipboard_entry.is_some() {
                    Command::SetClipboardEntry(None).into()
                } else {
                    CommandResult::Handled
                }
            }
            _ => return CommandResult::NotHandled,
        };
        self.rebuild_notices();
        result
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::ClearProgress) => {
                let result = self.clear_progress();
                self.rebuild_notices();
                result
            }
            _ => CommandResult::NotHandled,
        }
    }

    fn handle_mouse(&mut self, event: MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let y = event.row.saturating_sub(self.area.y) as usize;
                match self.notices.get(y) {
                    Some(
                        Notice::Clipboard(_)
                        | Notice::Filter(_)
                        | Notice::Marked { .. }
                        | Notice::Search(_)
                        | Notice::SearchCancelled(_)
                        | Notice::SearchFinished { .. }
                        | Notice::SearchLoading,
                    ) => Command::ResetView.into(),
                    _ => CommandResult::Handled,
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: MouseEvent) -> bool {
        contains(self.area, event)
    }
}
