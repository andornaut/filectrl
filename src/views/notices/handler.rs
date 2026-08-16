use std::time::Instant;

use ratatui::{
    crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    prelude::Position,
};

use super::{NoticesView, notice::Notice};
use crate::{
    app::config::{Config, keybindings::Action},
    command::{Command, PromptAction, handler::CommandHandler, result::CommandResult},
    views::ListingMode,
};

impl CommandHandler for NoticesView {
    fn handle_command(&mut self, command: &Command) -> CommandResult {
        // Any listing-mode transition away from search clears the search
        // notice; the transition rules live in ListingMode. Every transition
        // command has an arm below, so the rebuild at the end still runs.
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
                self.search_cancelled = false;
                // Search results are unfiltered (`start_search` clears the
                // filter), so the notice has to clear with it. The table
                // rejects an empty query and keeps its filter applied, so
                // mirror that guard or the notice would vanish while the
                // listing stays silently filtered.
                if !query.is_empty() {
                    self.filter.clear();
                }
                CommandResult::NotHandled
            }
            Command::CancelSearch => {
                // Keep the search notice visible; relabel it to "Cancelled: ...".
                self.search_cancelled = true;
                self.search_started_at = None;
                CommandResult::Handled
            }
            Command::SearchStarted { generation } => {
                self.search_generation = *generation;
                CommandResult::Handled
            }
            Command::ExitedSearch { generation } => {
                // Ignore exits from superseded searches (the current search
                // is still running). For the current search: a cancelled exit
                // keeps the relabeled notice, a natural one clears it.
                if *generation == self.search_generation && !self.search_cancelled {
                    self.search_query = None;
                    self.search_started_at = None;
                }
                CommandResult::Handled
            }
            // The loading indicator's position is a function of elapsed time,
            // read live in render, so a tick changes no state at all: it exists
            // only to wake the event loop for the next frame while a search is
            // running and nothing else is arriving.
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
            Command::FilterChanged(filter) => {
                self.filter.clone_from(filter);
                CommandResult::NotHandled
            }
            // Opening bookmarks cancels any in-flight search; the transition
            // hook above clears the notice immediately (the walker's eventual
            // ExitedSearch can lag and is then a no-op).
            Command::Bookmarks { .. } => {
                // The bookmarks listing is unfiltered (`set_bookmarks` clears
                // the filter), so the notice has to clear with it.
                self.filter.clear();
                CommandResult::NotHandled
            }
            Command::SelectionChanged { mark_count, .. } => {
                // Every cursor move carries the (usually unchanged) mark
                // count; skip the rebuild and the derivation below for those.
                if *mark_count == self.mark_count {
                    return CommandResult::Handled;
                }
                self.mark_count = *mark_count;
                // Marks and clipboard are mutually exclusive. Fires only on a
                // mark-count change: a clipboard set while marks are held
                // (copying marked files) must survive plain cursor movement.
                if *mark_count > 0 && self.clipboard_entry.is_some() {
                    Command::SetClipboardEntry(None).into()
                } else {
                    CommandResult::Handled
                }
            }
            // Commands that never touch notice state must not trigger a
            // rebuild on every broadcast.
            _ => return CommandResult::NotHandled,
        };
        // Keep the cached notice list in sync with the state the matched arm
        // just mutated, so `constraint`/`render` can read it without
        // rebuilding every frame.
        self.rebuild_notices();
        result
    }

    fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> CommandResult {
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

    fn handle_mouse(&mut self, event: MouseEvent) -> CommandResult {
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let y = event.row.saturating_sub(self.area.y) as usize;
                match self.notices.get(y) {
                    Some(
                        Notice::Clipboard(_)
                        | Notice::Filter(_)
                        | Notice::Marked(_)
                        | Notice::Search(_)
                        | Notice::SearchCancelled(_)
                        | Notice::SearchLoading,
                    ) => Command::ResetView.into(),
                    _ => CommandResult::Handled,
                }
            }
            _ => CommandResult::Handled,
        }
    }

    fn should_handle_mouse(&self, event: MouseEvent) -> bool {
        self.area.contains(Position {
            x: event.column,
            y: event.row,
        })
    }
}
