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
        match command {
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
                CommandResult::Handled
            }
            Command::StartSearch(query) => {
                log::debug!("NoticesView: StartSearch({query:?})");
                self.search_query = Some(query.clone());
                self.search_tick = 0;
                CommandResult::NotHandled
            }
            Command::SearchComplete => {
                log::debug!("NoticesView: SearchComplete");
                self.search_query = None;
                self.search_tick = 0;
                CommandResult::Handled
            }
            Command::SearchTick => {
                if self.search_query.is_some() {
                    self.search_tick = self.search_tick.wrapping_add(1);
                    log::debug!("NoticesView: SearchTick {}", self.search_tick);
                }
                CommandResult::Handled
            }
            Command::Progress(task) => self.update_tasks(task.clone()),
            Command::ResetView => {
                self.clipboard_entry = None;
                self.filter.clear();
                self.mark_count = 0;
                self.search_query = None;
                CommandResult::Handled
            }
            Command::SetClipboard(entry) => {
                self.clipboard_entry = Some(entry.clone());
                CommandResult::NotHandled
            }
            Command::FilterChanged(filter) => {
                self.filter.clone_from(filter);
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
        }
    }

    fn handle_key(&mut self, code: &KeyCode, modifiers: &KeyModifiers) -> CommandResult {
        match Config::global().keybindings.normal_action(code, modifiers) {
            Some(Action::ClearProgress) => self.clear_progress(),
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
                    | Some(Notice::SearchLoading) => Command::ResetView.into(),
                    Some(Notice::Progress) => self.clear_progress(),
                    None => CommandResult::Handled,
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
